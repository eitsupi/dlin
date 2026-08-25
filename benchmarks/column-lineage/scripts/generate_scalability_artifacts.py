#!/usr/bin/env python3
"""Generate deterministic synthetic scalability artifacts from real dbt artifacts."""

from __future__ import annotations

import argparse
import copy
import hashlib
import json
import sys
from pathlib import Path


HERE = Path(__file__).resolve().parent.parent
PROFILE_PATH = HERE / "metadata" / "scalability_profiles.json"
ARTIFACT_DIR = HERE / "artifacts"
FIXED_TIME = 0.0
FIXED_METADATA = {
    "generated_at": "1970-01-01T00:00:00Z",
    "invocation_id": "column-lineage-scalability",
    "invocation_started_at": "1970-01-01T00:00:00Z",
    "run_started_at": "1970-01-01T00:00:00Z",
}
PROJECT = "column_lineage_correctness"


def relation(name: str) -> str:
    return f'"{PROJECT}"."main"."{name}"'


def model_id(name: str) -> str:
    return f"model.{PROJECT}.{name}"


def source_id(name: str) -> str:
    return f"source.{PROJECT}.synthetic.{name}"


def columns(width: int) -> list[str]:
    return [f"c{i:04d}" for i in range(1, width + 1)]


def projection_sql(names: list[str], upstream: str) -> str:
    return f"select {', '.join(names)} from {relation(upstream)}"


def manifest_columns(names: list[str]) -> dict:
    return {name: {"name": name, "description": ""} for name in names}


def catalog_columns(template: dict, names: list[str]) -> dict:
    return {
        name: {**copy.deepcopy(template), "name": name, "index": index}
        for index, name in enumerate(names, start=1)
    }


def stable_metadata(metadata: dict) -> dict:
    result = copy.deepcopy(metadata)
    result.update(FIXED_METADATA)
    return result


def empty_manifest(template: dict) -> dict:
    result = copy.deepcopy(template)
    result["metadata"] = stable_metadata(template["metadata"])
    result["nodes"] = {}
    result["sources"] = {}
    for key in result:
        if key not in {"metadata", "nodes", "sources"}:
            if not isinstance(template[key], dict):
                raise ValueError(f"dbt v12 top-level field is not a map: {key}")
            result[key] = {}
    return result


def empty_catalog(template: dict) -> dict:
    result = copy.deepcopy(template)
    result["metadata"] = stable_metadata(template["metadata"])
    result["nodes"] = {}
    result["sources"] = {}
    result["errors"] = []
    return result


def make_source(template: dict, name: str, names: list[str]) -> dict:
    result = copy.deepcopy(template)
    result.update(
        {
            "database": PROJECT,
            "schema": "main",
            "name": name,
            "resource_type": "source",
            "package_name": PROJECT,
            "path": f"generated/{name}.yml",
            "original_file_path": f"models/generated/{name}.yml",
            "unique_id": source_id(name),
            "fqn": [PROJECT, "synthetic", name],
            "source_name": "synthetic",
            "source_description": "",
            "identifier": name,
            "description": "",
            "columns": manifest_columns(names),
            "meta": {},
            "source_meta": {},
            "tags": [],
            "relation_name": relation(name),
            "created_at": FIXED_TIME,
            "doc_blocks": [],
        }
    )
    return result


