#!/usr/bin/env python3
"""Run bounded three-tool measurements for generated scalability profiles."""

from __future__ import annotations

import argparse
import hashlib
import json
import shlex
import subprocess
import sys
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
RESULTS_ROOT = ROOT / "results" / "local" / "scalability"
PREFLIGHT = ROOT / "results" / "local" / "preflight" / "status.json"
GENERATOR = "scripts/generate_scalability_artifacts.py"
MANIFEST = "manifest.json"
CATALOG = "catalog.json"


def rel(path: Path) -> str:
    return path.relative_to(ROOT).as_posix()


def load_json(path: Path) -> dict:
    return json.loads(path.read_text())


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def preflight_valid() -> bool:
    try:
        payload = load_json(PREFLIGHT)
        commands = payload["commands"]
        return len(commands) == 10 and all(command.get("valid") for command in commands)
    except (OSError, KeyError, TypeError, json.JSONDecodeError):
        return False


def run_capture(argv: list[str], cwd: Path, timeout: int, stdout_path: Path, stderr_path: Path) -> int:
    try:
        completed = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, check=False, timeout=timeout)
        stdout_path.write_text(completed.stdout)
        stderr_path.write_text(completed.stderr)
        return completed.returncode
    except subprocess.TimeoutExpired as error:
        stdout = error.stdout or ""
        stderr = error.stderr or ""
        if isinstance(stdout, bytes):
            stdout = stdout.decode(errors="replace")
        if isinstance(stderr, bytes):
            stderr = stderr.decode(errors="replace")
        stdout_path.write_text(stdout)
        stderr_path.write_text(f"timeout after {timeout}s\n{stderr}")
        return 124
    except OSError as error:
        stdout_path.write_text("")
        stderr_path.write_text(str(error))
        return 127


def json_text(path: Path) -> object:
    return json.loads(path.read_text())


def model_name(unique_id: str) -> str:
    return unique_id.rsplit(".", 1)[-1]


def source_name(unique_id: str) -> str:
    return unique_id.rsplit(".", 1)[-1]


def upstream_labels(unique_ids: list[str]) -> dict[str, str]:
    labels = {}
    for unique_id in unique_ids:
        label = source_name(unique_id) if unique_id.startswith("source.") else model_name(unique_id)
        labels[label] = unique_id
    return labels


def dlin_upstream_valid(stdout: Path, workload: dict) -> str:
    payload = json_text(stdout)
    assert isinstance(payload, list) and payload, "dlin upstream JSON is empty"
    query = workload["selected_queries"]["upstream"]
    expected_model = model_name(query["model"])
    entry = next(item for item in payload if item.get("model") == expected_model)
    column = next(item for item in entry["columns"] if item["column"] == query["column"])
    expected_source = source_name(query["expected_terminal_source_ids"][0])
    actual = {
        (str(item.get("table", "")).split(".")[-1], item.get("column"))
        for item in column["sources"]
    }
    assert actual == {(expected_source, query["column"])}, "dlin source mismatch"
    return "dlin upstream terminal source found"


def dlin_whole_valid(stdout: Path, workload: dict) -> str:
    payload = json_text(stdout)
    assert isinstance(payload, list) and payload, "dlin whole-model JSON is empty"
    query = workload["selected_queries"]["upstream"]
    entry = next(item for item in payload if item.get("model") == model_name(query["model"]))
    expected_columns = {f"c{i:04d}" for i in range(1, workload["parameters"]["width"] + 1)}
    actual_columns = {item["column"] for item in entry["columns"]}
    assert entry["traced_columns"] == len(expected_columns) and entry["total_columns"] == len(expected_columns), "dlin whole-model width mismatch"
    assert actual_columns == expected_columns, "dlin whole-model columns mismatch"
    expected_source = source_name(query["expected_terminal_source_ids"][0])
    for item in entry["columns"]:
        sources = item.get("sources", [])
        assert len(sources) == 1 and expected_source in str(sources[0].get("table", "")) and sources[0].get("column") == item["column"], "dlin whole-model source mismatch"
    return "dlin whole-model columns and terminal sources found"


