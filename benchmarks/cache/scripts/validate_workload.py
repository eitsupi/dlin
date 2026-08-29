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
    manifest_files = sorted(
        path.relative_to(root / "manifest_project").as_posix()
        for path in (root / "manifest_project").rglob("*")
        if path.is_file()
    )
    if manifest_files != ["target/manifest.json"]:
        parser.error(f"manifest_project contains files outside target/manifest.json: {manifest_files}")
    models = sorted((root / "sql_project/models").glob("*.sql"))
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
    print(f"validated {metadata['profile']} workload ({metadata['model_count']} models)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
