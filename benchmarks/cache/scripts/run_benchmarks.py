#!/usr/bin/env python3
"""Probe and benchmark dlin's three persistent cache workloads."""

from __future__ import annotations

import argparse
import hashlib
import json
import platform
import shutil
import subprocess
import sys
import time
from pathlib import Path
from shlex import join as shell_join


SUITE_ROOT = Path(__file__).resolve().parents[1]
REPO_ROOT = SUITE_ROOT.parents[1]
CACHE_FILES = (
    "extraction_cache.json",
    "manifest_graph_cache.json",
    "column_lineage_cache.json",
)


def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(value, sort_keys=True, separators=(",", ":")).encode()).hexdigest()


def canonical(value: object) -> object:
    if isinstance(value, dict):
        return {key: canonical(value[key]) for key in sorted(value)}
    if isinstance(value, list):
        return [canonical(item) for item in value]
    return value


def run(command: list[str], *, label: str) -> subprocess.CompletedProcess[str]:
    result = subprocess.run(command, text=True, capture_output=True, check=False)
    if result.returncode:
        print(result.stdout, end="")
        print(result.stderr, end="", file=sys.stderr)
        raise RuntimeError(f"{label} failed with exit code {result.returncode}: {shell_join(command)}")
    return result


def json_probe(command: list[str], label: str) -> tuple[object, str]:
    result = run(command, label=label)
    try:
        value = json.loads(result.stdout)
    except json.JSONDecodeError as error:
        raise RuntimeError(f"{label} did not emit JSON: {error}") from error
    return value, digest(canonical(value))


def cache_snapshot(cache_dir: Path) -> dict[str, object]:
    result: dict[str, object] = {}
    for name in CACHE_FILES:
        path = cache_dir / name
        if not path.exists():
            continue
        stat = path.stat()
        content = path.read_bytes()
        result[name] = {
            "sha256": hashlib.sha256(content).hexdigest(),
            "mtime_ns": stat.st_mtime_ns,
            "size": len(content),
        }
    return result


def file_sizes(root: Path) -> dict[str, int]:
    return {
        path.relative_to(root).as_posix(): path.stat().st_size
        for path in sorted(root.rglob("*"))
        if path.is_file()
    }


def git_head() -> str | None:
    result = subprocess.run(
        ["git", "rev-parse", "HEAD"],
        cwd=REPO_ROOT,
        text=True,
        capture_output=True,
        check=False,
    )
    return result.stdout.strip() if result.returncode == 0 else None


def hyperfine(command: list[str], output: Path, runs: int, warmup: int, *, prepare: str | None = None) -> None:
    if shutil.which("hyperfine") is None:
        raise RuntimeError(
            "hyperfine is required for timing; install it separately "
            "(for example cargo install hyperfine)"
        )
    args = [
        "hyperfine",
        "--runs",
        str(runs),
        "--warmup",
        str(warmup),
        "--export-json",
        str(output),
    ]
    if prepare:
        args.extend(["--prepare", prepare])
    args.append(shell_join(command))
    run(args, label=f"hyperfine {output.name}")