def dlin_downstream_valid(stdout: Path, workload: dict) -> str:
    payload = json_text(stdout)
    assert isinstance(payload, list) and payload, "dlin downstream JSON is empty"
    query = workload["selected_queries"]["downstream"]
    entry = next(
        item
        for item in payload
        if item.get("model") == model_name(query["model"])
        and item.get("column") == query["column"]
    )
    actual = {
        (item.get("unique_id"), item.get("column"))
        for item in entry.get("impacted_columns", [])
    }
    expected = {(target, query["column"]) for target in query["expected_target_ids"]}
    actual.discard((query["model"], query["column"]))
    assert actual == expected, "dlin downstream targets mismatch"
    return "dlin downstream targets found"


def parrant_coverage(entry: dict, workload: dict) -> None:
    coverage = entry["coverage"]
    assert coverage["parsed_ok"] == workload["model_count"], "Parrant parsed coverage incomplete"
    assert coverage["parse_failed"] == 0, "Parrant parse failures present"


def parrant_upstream_valid(stdout: Path, workload: dict) -> str:
    entry = json_text(stdout)
    query = workload["selected_queries"]["upstream"]
    parrant_coverage(entry, workload)
    assert entry["model"] == model_name(query["model"]) and entry["column"] == query["column"], "Parrant target mismatch"
    expected_ids = query["expected_upstream_node_ids"]
    labels = upstream_labels(expected_ids)
    models = entry["upstream"]["models"]
    actual_ids = set()
    for label, columns in models.items():
        assert label in labels, f"Parrant unexpected upstream node: {label}"
        actual_ids.add(labels[label])
        assert set(columns) == {query["column"]}, "Parrant upstream column mismatch"
    assert actual_ids == set(expected_ids), "Parrant upstream node set mismatch"
    expected_edges = {
        edge["child_id"]: edge["parent_id"]
        for edge in query["expected_upstream_edges"]
        if edge["child_id"] in expected_ids
    }
    for label, columns in models.items():
        unique_id = labels[label]
        column_entry = columns[query["column"]]
        actual_refs = set()
        for source in column_entry.get("source_columns", []):
            name, column = source.split(".", 1)
            actual_refs.add((name, column))
        if unique_id.startswith("source."):
            expected_refs = {(label, query["column"])}
        else:
            parent_id = expected_edges[unique_id]
            parent_label = next(label for label, uid in labels.items() if uid == parent_id)
            expected_refs = {(parent_label, query["column"])}
        assert actual_refs == expected_refs, "Parrant upstream edge mismatch"
    return "Parrant upstream terminal source found"


def parrant_downstream_valid(stdout: Path, workload: dict) -> str:
    entry = json_text(stdout)
    query = workload["selected_queries"]["downstream"]
    parrant_coverage(entry, workload)
    assert entry["model"] == model_name(query["model"]) and entry["column"] == query["column"], "Parrant target mismatch"
    actual = {
        (model_id, column)
        for model_id, columns in entry["downstream"]["models"].items()
        for column in columns
    }
    expected = {(model_name(uid), query["column"]) for uid in query["expected_target_ids"]}
    actual.discard((model_name(query["model"]), query["column"]))
    assert actual == expected, "Parrant downstream targets mismatch"
    return "Parrant downstream targets found"


def meta_build_valid(stdout: Path, workload: dict, artifact: Path) -> str:
    summary = json_text(stdout)
    assert artifact.is_file(), "dbt-meta artifact was not created"
    graph = load_json(artifact)["stats"]
    assert graph["nodes"] == workload["total_declared_columns"], "dbt-meta node count mismatch"
    assert graph["edges"] == workload["resolved_column_edges"], "dbt-meta edge count mismatch"
    assert summary["nodes"] == graph["nodes"] and summary["edges"] == graph["edges"], "dbt-meta build summary mismatch"
    return "dbt-meta graph counts match workload"


def meta_upstream_valid(stdout: Path, workload: dict) -> str:
    entry = json_text(stdout)
    query = workload["selected_queries"]["upstream"]
    expected_source = source_name(query["expected_terminal_source_ids"][0])
    expected_target = f"{model_name(query['model'])}.{query['column']}"
    assert entry["target"]["id"] == expected_target, "dbt-meta target mismatch"
    identities = {(item.get("model"), item.get("column")) for item in entry["all"]}
    expected = {
        (
            f"source.main.{source_name(unique_id)}" if unique_id.startswith("source.") else model_name(unique_id),
            query["column"],
        )
        for unique_id in query["expected_upstream_node_ids"]
    }
    assert identities == expected, "dbt-meta upstream node/column set mismatch"
    assert (f"source.main.{expected_source}", query["column"]) in identities, "dbt-meta source mismatch"
    return "dbt-meta upstream terminal source found"


