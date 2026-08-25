"""Summarize the current local benchmark run without copying raw outputs."""

from __future__ import annotations

import hashlib
import json
import platform
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RESULTS = ROOT / "results" / "local" / "benchmark"
RUN_METADATA = RESULTS / "run_metadata.json"
PREFLIGHT = ROOT / "results" / "local" / "preflight" / "status.json"
TOOLS = ROOT / "metadata" / "tools.json"
CORPUS = ROOT / "metadata" / "corpus.json"


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def git_head() -> str | None:
    git_entry = ROOT.parent.parent / ".git"
    if git_entry.is_file():
        text = git_entry.read_text().strip()
        if text.startswith("gitdir:"):
            git_entry = Path(text.split(":", 1)[1].strip())
            if not git_entry.is_absolute():
                git_entry = (ROOT.parent.parent / git_entry).resolve()
    head_path = git_entry / "HEAD"
    if not head_path.is_file():
        return None
    head = head_path.read_text().strip()
    if not head.startswith("ref: "):
        return head or None
    ref = head[5:]
    ref_path = git_entry / ref
    if ref_path.is_file():
        return ref_path.read_text().strip() or None
    packed = git_entry / "packed-refs"
    if packed.is_file():
        for line in packed.read_text().splitlines():
            if line and not line.startswith("#") and not line.startswith("^"):
                sha, name = line.split(" ", 1)
                if name == ref:
                    return sha
    return None


def cpu_model() -> str | None:
    cpuinfo = Path("/proc/cpuinfo")
    if cpuinfo.is_file():
        for line in cpuinfo.read_text(errors="replace").splitlines():
            if line.lower().startswith("model name") and ":" in line:
                return line.split(":", 1)[1].strip()
    return platform.processor() or None


def total_memory() -> int | None:
    meminfo = Path("/proc/meminfo")
    if meminfo.is_file():
        for line in meminfo.read_text(errors="replace").splitlines():
            if line.startswith("MemTotal:"):
                parts = line.split()
                if len(parts) >= 2 and parts[1].isdigit():
                    return int(parts[1]) * 1024
    return None


def artifact_summary(metadata: dict, name: str) -> dict:
    entry = metadata[name]
    path = ROOT / entry["path"]
    if not path.is_file():
        raise RuntimeError(f"missing artifact: {entry['path']}")
    actual_bytes = path.read_bytes()
    actual_sha = hashlib.sha256(actual_bytes).hexdigest()
    if len(actual_bytes) != entry["size_bytes"] or actual_sha != entry["sha256"]:
        raise RuntimeError(f"artifact metadata mismatch: {entry['path']}")
    return {
        "path": entry["path"],
        "size_bytes": entry["size_bytes"],
        "sha256": entry["sha256"],
    }


def scenario_summary(scenario: dict, runs: int) -> dict:
    raw_paths = {
        "stdout": scenario["raw_stdout"],
        "stderr": scenario["raw_stderr"],
    }
    for raw_path in raw_paths.values():
        if not (ROOT / raw_path).is_file():
            raise RuntimeError(f"missing raw output: {raw_path}")
    result = {
        "name": scenario["name"],
        "kind": scenario["kind"],
        "command": scenario["command"],
        "exit_code": scenario["exit_code"],
        "reason": scenario.get("reason"),
        "raw_stdout": raw_paths["stdout"],
        "raw_stderr": raw_paths["stderr"],
        "hyperfine_json": scenario["hyperfine_json"],
    }
    if scenario["exit_code"] != 0:
        result["status"] = "invalid"
        return result

    hyperfine_path = ROOT / scenario["hyperfine_json"]
    if not hyperfine_path.is_file():
        raise RuntimeError(f"missing hyperfine result: {scenario['hyperfine_json']}")
    payload = load(hyperfine_path)
    results = payload.get("results")
    if not isinstance(results, list) or len(results) != 1:
        raise RuntimeError(f"unexpected hyperfine result shape: {scenario['name']}")
    measured = results[0]
    times = measured.get("times")
    if not isinstance(times, list) or len(times) != runs:
        raise RuntimeError(f"sample count mismatch: {scenario['name']}")
    for key in ("mean", "median", "stddev", "min", "max"):
        if not isinstance(measured.get(key), (int, float)):
            raise RuntimeError(f"missing hyperfine statistic {key}: {scenario['name']}")
    result["status"] = "valid"
    result["sample_count"] = len(times)
    result["stats_seconds"] = {
        key: measured[key] for key in ("mean", "median", "stddev", "min", "max")
    }
    return result


def format_seconds(value: float) -> str:
    return f"{value:.6f}"


