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
PROFILES = {
    "small": {"model_count": 64, "macro_file_count": 1, "macro_count": 1},
    "medium": {"model_count": 512, "macro_file_count": 1, "macro_count": 1},
    # Keep the model count comparable to the default workload while making the
    # effective macro prefix large enough to expose both per-model prefix
    # parsing and build_macro_prefix's cumulative source validation.
    "macro-heavy": {"model_count": 64, "macro_file_count": 16, "macro_count": 128},
}


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


def write_workload(root: Path, profile: str) -> None:
    profile_config = PROFILES[profile]
    count = profile_config["model_count"]
    macro_file_count = profile_config["macro_file_count"]
    macro_count = profile_config["macro_count"]
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
    macro_definitions = [
        "{% macro benchmark_label(value) %}{{ value }}{% endmacro %}"
    ]
    macro_definitions.extend(
        f"{{% macro benchmark_helper_{index:03d}(value) %}}"
        "{{ value }}{% endmacro %}"
        for index in range(1, macro_count)
    )
    (sql_root / "dbt_project.yml").write_text(dbt_project, encoding="utf-8")
    (sql_root / "macros").mkdir(exist_ok=True)
    for file_index in range(macro_file_count):
        start = macro_count * file_index // macro_file_count
        end = macro_count * (file_index + 1) // macro_file_count
        macro = "\n".join(macro_definitions[start:end]) + "\n"
        filename = "benchmark.sql" if file_index == 0 else f"benchmark_{file_index:03d}.sql"
        (sql_root / "macros" / filename).write_text(macro, encoding="utf-8")
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
        "profile": profile,
        "model_count": count,
        "macro_file_count": macro_file_count,
        "macro_count": macro_count,
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
            write_workload(Path(first), args.profile)
            write_workload(Path(second), args.profile)
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
    write_workload(args.output, args.profile)
    config = PROFILES[args.profile]
    print(
        f"generated {args.profile} workload "
        f"({config['model_count']} models, "
        f"macro_files={config['macro_file_count']}, "
        f"macros={config['macro_count']}) "
        f"at {args.output}"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