def meta_downstream_valid(stdout: Path, workload: dict) -> str:
    entry = json_text(stdout)
    query = workload["selected_queries"]["downstream"]
    expected_target = f"{model_name(query['model'])}.{query['column']}"
    assert entry["target"]["id"] == expected_target, "dbt-meta target mismatch"
    actual = {item["id"] for item in entry["all"]}
    expected = {f"{model_name(uid)}.{query['column']}" for uid in query["expected_target_ids"]}
    actual.discard(f"{model_name(query['model'])}.{query['column']}")
    assert actual == expected, "dbt-meta downstream targets mismatch"
    return "dbt-meta downstream targets found"


def command_string(argv: list[str]) -> str:
    return " ".join(shlex.quote(item) for item in argv)


def probe_and_record(name: str, tool: str, kind: str, argv: list[str], validator, benchmark_dir: Path, workload: dict, timeout: int, records: list[dict]) -> bool:
    probe_stdout = benchmark_dir / f"{name}.probe.stdout"
    probe_stderr = benchmark_dir / f"{name}.probe.stderr"
    exit_code = run_capture(argv, ROOT, timeout, probe_stdout, probe_stderr)
    record = {
        "name": name,
        "tool": tool,
        "kind": kind,
        "command": command_string(argv),
        "status": "invalid",
        "exit_code": exit_code,
        "reason": f"timeout after {timeout} seconds" if exit_code == 124 else ("command failed" if exit_code else "semantic validation failed"),
        "probe_stdout": rel(probe_stdout),
        "probe_stderr": rel(probe_stderr),
        "raw_stdout": None,
        "raw_stderr": None,
        "hyperfine_json": None,
    }
    if exit_code == 0:
        try:
            record["reason"] = validator(probe_stdout, workload)
            record["validation_reason"] = record["reason"]
            record["status"] = "valid"
        except (AssertionError, IndexError, KeyError, OSError, StopIteration, TypeError, ValueError, json.JSONDecodeError) as error:
            record["reason"] = str(error) or error.__class__.__name__
    records.append(record)
    return record["status"] == "valid"


def probe_unsupported(name: str, tool: str, kind: str, argv: list[str], reason: str, benchmark_dir: Path, timeout: int, records: list[dict]) -> None:
    stdout = benchmark_dir / f"{name}.probe.stdout"
    stderr = benchmark_dir / f"{name}.probe.stderr"
    exit_code = run_capture(argv, ROOT, timeout, stdout, stderr)
    records.append({"name": name, "tool": tool, "kind": kind, "command": command_string(argv), "status": "unsupported", "exit_code": exit_code, "reason": reason, "probe_stdout": rel(stdout), "probe_stderr": rel(stderr), "raw_stdout": None, "raw_stderr": None, "hyperfine_json": None})


def record_unsupported(name: str, tool: str, kind: str, reason: str, records: list[dict]) -> None:
    records.append({"name": name, "tool": tool, "kind": kind, "status": "unsupported", "command": None, "exit_code": None, "reason": reason, "probe_stdout": None, "probe_stderr": None, "raw_stdout": None, "raw_stderr": None, "hyperfine_json": None})


def measure(record: dict, argv: list[str], benchmark_dir: Path, runs: int, warmup: int, timeout: int) -> None:
    name = record["name"]
    raw_stdout = benchmark_dir / f"{name}.stdout"
    raw_stderr = benchmark_dir / f"{name}.stderr"
    hyperfine_json = benchmark_dir / f"{name}.hyperfine.json"
    timed = f"/usr/bin/timeout {timeout}s {command_string(argv)} > {shlex.quote(rel(raw_stdout))} 2> {shlex.quote(rel(raw_stderr))}"
    invocation = ["hyperfine", "--runs", str(runs), "--warmup", str(warmup), "--export-json", rel(hyperfine_json), "--command-name", name, timed]
    outer_timeout = (runs + warmup) * timeout + 30
    stdout = benchmark_dir / f"{name}.hyperfine.stdout"
    stderr = benchmark_dir / f"{name}.hyperfine.stderr"
    exit_code = run_capture(invocation, ROOT, outer_timeout, stdout, stderr)
    status = "valid" if exit_code == 0 else "invalid"
    reason = ("hyperfine completed; " + str(record.get("validation_reason", "semantic probe valid"))) if exit_code == 0 else "hyperfine failed"
    sample_count = None
    if exit_code == 0:
        try:
            payload = load_json(hyperfine_json)
            results = payload.get("results")
            times = results[0].get("times") if isinstance(results, list) and len(results) == 1 else None
            if not isinstance(times, list) or len(times) != runs:
                raise ValueError(f"hyperfine sample count mismatch: expected {runs}")
            sample_count = len(times)
        except (OSError, KeyError, IndexError, TypeError, ValueError, json.JSONDecodeError) as error:
            status = "invalid"
            reason = str(error) or error.__class__.__name__
    record.update({"command": timed, "hyperfine": command_string(invocation), "hyperfine_json": rel(hyperfine_json), "raw_stdout": rel(raw_stdout), "raw_stderr": rel(raw_stderr), "exit_code": exit_code, "status": status, "reason": reason, "sample_count": sample_count, "timeout_seconds": timeout, "outer_timeout_seconds": outer_timeout})


