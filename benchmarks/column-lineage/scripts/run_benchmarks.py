"""Run the first representative column-lineage benchmark scenarios."""

from __future__ import annotations

import json
import hashlib
import os
import shlex
import shutil
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "results" / "local" / "benchmark"
PREFLIGHT_STATUS = ROOT / "results" / "local" / "preflight" / "status.json"
CACHE_DIR = RESULTS / "cache" / "dlin"
META_ARTIFACT = RESULTS / "dbt-meta-lineage.json"
MANIFEST = "artifacts/manifest.json"
CATALOG = "artifacts/catalog.json"


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def write(path: Path, content: str) -> str:
    path.write_text(content)
    return relative(path)


def setting(name: str, default: int) -> int:
    value = int(os.environ.get(name, str(default)))
    if value < 1:
        raise ValueError(f"{name} must be positive")
    return value


def preflight_is_valid() -> bool:
    try:
        payload = json.loads(PREFLIGHT_STATUS.read_text())
        commands = payload["commands"]
        return len(commands) == 10 and all(command["valid"] for command in commands)
    except (OSError, json.JSONDecodeError, KeyError, TypeError):
        return False


def ensure_preflight() -> None:
    if preflight_is_valid():
        return
    completed = subprocess.run(
        ["uv", "run", "--locked", "python", "scripts/preflight_tools.py"],
        cwd=ROOT,
        capture_output=True,
        text=True,
        check=False,
        timeout=180,
    )
    if completed.returncode != 0 or not preflight_is_valid():
        raise RuntimeError("preflight status is not 10/10 valid")


def redirected(command: str, stdout_path: Path, stderr_path: Path) -> str:
    return (
        f"{command} > {shlex.quote(relative(stdout_path))}"
        f" 2> {shlex.quote(relative(stderr_path))}"
    )


def prepare(name: str, argv: list[str]) -> bool:
    stdout_path = RESULTS / f"{name}.stdout"
    stderr_path = RESULTS / f"{name}.stderr"
    try:
        completed = subprocess.run(
            argv,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=60,
        )
    except (OSError, subprocess.TimeoutExpired) as error:
        write(stdout_path, "")
        write(stderr_path, str(error))
        return False
    write(stdout_path, completed.stdout)
    write(stderr_path, completed.stderr)
    return completed.returncode == 0


def benchmark(
    name: str,
    kind: str,
    command: str,
    runs: int,
    warmup: int,
    records: list[dict[str, object]],
) -> None:
    hyperfine_json = RESULTS / f"{name}.hyperfine.json"
    hyperfine_stdout = RESULTS / f"{name}.hyperfine.stdout"
    hyperfine_stderr = RESULTS / f"{name}.hyperfine.stderr"
    hyperfine_json.unlink(missing_ok=True)
    invocation = [
        "hyperfine",
        "--runs",
        str(runs),
        "--warmup",
        str(warmup),
        "--export-json",
        relative(hyperfine_json),
        "--command-name",
        name,
        command,
    ]
    outer_timeout = (runs + warmup) * 60 + 30
    try:
        completed = subprocess.run(
            invocation,
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
            timeout=outer_timeout,
        )
        exit_code = completed.returncode
        stdout = completed.stdout
        stderr = completed.stderr
    except (OSError, subprocess.TimeoutExpired) as error:
        exit_code = 124
        stdout = ""
        stderr = str(error)
    write(hyperfine_stdout, stdout)
    write(hyperfine_stderr, stderr)
    records.append(
        {
            "name": name,
            "kind": kind,
            "command": command,
            "hyperfine": " ".join(shlex.quote(arg) for arg in invocation),
            "hyperfine_json": relative(hyperfine_json),
            "exit_code": exit_code,
            "reason": "hyperfine completed" if exit_code == 0 else "hyperfine failed",
            "timeout_seconds": outer_timeout,
            "raw_stdout": relative(RESULTS / f"{name}.stdout"),
            "raw_stderr": relative(RESULTS / f"{name}.stderr"),
        }
    )


def skipped(
    name: str,
    kind: str,
    command: str,
    reason: str,
    records: list[dict[str, object]],
) -> None:
    stdout_path = RESULTS / f"{name}.stdout"
    stderr_path = RESULTS / f"{name}.stderr"
    hyperfine_json = RESULTS / f"{name}.hyperfine.json"
    hyperfine_json.unlink(missing_ok=True)
    write(stdout_path, "")
    write(stderr_path, reason)
    records.append(
        {
            "name": name,
            "kind": kind,
            "command": command,
            "hyperfine": None,
            "hyperfine_json": relative(hyperfine_json),
            "exit_code": 126,
            "reason": reason,
            "raw_stdout": relative(stdout_path),
            "raw_stderr": relative(stderr_path),
        }
    )


def artifact_metadata(path: Path) -> dict[str, object]:
    if not path.is_file():
        raise FileNotFoundError(path)
    return {
        "path": relative(path),
        "size_bytes": path.stat().st_size,
        "sha256": hashlib.sha256(path.read_bytes()).hexdigest(),
    }


def dlin_command(direction: str, model: str, column: str, cache: str) -> str:
    return " ".join(
        [
            "timeout",
            "60s",
            "dlin",
            "column",
            direction,
            model,
            "--column",
            column,
            "--manifest-path",
            MANIFEST,
            "--cache-dir",
            cache,
            "--no-cache" if cache == "" else "",
            "-o",
            "json",
        ]
    ).replace("  ", " ").strip()