def table(title: str, rows: list[dict]) -> list[str]:
    lines = [f"### {title}", "", "| Scenario | Mean (s) | Median (s) | Stddev (s) | Min (s) | Max (s) | Samples |", "| --- | ---: | ---: | ---: | ---: | ---: | ---: |"]
    for row in rows:
        stats = row["stats_seconds"]
        lines.append(
            f"| {row['name']} | {format_seconds(stats['mean'])} | {format_seconds(stats['median'])} | {format_seconds(stats['stddev'])} | {format_seconds(stats['min'])} | {format_seconds(stats['max'])} | {row['sample_count']} |"
        )
    if not rows:
        lines.append("| No valid scenarios | | | | | | |")
    lines.append("")
    return lines


def main() -> int:
    metadata = load(RUN_METADATA)
    preflight = load(PREFLIGHT)
    tools = load(TOOLS)
    corpus = load(CORPUS)
    commands = preflight.get("commands", [])
    preflight_valid = len(commands) == 10 and all(command.get("valid") for command in commands)
    runs = metadata["runs"]
    scenario_results = [scenario_summary(scenario, runs) for scenario in metadata["scenarios"]]
    artifacts = {
        "manifest": artifact_summary(metadata, "manifest"),
        "catalog": artifact_summary(metadata, "catalog"),
    }
    tool_summary = [
        {key: target[key] for key in ("name", "version", "binary", "install")}
        for target in tools["comparison_targets"]
    ]
    summary = {
        "schema_version": 1,
        "git_head": git_head(),
        "environment": {
            "os": platform.platform(),
            "arch": platform.machine() or None,
            "cpu_model": cpu_model(),
            "total_memory_bytes": total_memory(),
        },
        "tools": tool_summary,
        "artifacts": artifacts,
        "runs": runs,
        "warmup": metadata["warmup"],
        "preflight": {
            "path": relative(PREFLIGHT),
            "valid_commands": sum(command.get("valid", False) for command in commands),
            "total_commands": len(commands),
            "representative_only": True,
        },
        "corpus": {
            "name": corpus["name"],
            "atomic_cases": corpus["atomic_cases"],
            "integration_cases": corpus["integration_cases"],
        },
        "scenarios": scenario_results,
    }
    summary_path = RESULTS / "summary.json"
    summary_path.write_text(json.dumps(summary, indent=2) + "\n")

    valid = [row for row in scenario_results if row["status"] == "valid"]
    invalid = [row for row in scenario_results if row["status"] != "valid"]
    preparation = [row for row in valid if row["kind"] in {"cold", "build"}]
    queries = [row for row in valid if row["kind"] in {"warm", "query_parse_included", "query"}]
    markdown = [
        "# Benchmark summary",
        "",
        "This is a three-run quick measurement. It reports measurements only and declares no ranking or winner.",
        "",
        *table("Preparation and cold", preparation),
        *table("Query", queries),
        "## Run context",
        "",
        f"Runs: {runs}. Warmup: {metadata['warmup']}. Times are seconds.",
        f"Git HEAD: `{summary['git_head'] or 'unavailable'}`.",
        f"Environment: {summary['environment']['os']}; arch `{summary['environment']['arch']}`; CPU `{summary['environment']['cpu_model']}`; total memory `{summary['environment']['total_memory_bytes']}` bytes.",
        f"Preflight: {summary['preflight']['valid_commands']}/{summary['preflight']['total_commands']} representative commands valid. This is not a full 16-case correctness score.",
        f"Manifest: `{artifacts['manifest']['path']}`, {artifacts['manifest']['size_bytes']} bytes, SHA-256 `{artifacts['manifest']['sha256']}`.",
        f"Catalog: `{artifacts['catalog']['path']}`, {artifacts['catalog']['size_bytes']} bytes, SHA-256 `{artifacts['catalog']['sha256']}`.",
        "dlin cold removes and refreshes its dedicated cache; dlin warm uses a prepopulated cache. Parrant query measurements include project parsing. dbt-meta query measurements exclude lineage build.",
    ]
    if invalid:
        markdown.extend(["", "## Invalid or unsupported scenarios", ""])
        markdown.extend(f"- `{row['name']}`: exit {row['exit_code']}; {row['reason']}" for row in invalid)
    (RESULTS / "summary.md").write_text("\n".join(markdown) + "\n")

    if not preflight_valid or invalid:
        print("summary generated with invalid input or scenarios", file=sys.stderr)
        return 1
    print(f"summary: {relative(summary_path)}")
    print(f"markdown: {relative(RESULTS / 'summary.md')}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