def scenario(
    name: str,
    command: list[str],
    cache_dir: Path,
    result_dir: Path,
    runs: int,
    warmup: int,
    timing: bool,
) -> dict[str, object]:
    cache_dir.parent.mkdir(parents=True, exist_ok=True)
    if cache_dir.exists():
        shutil.rmtree(cache_dir)
    no_cache_command = [*command, "--no-cache"]
    refresh_command = [*command, "--refresh-cache"]
    no_cache_value, no_cache_hash = json_probe(no_cache_command, f"{name}/no-cache")
    no_cache_cache_state = {
        "directory_exists": cache_dir.exists(),
        "files": cache_snapshot(cache_dir) if cache_dir.exists() else {},
    }
    if no_cache_cache_state["directory_exists"]:
        raise RuntimeError(f"{name}: --no-cache created the persistent cache directory")
    cold_value, cold_hash = json_probe(refresh_command, f"{name}/persistent-cold")
    if no_cache_value != cold_value:
        raise RuntimeError(f"{name}: no-cache and persistent-cold output differ")
    ignore_path = cache_dir / ".gitignore"
    expected_ignore = "# Automatically created by dlin\n*\n"
    if not ignore_path.is_file() or ignore_path.read_text(encoding="utf-8") != expected_ignore:
        raise RuntimeError(f"{name}: dlin did not create the expected cache .gitignore")

    raw: dict[str, str] = {}
    if timing:
        for mode, timing_command in (("no-cache", no_cache_command), ("persistent-cold", refresh_command)):
            raw_path = result_dir / f"{name}.{mode}.json"
            hyperfine(timing_command, raw_path, runs, warmup)
            raw[mode] = str(raw_path)
    # The cold hyperfine runs intentionally rewrite the cache. Prepare a final
    # warm state after those runs so the warm write assertion covers only the
    # measured warm executions.
    run(refresh_command, label=f"{name}/persistent-warm preparation")
    warm_before = cache_snapshot(cache_dir)
    if not warm_before:
        raise RuntimeError(f"{name}: persistent-cold produced no observed cache file")
    warm_value, warm_hash = json_probe(command, f"{name}/persistent-warm")
    warm_after_probe = cache_snapshot(cache_dir)
    if warm_value != cold_value:
        raise RuntimeError(f"{name}: persistent-warm output differs from persistent-cold")
    if warm_before != warm_after_probe:
        raise RuntimeError(f"{name}: persistent-warm rewrote an observed cache file")
    if timing:
        raw_path = result_dir / f"{name}.persistent-warm.json"
        hyperfine(command, raw_path, runs, warmup)
        raw["persistent-warm"] = str(raw_path)
        warm_after_timing = cache_snapshot(cache_dir)
        if warm_before != warm_after_timing:
            raise RuntimeError(f"{name}: timed persistent-warm rewrote an observed cache file")
    else:
        warm_after_timing = warm_after_probe
    return {
        "name": name,
        "kind": name,
        "persistent_cold": "persistent-cold",
        "persistent_warm": "persistent-warm",
        "commands": {
            "no-cache": shell_join(no_cache_command),
            "persistent-cold": shell_join(refresh_command),
            "persistent-warm": shell_join(command),
        },
        "semantic_probe": {
            "no_cache_hash": no_cache_hash,
            "persistent_cold_hash": cold_hash,
            "persistent_warm_hash": warm_hash,
            "equivalent": True,
        },
        "no_cache_cache_state": no_cache_cache_state,
        "cache_before_warm": warm_before,
        "cache_after_warm": warm_after_timing,
        "cache_gitignore": {"path": str(ignore_path), "content": expected_ignore},
        "hyperfine_results": raw,
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workload", type=Path, default=SUITE_ROOT / "workloads/default")
    parser.add_argument("--binary", type=Path, default=REPO_ROOT / "target/release/dlin")
    parser.add_argument("--results-dir", type=Path, default=SUITE_ROOT / "results/local")
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument(
        "--skip-timing",
        action="store_true",
        help="run probes and cache validation without hyperfine",
    )
    args = parser.parse_args()
    if args.runs < 1 or args.warmup < 0:
        parser.error("--runs must be positive and --warmup must not be negative")
    workload = args.workload.resolve()
    binary = args.binary.resolve()
    results = args.results_dir.resolve()
    if not workload.is_dir() or not binary.is_file():
        parser.error("workload directory and explicit binary path must exist")
    validator = Path(__file__).with_name("validate_workload.py")
    run(
        [sys.executable, str(validator), str(workload)],
        label="workload validation",
    )
    version = run([str(binary), "--version"], label="binary version").stdout.strip()
    workload_metadata = workload / "workload_metadata.json"
    model_count = json.loads(workload_metadata.read_text(encoding="utf-8"))["model_count"]
    final_model = "orders" if model_count == 1 else f"orders_{model_count - 1:04d}"
    scenarios = [
        ("sql", [str(binary), "summary", "-p", str(workload / "sql_project"), "-o", "json", "--quiet"]),
        (
            "manifest",
            [
                str(binary),
                "summary",
                "-p",
                str(workload / "manifest_project"),
                "--source",
                "manifest",
                "--manifest-path",
                str(workload / "manifest_project/target/manifest.json"),
                "-o",
                "json",
                "--quiet",
            ],
        ),
        (
            "column",
            [
                str(binary),
                "column",
                "upstream",
                final_model,
                "--column",
                "amount",
                "-p",
                str(workload / "manifest_project"),
                "--manifest-path",
                str(workload / "manifest_project/target/manifest.json"),
                "-o",
                "json",
                "--quiet",
            ],
        ),
    ]
    results.mkdir(parents=True, exist_ok=True)
    metadata: dict[str, object] = {
        "git_head": git_head(),
        "binary": str(binary),
        "binary_version": version,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "os": platform.system(),
        "arch": platform.machine(),
        "cpu": platform.processor() or None,
        "workload": str(workload),
        "input_sizes": file_sizes(workload),
        "runs": args.runs,
        "warmup": args.warmup,
        "timing": not args.skip_timing,
        "cache_semantics": (
            "persistent-cold/persistent-warm distinguish persistent cache state "
            "only; OS/filesystem cache is not flushed"
        ),
        "manifest_scope": (
            "model-level DAG commands only; this suite does not measure column "
            "lineage or MCP typed Manifest replacement"
        ),
        "scenarios": [],
    }
    for name, command in scenarios:
        cache_dir = results / "cache" / name
        result_dir = results / "hyperfine"
        result_dir.mkdir(parents=True, exist_ok=True)
        metadata["scenarios"].append(
            scenario(
                name,
                [*command, "--cache-dir", str(cache_dir)],
                cache_dir,
                result_dir,
                args.runs,
                args.warmup,
                not args.skip_timing,
            )
        )
    metadata["finished_at_epoch"] = time.time()
    (results / "run_metadata.json").write_text(json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    print(f"wrote {results / 'run_metadata.json'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