def make_model(template: dict, name: str, names: list[str], upstream_uid: str, upstream_name: str, source_name: str | None, previous_name: str | None) -> dict:
    sql = projection_sql(names, upstream_name)
    result = copy.deepcopy(template)
    result.update(
        {
            "database": PROJECT,
            "schema": "main",
            "name": name,
            "alias": name,
            "resource_type": "model",
            "package_name": PROJECT,
            "path": f"generated/{name}.sql",
            "original_file_path": f"models/generated/{name}.sql",
            "unique_id": model_id(name),
            "fqn": [PROJECT, "generated", name],
            "checksum": {"name": "sha256", "checksum": hashlib.sha256(sql.encode()).hexdigest()},
            "description": "",
            "columns": manifest_columns(names),
            "meta": {},
            "tags": [],
            "relation_name": relation(name),
            "raw_code": sql,
            "created_at": FIXED_TIME,
            "refs": [] if previous_name is None else [{"name": previous_name, "package": None, "version": None}],
            "sources": [] if source_name is None else [["synthetic", source_name]],
            "depends_on": {"macros": [], "nodes": [upstream_uid]},
            "compiled_path": f"target/compiled/{PROJECT}/models/generated/{name}.sql",
            "compiled": True,
            "compiled_code": sql,
            "extra_ctes_injected": False,
            "extra_ctes": [],
            "doc_blocks": [],
            "constraints": [],
            "primary_key": [],
            "contract": {"enforced": False, "alias_types": True, "checksum": None},
            "patch_path": None,
            "build_path": None,
            "version": None,
            "latest_version": None,
            "deprecation_date": None,
            "time_spine": None,
        }
    )
    return result


def catalog_entry(template: dict, uid: str, name: str, kind: str, column_template: dict, names: list[str]) -> dict:
    result = copy.deepcopy(template)
    result.update(
        {
            "unique_id": uid,
            "metadata": {
                **copy.deepcopy(template["metadata"]),
                "database": PROJECT,
                "schema": "main",
                "name": name,
                "type": kind,
            },
            "columns": catalog_columns(column_template, names),
        }
    )
    return result


