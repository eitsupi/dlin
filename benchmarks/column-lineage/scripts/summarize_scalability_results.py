#!/usr/bin/env python3
"""Publish a deterministic summary of the formal synthetic scale runs."""

from __future__ import annotations

import hashlib
import json
import os
import platform
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
PROFILE_CONFIG = ROOT / "metadata" / "scalability_profiles.json"
LOCAL_ROOT = ROOT / "results" / "local" / "scalability"
PUBLISHED_ROOT = ROOT / "results" / "published" / "scalability"
TOOLS = ROOT / "metadata" / "tools.json"


def load(path: Path) -> dict:
    return json.loads(path.read_text())


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def relative(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def git_head() -> str | None:
    git_entry = ROOT.parent.parent / ".git"
    if git_entry.is_file():
        marker = git_entry.read_text().strip()
        if marker.startswith("gitdir:"):
            git_entry = Path(marker.split(":", 1)[1].strip())
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
                if len(parts) > 1 and parts[1].isdigit():
                    return int(parts[1]) * 1024
    return None


def file_metadata(path: Path, expected: dict | None = None) -> dict:
    if not path.is_file():
        raise RuntimeError(f"missing artifact: {relative(path)}")
    actual = {"path": relative(path), "size_bytes": path.stat().st_size, "sha256": sha256(path)}
    if expected is not None and (actual["size_bytes"] != expected["size_bytes"] or actual["sha256"] != expected["sha256"]):
        raise RuntimeError(f"artifact metadata mismatch: {relative(path)}")
    return actual


def hyperfine_stats(scenario: dict, runs: int) -> dict:
    path = ROOT / scenario["hyperfine_json"]
    payload = load(path)
    results = payload.get("results")
    if not isinstance(results, list) or len(results) != 1:
        raise RuntimeError(f"unexpected hyperfine results: {scenario['name']}")
    result = results[0]
    times = result.get("times")
    if not isinstance(times, list) or len(times) != runs:
        raise RuntimeError(f"hyperfine sample count mismatch: {scenario['name']}")
    stats = {key: result.get(key) for key in ("mean", "median", "stddev", "min", "max")}
    if not all(isinstance(value, (int, float)) for value in stats.values()):
        raise RuntimeError(f"hyperfine statistics missing: {scenario['name']}")
    return {"sample_count": len(times), "raw_times": times, **stats}


def scenario_record(scenario: dict, runs: int) -> dict:
    status = scenario.get("status")
    reason = scenario.get("reason")
    if scenario.get("exit_code") == 124:
        status = "timeout"
        reason = f"timeout after {scenario.get('timeout_seconds', 120)} seconds"
    if status == "valid":
        if scenario.get("exit_code") != 0:
            raise RuntimeError(f"valid scenario has nonzero exit: {scenario['name']}")
        stats = hyperfine_stats(scenario, runs)
    else:
        stats = {"sample_count": None, "raw_times": None, "mean": None, "median": None, "stddev": None, "min": None, "max": None}
    return {
        "name": scenario["name"],
        "tool": scenario["tool"],
        "kind": scenario["kind"],
        "status": status,
        "reason": reason,
        "command": scenario.get("command"),
        "sample_count": stats["sample_count"],
        "raw_times": stats["raw_times"],
        "mean": stats["mean"],
        "median": stats["median"],
        "stddev": stats["stddev"],
        "min": stats["min"],
        "max": stats["max"],
        "raw_stdout": scenario.get("raw_stdout"),
        "raw_stderr": scenario.get("raw_stderr"),
        "probe_stdout": scenario.get("probe_stdout"),
        "probe_stderr": scenario.get("probe_stderr"),
        "hyperfine_json": scenario.get("hyperfine_json"),
    }


def profile_result(profile: dict) -> dict:
    name = profile["name"]
    profile_root = LOCAL_ROOT / name
    workload_path = profile_root / "workload.json"
    run_path = profile_root / "benchmark" / "run_metadata.json"
    workload = load(workload_path)
    run = load(run_path)
    if run.get("profile") != name or workload.get("profile") != name:
        raise RuntimeError(f"profile mismatch: {name}")
    if run.get("runs") != 3 or run.get("warmup") != 1 or run.get("timeout_seconds") != 120:
        raise RuntimeError(f"run configuration mismatch: {name}")
    if workload.get("family") != profile["family"] or workload.get("parameters") != profile["parameters"]:
        raise RuntimeError(f"workload profile mismatch: {name}")
    workload_expected = file_metadata(workload_path, {"size_bytes": workload_path.stat().st_size, "sha256": sha256(workload_path)})
    manifest_path = profile_root / "manifest.json"
    catalog_path = profile_root / "catalog.json"
    manifest = file_metadata(manifest_path, run["manifest"])
    catalog = file_metadata(catalog_path, run["catalog"])
    if workload["artifact"]["manifest_sha256"] != manifest["sha256"] or workload["artifact"]["catalog_sha256"] != catalog["sha256"]:
        raise RuntimeError(f"workload artifact hash mismatch: {name}")
    if run["workload"]["path"] != relative(workload_path) or run["workload"]["sha256"] != workload_expected["sha256"]:
        raise RuntimeError(f"workload metadata mismatch: {name}")
    dbt_index = run.get("dbt_meta_index", {"path": None, "size_bytes": None, "sha256": None})
    if dbt_index.get("path") and (ROOT / dbt_index["path"]).is_file():
        dbt_index = file_metadata(ROOT / dbt_index["path"], dbt_index)
    boundaries = run.get("boundaries", {})
    if "dlin_cache" in boundaries:
        boundaries["dlin_cache"] = {
            **boundaries["dlin_cache"],
            "reason": "v1 uses --no-cache because cache behavior is not yet verified",
        }
    scenarios = [scenario_record(scenario, run["runs"]) for scenario in run.get("scenarios", [])]
    return {
        "name": name,
        "family": profile["family"],
        "manual": False,
        "parameters": profile["parameters"],
        "model_count": workload["model_count"],
        "source_count": workload["source_count"],
        "total_declared_columns": workload["total_declared_columns"],
        "relation_edges": workload["relation_edges"],
        "resolved_column_edges": workload["resolved_column_edges"],
        "manifest": manifest,
        "catalog": catalog,
        "dbt_meta_index": dbt_index,
        "boundaries": boundaries,
        "scenarios": scenarios,
    }


def ratios(profiles: list[dict]) -> list[dict]:
    output = []
    families = []
    for profile in profiles:
        if profile["family"] not in families:
            families.append(profile["family"])
    for family in families:
        group = [profile for profile in profiles if profile["family"] == family]
        scenario_names = []
        for profile in group:
            for scenario in profile["scenarios"]:
                if scenario["name"] not in scenario_names:
                    scenario_names.append(scenario["name"])
        for scenario_name in scenario_names:
            for previous, current in zip(group, group[1:]):
                before = next((item for item in previous["scenarios"] if item["name"] == scenario_name), None)
                after = next((item for item in current["scenarios"] if item["name"] == scenario_name), None)
                if before is not None and after is not None and before["status"] == "valid" and after["status"] == "valid":
                    ratio = after["mean"] / before["mean"]
                    reason = "both consecutive measurements valid"
                else:
                    ratio = None
                    missing = []
                    for label, item in ((previous["name"], before), (current["name"], after)):
                        missing.append(f"{label}:{item['status'] if item else 'not_run'}")
                    reason = "; ".join(missing)
                output.append({"family": family, "scenario": scenario_name, "from_profile": previous["name"], "to_profile": current["name"], "mean_ratio": ratio, "reason": reason})
    return output


def format_measure(profile: dict, scenario_name: str, ratio_map: dict) -> str:
    scenario = next((item for item in profile["scenarios"] if item["name"] == scenario_name), None)
    if scenario is None:
        return "not-run"
    if scenario["status"] != "valid":
        return scenario["status"]
    ratio = ratio_map.get((profile["name"], scenario_name))
    ratio_text = "" if ratio is None or ratio["mean_ratio"] is None else f"; {ratio['mean_ratio']:.2f}x"
    return f"{scenario['mean']:.6f}s{ratio_text}"


def markdown(result: dict) -> str:
    tool_versions = ", ".join(
        f"{tool['name']} {tool['version']}" for tool in result["tool_versions"]
    )
    environment = result["environment"]
    formal_profiles = [profile["name"] for profile in result["profiles"]]
    reproduce = [
        "cd benchmarks/column-lineage",
        "./scripts/regenerate_artifacts.sh",
        "uv run --locked python scripts/preflight_tools.py",
        "uv run --locked python scripts/run_scalability_benchmarks.py \\",
    ]
    reproduce.extend(f'  --profile {name} \\' for name in formal_profiles[:-1])
    reproduce.append(f"  --profile {formal_profiles[-1]}")
    reproduce.append("uv run --locked python scripts/summarize_scalability_results.py")
    lines = [
        "# Synthetic scalability results",
        "",
        "These are three-run, one-warmup measurements with a 120-second inner timeout. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.",
        "",
        "## Run context",
        "",
        f"Tools: {tool_versions}. Runs/warmup/timeout: {result['runs']}/{result['warmup']}/{result['timeout_seconds']} seconds. Environment: {environment['os']}, {environment['arch']}, {environment['cpu_model']}, {environment['total_memory_bytes']} bytes memory.",
        "",
        "## Reproduce",
        "",
        "```sh",
        *reproduce,
        "```",
        "",
    ]
    ratio_map = {(item["to_profile"], item["scenario"]): item for item in result["ratios"]}
    dimension = {
        "volume": ("background models", "background_models"),
        "wide": ("width", "width"),
        "deep": ("depth", "depth"),
        "fanout": ("branches", "branches"),
    }
    for family in ("volume", "wide", "deep", "fanout"):
        group = [profile for profile in result["profiles"] if profile["family"] == family]
        if not group:
            continue
        scenario_names = []
        for profile in group:
            for scenario in profile["scenarios"]:
                if scenario["name"] not in scenario_names:
                    scenario_names.append(scenario["name"])
        lines.extend([f"## {family}", "", "| Profile | " + dimension[family][0] + " | Columns | Edges | " + " | ".join(scenario_names) + " |", "| --- | ---: | ---: | ---: | " + " | ".join(["---:"] * len(scenario_names)) + " |"])
        for profile in group:
            scale = profile["parameters"].get(dimension[family][1])
            cells = [format_measure(profile, scenario, ratio_map) for scenario in scenario_names]
            lines.append(f"| {profile['name']} | {scale} | {profile['total_declared_columns']} | {profile['resolved_column_edges']} | " + " | ".join(cells) + " |")
        lines.append("")
    lines.extend([
        "## Method and limitations",
        "",
        "- dlin uses `--no-cache`; its cold/warm cache distinction is not used here and OS cache drop is not performed.",
        "- Parrant timings include project parsing. dbt-meta build and query are separate scenarios.",
        "- Peak RSS is N/A. Canva is excluded. Whole-model unsupported scenarios and invalid/timeouts are shown as non-numeric statuses.",
        "- volume-100k dbt-meta build timed out; its dependent query is not-run and has null ratios.",
        "",
    ])
    return "\n".join(lines)


def main() -> int:
    config = load(PROFILE_CONFIG)
    profiles_config = [profile for profile in config["profiles"] if not profile["manual"]]
    profiles = [profile_result(profile) for profile in profiles_config]
    result = {
        "schema_version": 1,
        "environment": {"os": platform.platform(), "arch": platform.machine() or None, "cpu_model": cpu_model(), "total_memory_bytes": total_memory()},
        "git_head": git_head(),
        "tool_versions": [{key: target[key] for key in ("name", "version", "binary", "install")} for target in load(TOOLS)["comparison_targets"]],
        "runs": 3,
        "warmup": 1,
        "timeout_seconds": 120,
        "profiles": profiles,
    }
    result["ratios"] = ratios(profiles)
    PUBLISHED_ROOT.mkdir(parents=True, exist_ok=True)
    (PUBLISHED_ROOT / "results.json").write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
    (PUBLISHED_ROOT / "summary.md").write_text(markdown(result))
    print(relative(PUBLISHED_ROOT / "results.json"))
    print(relative(PUBLISHED_ROOT / "summary.md"))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