def metadata(path: Path) -> dict:
    return {"path": rel(path), "size_bytes": path.stat().st_size, "sha256": sha256(path)}


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--profile", action="append", required=True)
    parser.add_argument("--runs", type=int, default=3)
    parser.add_argument("--warmup", type=int, default=1)
    parser.add_argument("--timeout", type=int, default=120)
    parser.add_argument("--allow-manual", action="store_true")
    args = parser.parse_args()
    if args.runs < 1 or args.warmup < 0 or args.timeout < 1:
        parser.error("runs and timeout must be positive; warmup must be non-negative")
    if not preflight_valid():
        print("preflight status is not 10/10 valid; run uv run --locked python scripts/preflight_tools.py first", file=sys.stderr)
        return 2
    failures = False
    for profile_name in args.profile:
        profile_dir = RESULTS_ROOT / profile_name
        benchmark_dir = profile_dir / "benchmark"
        benchmark_dir.mkdir(parents=True, exist_ok=True)
        generator = ["uv", "run", "--locked", "python", GENERATOR, "--profile", profile_name, "--output-root", rel(RESULTS_ROOT)]
        if args.allow_manual:
            generator.append("--allow-manual")
        gen_exit = run_capture(generator, ROOT, 300, benchmark_dir / "generator.stdout", benchmark_dir / "generator.stderr")
        if gen_exit != 0:
            print(f"{profile_name}: generator failed")
            failures = True
            continue
        workload_path = profile_dir / "workload.json"
        manifest_path = profile_dir / MANIFEST
        catalog_path = profile_dir / CATALOG
        workload = load_json(workload_path)
        records: list[dict] = []
        model = workload["selected_queries"]["upstream"]["model"]
        column = workload["selected_queries"]["upstream"]["column"]
        model_name_value = model_name(model)
        common = ["--manifest-path", rel(manifest_path), "-o", "json"]
        if workload["family"] in {"volume", "wide", "deep"}:
            dlin_up = ["dlin", "column", "upstream", model_name_value, "--column", column, *common, "--no-cache"]
            if probe_and_record("dlin_upstream", "dlin", "upstream_no_cache", dlin_up, dlin_upstream_valid, benchmark_dir, workload, args.timeout, records):
                measure(records[-1], dlin_up, benchmark_dir, args.runs, args.warmup, args.timeout)
            if workload["family"] == "wide":
                whole_dlin = ["dlin", "column", "upstream", model_name_value, *common, "--no-cache"]
                if probe_and_record("dlin_whole_model", "dlin", "whole_model_no_cache", whole_dlin, dlin_whole_valid, benchmark_dir, workload, args.timeout, records):
                    measure(records[-1], whole_dlin, benchmark_dir, args.runs, args.warmup, args.timeout)
        if workload["family"] == "fanout":
            downstream = workload["selected_queries"]["downstream"]
            dlin_down = ["dlin", "column", "downstream", model_name(downstream["model"]), "--column", downstream["column"], *common, "--no-cache"]
            if probe_and_record("dlin_downstream", "dlin", "downstream_no_cache", dlin_down, dlin_downstream_valid, benchmark_dir, workload, args.timeout, records):
                measure(records[-1], dlin_down, benchmark_dir, args.runs, args.warmup, args.timeout)
        parrant_up = ["parrant", "--manifest", rel(manifest_path), "--catalog", rel(catalog_path), "--select", f"+{model_name_value}.{column}", "--format", "json"]
        if workload["family"] in {"volume", "wide", "deep"}:
            if probe_and_record("parrant_upstream", "parrant", "upstream_parse_included", parrant_up, parrant_upstream_valid, benchmark_dir, workload, args.timeout, records):
                measure(records[-1], parrant_up, benchmark_dir, args.runs, args.warmup, args.timeout)
            if workload["family"] == "wide":
                whole = ["parrant", "--manifest", rel(manifest_path), "--catalog", rel(catalog_path), "--select", f"+{model_name_value}", "--format", "json"]
                probe_unsupported("parrant_whole_model", "parrant", "whole_model_parse_included", whole, "Parrant whole-model selection returns text rather than the JSON column schema; not benchmarked", benchmark_dir, args.timeout, records)
        else:
            downstream = workload["selected_queries"]["downstream"]
            parrant_down = ["parrant", "--manifest", rel(manifest_path), "--catalog", rel(catalog_path), "--select", f"{model_name(downstream['model'])}.{downstream['column']}+", "--format", "json"]
            if probe_and_record("parrant_downstream", "parrant", "downstream_parse_included", parrant_down, parrant_downstream_valid, benchmark_dir, workload, args.timeout, records):
                measure(records[-1], parrant_down, benchmark_dir, args.runs, args.warmup, args.timeout)
        meta_artifact = benchmark_dir / "dbt-meta-lineage.json"
        meta_build = ["meta", "lineage", "build", "--manifest", rel(manifest_path), "--catalog", rel(catalog_path), "--output", rel(meta_artifact), "--json", "--no-compile"]
        if probe_and_record("dbt_meta_build", "dbt-meta", "build", meta_build, lambda p, w: meta_build_valid(p, w, meta_artifact), benchmark_dir, workload, args.timeout, records):
            measure(records[-1], meta_build, benchmark_dir, args.runs, args.warmup, args.timeout)
            meta_query = ["meta", "lineage", "column", "--artifact", rel(meta_artifact), "--json", f"{model_name_value}.{column}"]
            if probe_and_record("dbt_meta_upstream", "dbt-meta", "query", meta_query, meta_upstream_valid, benchmark_dir, workload, args.timeout, records):
                measure(records[-1], meta_query, benchmark_dir, args.runs, args.warmup, args.timeout)
            if workload["family"] == "fanout":
                downstream = workload["selected_queries"]["downstream"]
                meta_down = ["meta", "lineage", "downstream", "--artifact", rel(meta_artifact), "--json", f"{model_name(downstream['model'])}.{downstream['column']}"]
                if probe_and_record("dbt_meta_downstream", "dbt-meta", "query", meta_down, meta_downstream_valid, benchmark_dir, workload, args.timeout, records):
                    measure(records[-1], meta_down, benchmark_dir, args.runs, args.warmup, args.timeout)
            if workload["family"] == "wide":
                record_unsupported("dbt_meta_whole_model", "dbt-meta", "whole_model", "dbt-meta public CLI requires model.column; no whole-model query equivalent", records)
        for record in records:
            if record["status"] == "invalid":
                failures = True
        dbt_meta_index = metadata(meta_artifact) if meta_artifact.is_file() else {"path": rel(meta_artifact), "size_bytes": None, "sha256": None}
        run_metadata = {
            "schema_version": 1,
            "profile": profile_name,
            "workload": {"path": rel(workload_path), "sha256": sha256(workload_path)},
            "manifest": metadata(manifest_path),
            "catalog": metadata(catalog_path),
            "runs": args.runs,
            "warmup": args.warmup,
            "timeout_seconds": args.timeout,
            "boundaries": {
                "dlin_cache": {"size_bytes": None, "reason": "v1 uses --no-cache because cache behavior is not yet verified"},
                "parrant_cache": {"size_bytes": None, "reason": "no persistent cache; project parsing is included"},
                "peak_rss": {"size_bytes": None, "reason": "N/A; no low-overhead process-tree measurement"},
            },
            "dbt_meta_index": dbt_meta_index,
            "scenarios": records,
        }
        (benchmark_dir / "run_metadata.json").write_text(json.dumps(run_metadata, indent=2, sort_keys=True) + "\n")
        print(f"{profile_name}: {sum(record['status'] == 'valid' for record in records)}/{len(records)} valid")
    return 1 if failures else 0


if __name__ == "__main__":
    raise SystemExit(main())