def profile_graph(profile: dict, manifest_template: dict, catalog_template: dict, model_template: dict, source_template: dict, catalog_node_template: dict, catalog_source_template: dict) -> tuple[dict, dict, dict]:
    name = profile["name"]
    family = profile["family"]
    params = profile["parameters"]
    width = params["width"]
    names = columns(width)
    manifest = empty_manifest(manifest_template)
    catalog = empty_catalog(catalog_template)
    source_catalog_column = next(iter(catalog_source_template["columns"].values()))
    model_catalog_column = next(iter(catalog_node_template["columns"].values()))
    model_names: list[str] = []
    source_names: list[str] = []
    selected_model: str
    selected_source: str
    downstream_names: list[str]
    downstream_model_name: str
    relation_edges: int
    resolved_edges: int
    path_edges: int
    background_edges: int
    selected_edges: int

    def add_source(source_name: str) -> str:
        uid = source_id(source_name)
        source_names.append(source_name)
        manifest["sources"][uid] = make_source(source_template, source_name, names)
        catalog["sources"][uid] = catalog_entry(catalog_source_template, uid, source_name, "BASE TABLE", source_catalog_column, names)
        manifest["parent_map"][uid] = []
        manifest["child_map"][uid] = []
        return uid

    def add_model(model_name: str, upstream_uid: str, upstream_name: str, source_name: str | None, previous_name: str | None) -> str:
        uid = model_id(model_name)
        model_names.append(model_name)
        manifest["nodes"][uid] = make_model(model_template, model_name, names, upstream_uid, upstream_name, source_name, previous_name)
        catalog["nodes"][uid] = catalog_entry(catalog_node_template, uid, model_name, "VIEW", model_catalog_column, names)
        manifest["parent_map"][uid] = [upstream_uid]
        manifest["child_map"].setdefault(upstream_uid, []).append(uid)
        manifest["child_map"][uid] = []
        return uid

    if family == "volume":
        background_source = f"{name}_background_source"
        query_source = f"{name}_query_source"
        background_uid = add_source(background_source)
        selected_source = query_source
        query_uid = add_source(query_source)
        background_count = params["background_models"]
        for index in range(1, background_count + 1):
            add_model(f"{name}_background_{index:05d}", background_uid, background_source, background_source, None)
        selected_model = f"{name}_query"
        add_model(selected_model, query_uid, query_source, query_source, None)
        downstream_names = []
        downstream_model_name = selected_model
        background_edges = background_count
        selected_edges = 1
        relation_edges = background_edges + selected_edges
        resolved_edges = relation_edges * width
        path_edges = 1
    elif family in {"wide", "deep"}:
        source_name = f"{name}_source"
        upstream_uid = add_source(source_name)
        depth = params["depth"]
        previous_name = None
        for index in range(1, depth + 1):
            model_name = f"{name}_m{index:03d}"
            add_model(model_name, upstream_uid, source_name if previous_name is None else previous_name, None if previous_name else source_name, previous_name)
            upstream_uid = model_id(model_name)
            previous_name = model_name
        model_names_chain = [f"{name}_m{index:03d}" for index in range(1, depth + 1)]
        selected_model = model_names_chain[-1]
        selected_source = source_name
        downstream_names = model_names_chain[1:]
        downstream_model_name = model_names_chain[0]
        background_edges = 0
        selected_edges = depth
        relation_edges = depth
        resolved_edges = depth * width
        path_edges = depth
    elif family == "fanout":
        source_name = f"{name}_source"
        source_uid = add_source(source_name)
        selected_model = f"{name}_base"
        add_model(selected_model, source_uid, source_name, source_name, None)
        downstream_model_name = selected_model
        branch_count = params["branches"]
        downstream_names = []
        for index in range(1, branch_count + 1):
            branch_name = f"{name}_branch_{index:04d}"
            add_model(branch_name, model_id(selected_model), selected_model, None, selected_model)
            downstream_names.append(branch_name)
        selected_source = source_name
        background_edges = 0
        selected_edges = 1 + branch_count
        relation_edges = selected_edges
        resolved_edges = selected_edges * width
        path_edges = 1
    else:
        raise ValueError(f"unsupported family: {family}")

    for uid in manifest["nodes"]:
        manifest["child_map"].setdefault(uid, [])
    selected_uid = model_id(selected_model)
    selected_source_uid = source_id(selected_source)
    expected_upstream_node_ids = []
    expected_upstream_edges = []
    current_uid = selected_uid
    while current_uid in manifest["nodes"]:
        parent_uid = manifest["parent_map"][current_uid][0]
        expected_upstream_node_ids.append(parent_uid)
        expected_upstream_edges.append({"child_id": current_uid, "parent_id": parent_uid, "column": names[0]})
        current_uid = parent_uid
    expected_targets = [model_id(item) for item in downstream_names]
    if family == "volume":
        downstream_query = {
            "applicable": False,
            "reason": "volume isolates preparation and constant upstream work; downstream scaling uses fanout profiles",
        }
    else:
        downstream_query = {
            "applicable": True,
            "model": model_id(downstream_model_name),
            "column": names[0],
            "expected_target_ids": expected_targets,
            "expected_target_count": len(expected_targets),
        }
    workload = {
        "schema_version": 1,
        "profile": name,
        "family": family,
        "manual": profile["manual"],
        "parameters": params,
        "model_count": len(model_names),
        "source_count": len(source_names),
        "total_declared_columns": (len(model_names) + len(source_names)) * width,
        "relation_edges": relation_edges,
        "resolved_column_edges": resolved_edges,
        "edge_definitions": {
            "relation_edges": "Each model depends directly on one source or model node.",
            "resolved_column_edges": "relation_edges multiplied by projected width; every edge carries every declared column.",
            "selected_path_edges": "The number of direct lineage edges from the selected upstream model to its terminal source.",
            "background_edges": "Volume background source-to-model edges, excluded from selected query work.",
            "selected_edges": "Edges on the selected query graph, including all fanout branches where applicable."
        },
        "workload": {"background_relation_edges": background_edges, "selected_relation_edges": selected_edges, "selected_query_work_separate": family == "volume"},
        "selected_queries": {
            "upstream": {
                "model": selected_uid,
                "column": names[0],
                "expected_terminal_source_ids": [selected_source_uid],
                "path_edges": path_edges,
                "expected_upstream_node_ids": expected_upstream_node_ids,
                "expected_upstream_edges": expected_upstream_edges,
            },
            "downstream": downstream_query,
            "whole_model": {
                "applicable": True,
                "model": selected_uid,
                "expected_output_column_count": width,
                "expected_terminal_source_ids": [selected_source_uid],
                "expected_resolved_path_edges": width * path_edges,
            }
        },
        "artifact": {"manifest_path": "manifest.json", "catalog_path": "catalog.json"}
    }
    return manifest, catalog, workload