def main() -> int:
    manifest_path = ROOT / MANIFEST
    catalog_path = ROOT / CATALOG
    manifest_metadata = artifact_metadata(manifest_path)
    catalog_metadata = artifact_metadata(catalog_path)
    ensure_preflight()
    runs = setting("BENCHMARK_RUNS", 3)
    warmup = setting("BENCHMARK_WARMUP", 1)
    RESULTS.mkdir(parents=True, exist_ok=True)
    CACHE_DIR.parent.mkdir(parents=True, exist_ok=True)
    shutil.rmtree(CACHE_DIR, ignore_errors=True)
    META_ARTIFACT.unlink(missing_ok=True)
    records: list[dict[str, object]] = []

    for direction, model, column in (
        ("upstream", "i01_deep_08", "amount_double"),
        ("downstream", "i05_fanout_base", "amount"),
    ):
        base = dlin_command(direction, model, column, relative(CACHE_DIR))
        stdout_path = RESULTS / f"dlin_{direction}_cold.stdout"
        stderr_path = RESULTS / f"dlin_{direction}_cold.stderr"
        cold = (
            f"rm -rf -- {shlex.quote(relative(CACHE_DIR))} && "
            f"{base.replace('--cache-dir ' + relative(CACHE_DIR), '--refresh-cache --cache-dir ' + relative(CACHE_DIR))}"
        )
        benchmark(
            f"dlin_{direction}_cold",
            "cold",
            redirected(cold, stdout_path, stderr_path),
            runs,
            warmup,
            records,
        )
        warm_ready = prepare(
            f"dlin_{direction}_warm_prepare",
            [
                "dlin",
                "column",
                direction,
                model,
                "--column",
                column,
                "--manifest-path",
                MANIFEST,
                "--cache-dir",
                relative(CACHE_DIR),
                "--refresh-cache",
                "-o",
                "json",
            ],
        )
        stdout_path = RESULTS / f"dlin_{direction}_warm.stdout"
        stderr_path = RESULTS / f"dlin_{direction}_warm.stderr"
        warm_name = f"dlin_{direction}_warm"
        warm_command = redirected(base, stdout_path, stderr_path)
        if warm_ready:
            benchmark(warm_name, "warm", warm_command, runs, warmup, records)
        else:
            skipped(warm_name, "warm", warm_command, "warm cache preparation failed; measurement skipped", records)

    for name, select in (
        ("upstream", "+i01_deep_08.amount_double"),
        ("downstream", "i05_fanout_base.amount+"),
    ):
        command = (
            f"timeout 60s parrant --manifest {MANIFEST} --catalog {CATALOG} "
            f"--select {shlex.quote(select)} --format json"
        )
        benchmark(
            f"parrant_{name}",
            "query_parse_included",
            redirected(
                command,
                RESULTS / f"parrant_{name}.stdout",
                RESULTS / f"parrant_{name}.stderr",
            ),
            runs,
            warmup,
            records,
        )

    meta_build = (
        f"rm -f -- {shlex.quote(relative(META_ARTIFACT))} && "
        f"timeout 60s meta lineage build --manifest {MANIFEST} --catalog {CATALOG} "
        f"--output {shlex.quote(relative(META_ARTIFACT))} --json --no-compile"
    )
    benchmark(
        "dbt_meta_build",
        "build",
        redirected(
            meta_build,
            RESULTS / "dbt_meta_build.stdout",
            RESULTS / "dbt_meta_build.stderr",
        ),
        runs,
        warmup,
        records,
    )
    meta_query_ready = prepare(
        "dbt_meta_query_prepare",
        [
            "meta",
            "lineage",
            "build",
            "--manifest",
            MANIFEST,
            "--catalog",
            CATALOG,
            "--output",
            relative(META_ARTIFACT),
            "--json",
            "--no-compile",
        ],
    )
    for name, command in (
        (
            "upstream",
            f"timeout 60s meta lineage column --artifact {shlex.quote(relative(META_ARTIFACT))} --json i01_deep_08.amount_double",
        ),
        (
            "downstream",
            f"timeout 60s meta lineage downstream --artifact {shlex.quote(relative(META_ARTIFACT))} --json i05_fanout_base.amount",
        ),
    ):
        query_name = f"dbt_meta_{name}"
        query_command = redirected(
            command,
            RESULTS / f"{query_name}.stdout",
            RESULTS / f"{query_name}.stderr",
        )
        if meta_query_ready:
            benchmark(query_name, "query", query_command, runs, warmup, records)
        else:
            skipped(query_name, "query", query_command, "lineage artifact preparation failed; measurement skipped", records)

    metadata = {
        "schema_version": 1,
        "runs": runs,
        "warmup": warmup,
        "manifest": manifest_metadata,
        "catalog": catalog_metadata,
        "scenarios": records,
    }
    metadata_path = RESULTS / "run_metadata.json"
    metadata_path.write_text(json.dumps(metadata, indent=2) + "\n")
    failures = [record for record in records if record["exit_code"] != 0]
    for record in records:
        mean = None
        hyperfine_path = ROOT / str(record["hyperfine_json"])
        if hyperfine_path.is_file():
            try:
                mean = json.loads(hyperfine_path.read_text())["results"][0]["mean"]
            except (KeyError, IndexError, TypeError, json.JSONDecodeError):
                pass
        print(f"{record['name']}: mean={mean}")
    print(f"benchmark metadata: {relative(metadata_path)}")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
