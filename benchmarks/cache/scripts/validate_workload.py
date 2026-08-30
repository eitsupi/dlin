#!/usr/bin/env python3
"""Validate a generated cache benchmark workload without third-party packages."""

from __future__ import annotations

import argparse
import hashlib
import json
from pathlib import Path


def sha256(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("workload", type=Path)
    args = parser.parse_args()
    root = args.workload
    metadata_path = root / "workload_metadata.json"
    if not metadata_path.is_file():
        parser.error(f"missing {metadata_path}")
    metadata = json.loads(metadata_path.read_text(encoding="utf-8"))
    if (
        not isinstance(metadata.get("model_count"), int)
        or metadata["model_count"] < 1
    ):
        parser.error("workload_metadata.json has an invalid model_count")
    if (
        not isinstance(metadata.get("macro_count"), int)
        or metadata["macro_count"] < 1
    ):
        parser.error("workload_metadata.json has an invalid macro_count")
    if (
        not isinstance(metadata.get("macro_file_count"), int)
        or metadata["macro_file_count"] < 1
    ):
        parser.error("workload_metadata.json has an invalid macro_file_count")
    expected = metadata.get("files", {})
    actual = {}
    for path in sorted(root.rglob("*")):
        if path.is_file() and path.name != metadata_path.name:
            actual[path.relative_to(root).as_posix()] = sha256(path)
    if actual != expected:
        parser.error("workload file hashes differ from workload_metadata.json")
    manifest_path = root / "manifest_project/target/manifest.json"
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    schema = manifest["metadata"]["dbt_schema_version"].rstrip("/")
    if not schema.endswith("/v12/manifest.json"):
        parser.error(f"unexpected manifest schema in {manifest_path}")
    if not manifest.get("nodes") or not manifest.get("sources"):
        parser.error(f"manifest is missing nodes or sources: {manifest_path}")
    if (root / "sql_project/target/manifest.json").exists():
        parser.error("SQL workload must not contain a target/manifest.json")
    macro_files = sorted((root / "sql_project/macros").glob("*.sql"))
    if (
        len(macro_files) != metadata["macro_file_count"]
        or macro_files[0].name != "benchmark.sql"
    ):
        parser.error("SQL workload macro files differ from workload_metadata.json")
    macro_source = "\n".join(
        path.read_text(encoding="utf-8") for path in macro_files
    )
    macro_definitions = macro_source.count("{% macro ")
    if macro_definitions != metadata["macro_count"]:
        parser.error(
            "macro definition count differs from workload_metadata.json: "
            f"{macro_definitions} != {metadata['macro_count']}"
        )
    if "{% macro benchmark_label(" not in macro_source:
        parser.error("SQL workload is missing the invoked benchmark_label macro")
    manifest_files = sorted(
        path.relative_to(root / "manifest_project").as_posix()
        for path in (root / "manifest_project").rglob("*")
        if path.is_file()
    )
    if manifest_files != ["target/manifest.json"]:
        parser.error(f"manifest_project contains files outside target/manifest.json: {manifest_files}")
    models = sorted((root / "sql_project/models").glob("*.sql"))
    if len(models) < 3 or not (root / "sql_project/models/orders_0002.sql").is_file():
        parser.error(
            "SQL workload must contain at least 3 models including "
            "models/orders_0002.sql for invalidation baselines"
        )
    vars_path = root / "sql_project/vars.yml"
    if not vars_path.is_file() or "benchmark_parent:" not in vars_path.read_text(encoding="utf-8"):
        parser.error("SQL workload is missing vars.yml benchmark_parent")
    final_model = models[-1]
    if "ref(var('benchmark_parent'))" not in final_model.read_text(encoding="utf-8"):
        parser.error("final SQL model does not exercise ref(var('benchmark_parent'))")
    if not models or not any(
        "{{ ref(" in path.read_text(encoding="utf-8") for path in models
    ):
        parser.error("SQL workload does not exercise ref() extraction")
    if metadata.get("profile") in {
        "runtime-local-macro-heavy",
        "runtime-local-macro-dense",
    }:
        dense = metadata["profile"] == "runtime-local-macro-dense"
        expected_reachable = 128 if dense else 3
        if metadata.get("local_macro_count") != 128:
            parser.error(f"{metadata['profile']} must declare 128 local macros")
        if metadata.get("reachable_local_macro_count") != expected_reachable:
            parser.error(
                f"{metadata['profile']} must declare {expected_reachable} reachable macros"
            )
        if metadata.get("runtime_uncertainty") != ["execute"]:
            parser.error(
                f"{metadata['profile']} must record execute runtime uncertainty"
            )
        if metadata.get("alternate_dependency_count") != metadata["model_count"]:
            parser.error(
                f"{metadata['profile']} must declare one alternate dependency per model"
            )
        if metadata.get("alternate_dependency_pattern") != (
            "runtime_local_alternate_{model_index:03d}"
        ):
            parser.error(f"{metadata['profile']} has an invalid alternate pattern")
        for model_index, model in enumerate(models):
            content = model.read_text(encoding="utf-8")
            if content.count("{% macro ") != metadata["local_macro_count"]:
                parser.error(f"unexpected local macro count in {model}")
            alternate_ref = f"runtime_local_alternate_{model_index:03d}"
            if content.count("runtime_local_alternate_") != 1:
                parser.error(
                    f"{model} must contain exactly one alternate dependency literal"
                )
            if content.count(f"ref('{alternate_ref}')") != 1:
                parser.error(f"{model} is missing its distinct alternate dependency")
            if dense:
                if any(f"runtime_macro_{index:03d}" not in content for index in range(128)):
                    parser.error(f"{model} is missing a dense runtime macro symbol")
                selector = "{% set runtime_selected = runtime_macro_000 if execute else runtime_macro_001 %}"
                invocation = "{{ runtime_macro_002(runtime_selected) }}"
                unused_names = [f"runtime_macro_{index:03d}" for index in range(3, 128)]
            else:
                if content.count("runtime_unused_") != 125:
                    parser.error(f"unexpected unused macro count in {model}")
                selector = "{% set runtime_selected = runtime_leaf_a if execute else runtime_leaf_b %}"
                invocation = "{{ runtime_invoke(runtime_selected) }}"
                unused_names = [f"runtime_unused_{index:03d}" for index in range(125)]
            if selector not in content or invocation not in content:
                parser.error(f"{model} is missing the runtime-local macro selector")
            for name in unused_names:
                opening = f"{{% macro {name}() %}}"
                if opening not in content:
                    parser.error(f"{model} is missing unused macro {name}")
                body = content.split(opening, 1)[1].split("{% endmacro %}", 1)[0]
                if "ref(" in body or "source(" in body:
                    parser.error(f"unused macro {name} in {model} contains graph calls")
    if metadata.get("profile") == "runtime-prefix-macro-heavy":
        if metadata.get("prefix_macro_count") != 128:
            parser.error("runtime-prefix-macro-heavy must declare 128 prefix macros")
        if metadata.get("reachable_prefix_macro_count") != 3:
            parser.error(
                "runtime-prefix-macro-heavy must declare 3 reachable prefix macros"
            )
        if metadata.get("unused_prefix_macro_count") != 125:
            parser.error(
                "runtime-prefix-macro-heavy must declare 125 unused prefix macros"
            )
        if metadata.get("runtime_uncertainty") != ["execute"]:
            parser.error(
                "runtime-prefix-macro-heavy must record execute runtime uncertainty"
            )
        reachable = {"benchmark_label", "prefix_dispatch", "prefix_choose"}
        unused = {f"prefix_unused_{index:03d}" for index in range(125)}
        for name in reachable:
            if macro_source.count(f"{{% macro {name}(") != 1:
                parser.error(f"prefix macro {name} must have exactly one definition")
        for name in unused:
            if macro_source.count(f"{{% macro {name}(") != 1:
                parser.error(f"prefix macro {name} must have exactly one definition")
        for index in range(125):
            ref_name = f"prefix_unused_ref_{index:03d}"
            source_name = f"prefix_unused_source_{index:03d}"
            if ref_name not in macro_source or source_name not in macro_source:
                parser.error(
                    f"unused prefix macro {index:03d} is missing graph literals"
                )
        for branch in ("true", "false"):
            if f"prefix_execute_{branch}" not in macro_source:
                parser.error(
                    f"prefix execute-{branch} branch is missing graph literals"
                )
        for model in models:
            content = model.read_text(encoding="utf-8")
            if "{% macro " in content:
                parser.error(f"{model} unexpectedly contains model-local macros")
            if "{{ prefix_dispatch() }}" not in content:
                parser.error(f"{model} is missing the prefix dispatch call")
            if any(name in content for name in unused):
                parser.error(f"{model} directly references an unused prefix macro")
    print(
        f"validated {metadata['profile']} workload "
        f"({metadata['model_count']} models, "
        f"macro_files={metadata['macro_file_count']}, "
        f"macros={metadata['macro_count']})"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