def hash_file(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def validate(manifest: dict, catalog: dict, workload: dict) -> None:
    model_ids = set(manifest["nodes"])
    source_ids = set(manifest["sources"])
    all_ids = model_ids | source_ids
    if set(catalog["nodes"]) != model_ids or set(catalog["sources"]) != source_ids:
        raise ValueError("catalog coverage does not match manifest")
    if any(
        not isinstance(manifest[key], dict)
        for key in manifest
        if key not in {"metadata", "nodes", "sources"}
    ):
        raise ValueError("manifest v12 ancillary top-level field is not a map")

    expected_children = {uid: [] for uid in all_ids}
    actual_dependency_count = 0
    for uid, node in manifest["nodes"].items():
        dependencies = node["depends_on"]["nodes"]
        if len(dependencies) != 1 or dependencies[0] not in all_ids:
            raise ValueError(f"invalid dependency for {uid}")
        if manifest["parent_map"][uid] != dependencies:
            raise ValueError(f"parent map mismatch for {uid}")
        expected_children[dependencies[0]].append(uid)
        actual_dependency_count += len(dependencies)
        metadata = catalog["nodes"][uid]["metadata"]
        if (
            metadata["name"] != node["name"]
            or metadata["database"] != PROJECT
            or metadata["schema"] != "main"
            or metadata["type"] != "VIEW"
        ):
            raise ValueError(f"catalog model metadata mismatch for {uid}")
        if set(node["columns"]) != set(catalog["nodes"][uid]["columns"]):
            raise ValueError(f"catalog columns mismatch for {uid}")
    for uid, source in manifest["sources"].items():
        metadata = catalog["sources"][uid]["metadata"]
        if (
            metadata["name"] != source["name"]
            or metadata["database"] != PROJECT
            or metadata["schema"] != "main"
            or metadata["type"] != "BASE TABLE"
        ):
            raise ValueError(f"catalog source metadata mismatch for {uid}")
        if set(source["columns"]) != set(catalog["sources"][uid]["columns"]):
            raise ValueError(f"catalog source columns mismatch for {uid}")
    for uid in all_ids:
        if sorted(manifest["child_map"].get(uid, [])) != sorted(expected_children[uid]):
            raise ValueError(f"child map inverse mismatch for {uid}")
    if actual_dependency_count != workload["relation_edges"]:
        raise ValueError("relation edge count mismatch")
    if workload["model_count"] != len(model_ids) or workload["source_count"] != len(source_ids):
        raise ValueError("workload node counts mismatch")
    width = workload["parameters"]["width"]
    if workload["total_declared_columns"] != (len(model_ids) + len(source_ids)) * width:
        raise ValueError("declared column formula mismatch")
    if workload["resolved_column_edges"] != workload["relation_edges"] * width:
        raise ValueError("resolved edge formula mismatch")

    queries = workload["selected_queries"]
    upstream = queries["upstream"]
    current = upstream["model"]
    path_edges = 0
    traversed_nodes = []
    traversed_edges = []
    while current in model_ids:
        parents = manifest["parent_map"][current]
        if len(parents) != 1:
            raise ValueError("selected upstream path is not a single chain")
        current = parents[0]
        path_edges += 1
        traversed_nodes.append(current)
        traversed_edges.append({"child_id": upstream["model"] if path_edges == 1 else traversed_nodes[-2], "parent_id": current, "column": upstream["column"]})
    if (
        current not in source_ids
        or [current] != upstream["expected_terminal_source_ids"]
        or path_edges != upstream["path_edges"]
        or traversed_nodes != upstream["expected_upstream_node_ids"]
        or traversed_edges != upstream["expected_upstream_edges"]
    ):
        raise ValueError("selected upstream endpoint mismatch")
    downstream = queries["downstream"]
    if not downstream.get("applicable", True):
        if not downstream.get("reason"):
            raise ValueError("inapplicable downstream query needs a reason")
    else:
        reachable = set()
        pending = list(manifest["child_map"].get(downstream["model"], []))
        while pending:
            child = pending.pop()
            if child in reachable:
                continue
            reachable.add(child)
            pending.extend(manifest["child_map"].get(child, []))
        expected_targets = set(downstream["expected_target_ids"])
        if (
            downstream["model"] in expected_targets
            or expected_targets != reachable
            or downstream["expected_target_count"] != len(reachable)
        ):
            raise ValueError("selected downstream target mismatch")
    whole_model = queries["whole_model"]
    if (
        whole_model["expected_output_column_count"] != width
        or whole_model["expected_terminal_source_ids"] != upstream["expected_terminal_source_ids"]
        or whole_model["expected_resolved_path_edges"] != width * upstream["path_edges"]
    ):
        raise ValueError("whole-model expectation mismatch")


def write_json(path: Path, value: dict) -> None:
    path.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n")


def load_profile(name: str) -> dict:
    data = json.loads(PROFILE_PATH.read_text())
    for profile in data["profiles"]:
        if profile["name"] == name:
            return profile
    raise ValueError(f"unknown profile: {name}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    group = parser.add_mutually_exclusive_group(required=True)
    group.add_argument("--list-profiles", action="store_true")
    group.add_argument("--profile")
    parser.add_argument("--output-root", type=Path, default=HERE / "results/local/scalability")
    parser.add_argument("--allow-manual", action="store_true")
    args = parser.parse_args()
    profiles = json.loads(PROFILE_PATH.read_text())["profiles"]
    if args.list_profiles:
        for profile in profiles:
            print(f"{profile['name']}\t{profile['family']}\tmanual={profile['manual']}\t{json.dumps(profile['parameters'], sort_keys=True)}")
        return 0
    profile = load_profile(args.profile)
    if profile["manual"] and not args.allow_manual:
        print(f"profile {args.profile} is manual; pass --allow-manual", file=sys.stderr)
        return 2
    manifest_path = ARTIFACT_DIR / "manifest.json"
    catalog_path = ARTIFACT_DIR / "catalog.json"
    if not manifest_path.is_file() or not catalog_path.is_file():
        print("missing real artifacts; run ./scripts/regenerate_artifacts.sh first", file=sys.stderr)
        return 2
    real_manifest = json.loads(manifest_path.read_text())
    real_catalog = json.loads(catalog_path.read_text())
    model_template = next((node for node in real_manifest["nodes"].values() if node.get("resource_type") == "model"), None)
    source_template = next(iter(real_manifest["sources"].values()), None)
    catalog_node_template = next(iter(real_catalog["nodes"].values()), None)
    catalog_source_template = next(iter(real_catalog["sources"].values()), None)
    if not all((model_template, source_template, catalog_node_template, catalog_source_template)):
        print("real artifacts do not contain model/source templates", file=sys.stderr)
        return 2
    manifest, catalog, workload = profile_graph(
        profile,
        real_manifest,
        real_catalog,
        model_template,
        source_template,
        catalog_node_template,
        catalog_source_template,
    )
    output = args.output_root / profile["name"]
    output.mkdir(parents=True, exist_ok=True)
    manifest_out = output / "manifest.json"
    catalog_out = output / "catalog.json"
    workload_out = output / "workload.json"
    validate(manifest, catalog, workload)
    write_json(manifest_out, manifest)
    write_json(catalog_out, catalog)
    workload["artifact"].update({
        "manifest_size_bytes": manifest_out.stat().st_size,
        "manifest_sha256": hash_file(manifest_out),
        "catalog_size_bytes": catalog_out.stat().st_size,
        "catalog_sha256": hash_file(catalog_out),
    })
    if (
        workload["artifact"]["manifest_size_bytes"] != manifest_out.stat().st_size
        or workload["artifact"]["manifest_sha256"] != hash_file(manifest_out)
        or workload["artifact"]["catalog_size_bytes"] != catalog_out.stat().st_size
        or workload["artifact"]["catalog_sha256"] != hash_file(catalog_out)
    ):
        raise ValueError("artifact hash metadata mismatch")
    write_json(workload_out, workload)
    print(json.dumps({"profile": profile["name"], "output": str(output), "workload": workload}, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
