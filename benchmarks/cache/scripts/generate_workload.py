#!/usr/bin/env python3
"""Generate deterministic, dlin-only cache benchmark fixtures."""

from __future__ import annotations

import argparse
import hashlib
import json
import shutil
import tempfile
from pathlib import Path


SUITE_ROOT = Path(__file__).resolve().parents[1]
PROFILES = {"small": 64, "medium": 512}


def manifest_node(
    project: str,
    name: str,
    depends_on: list[str],
    path: str,
    sql: str,
) -> dict:
    return {
        "unique_id": f"model.{project}.{name}",
        "name": name,
        "resource_type": "model",
        "depends_on": {"nodes": depends_on},
        "config": {"materialized": "view"},
        "description": f"Cache benchmark model {name}",
        "path": path,
        "original_file_path": path,
        "columns": {"id": {"name": "id"}, "amount": {"name": "amount"}},
        "compiled_code": sql,
        "database": "analytics",
        "schema": "main",
    }


def write_workload(root: Path, count: int) -> None:
    project = "cache_benchmark"
    sql_root = root / "sql_project"
    manifest_root = root / "manifest_project"
    (sql_root / "models").mkdir(parents=True, exist_ok=True)
    (manifest_root / "target").mkdir(parents=True, exist_ok=True)

    dbt_project = (
        "name: cache_benchmark\n"
        "version: '1.0'\n"
        "config-version: 2\n"
        "model-paths: [models]\n"
        "macro-paths: [macros]\n"
    )
    macro = """{% macro benchmark_label(value) %}{{ value }}{% endmacro %}\n"""
    (sql_root / "dbt_project.yml").write_text(dbt_project, encoding="utf-8")
    (sql_root / "macros").mkdir(exist_ok=True)
    (sql_root / "macros" / "benchmark.sql").write_text(macro, encoding="utf-8")
    parent_name = "orders" if count == 1 else f"orders_{count - 2:04d}"
    (sql_root / "vars.yml").write_text(
        f"vars:\n  benchmark_parent: {parent_name}\n", encoding="utf-8"
    )

    source_id = f"source.{project}.raw.orders"
    source = {
        "unique_id": source_id,
        "name": "orders",
        "source_name": "raw",
        "resource_type": "source",
        "description": "Benchmark source",
        "path": "models/schema.yml",
        "original_file_path": "models/schema.yml",
        "columns": {"id": {"name": "id"}, "amount": {"name": "amount"}},
        "database": "raw",
        "schema": "main",
        "identifier": "orders",
    }
    nodes: dict[str, dict] = {}
    previous = source_id
    for index in range(count):
        name = "orders" if index == 0 else f"orders_{index:04d}"
        path = f"models/{name}.sql"
        relation = (
            '"raw"."orders"'
            if index == 0
            else f'"main"."{("orders" if index == 1 else f"orders_{index - 1:04d}")}"'
        )
        if index == count - 1 and index > 0:
            sql = (
                f"select {{{{ benchmark_label('id') }}}}, amount + {index} as amount "
                "from {{ ref(var('benchmark_parent')) }}"
            )
        elif index == 0:
            sql = "select id, amount from {{ source('raw', 'orders') }}"
        else:
            previous_name = "orders" if index == 1 else f"orders_{index - 1:04d}"
            sql = (
                f"select {{{{ benchmark_label('id') }}}}, amount + {index} as amount "
                f"from {{{{ ref('{previous_name}') }}}}"
            )
        # The generated manifest contains compiled SQL, while SQL mode exercises
        # the Jinja source/ref extraction path above.
        compiled = (
            f"select id, amount{f' + {index}' if index else ''} as amount "
            f"from {relation}"
        )
        node = manifest_node(project, name, [previous], path, compiled)
        nodes[node["unique_id"]] = node
        previous = node["unique_id"]
        (sql_root / path).parent.mkdir(parents=True, exist_ok=True)
        (sql_root / path).write_text(sql + "\n", encoding="utf-8")

    manifest = {
        "metadata": {
            "project_name": project,
            "dbt_version": "1.12.0",
            "dbt_schema_version": "https://schemas.getdbt.com/dbt/manifest/v12/manifest.json",
            "adapter_type": "duckdb",
        },
        "nodes": nodes,
        "sources": {source_id: source},
        "exposures": {},
    }
    payload = json.dumps(manifest, indent=2, sort_keys=True) + "\n"
    (manifest_root / "target" / "manifest.json").write_text(payload, encoding="utf-8")

    metadata = {
        "profile": next(
            (name for name, size in PROFILES.items() if size == count), "custom"
        ),
        "model_count": count,
    }
    metadata["files"] = {}
    for file in sorted(root.rglob("*")):
        if file.is_file() and file.name != "workload_metadata.json":
            metadata["files"][file.relative_to(root).as_posix()] = hashlib.sha256(
                file.read_bytes()
            ).hexdigest()
    metadata_path = root / "workload_metadata.json"
    metadata_path.write_text(
        json.dumps(metadata, indent=2, sort_keys=True) + "\n", encoding="utf-8"
    )


def tree_digest(root: Path) -> str:
    digest = hashlib.sha256()
    for file in sorted(root.rglob("*")):
        if file.is_file():
            digest.update(file.relative_to(root).as_posix().encode())
            digest.update(file.read_bytes())
    return digest.hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, default=SUITE_ROOT / "workloads/default")
    parser.add_argument("--profile", choices=sorted(PROFILES), default="small")
    parser.add_argument(
        "--self-check",
        action="store_true",
        help="verify two independent generations are byte-identical",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="replace an existing generated workload containing workload_metadata.json",
    )
    args = parser.parse_args()
    if args.self_check:
        with tempfile.TemporaryDirectory() as first, tempfile.TemporaryDirectory() as second:
            write_workload(Path(first), PROFILES[args.profile])
            write_workload(Path(second), PROFILES[args.profile])
            if tree_digest(Path(first)) != tree_digest(Path(second)):
                parser.error("workload generation is not deterministic")
    args.output = args.output.resolve()
    if args.output.exists():
        if not args.output.is_dir():
            parser.error(f"output exists but is not a directory: {args.output}")
        if any(args.output.iterdir()):
            if not args.force or not (args.output / "workload_metadata.json").is_file():
                parser.error("output must be empty; use --force only for a previously generated workload")
            shutil.rmtree(args.output)
    args.output.mkdir(parents=True, exist_ok=True)
    write_workload(args.output, PROFILES[args.profile])
    print(f"generated {args.profile} workload ({PROFILES[args.profile]} models) at {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
