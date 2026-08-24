#!/usr/bin/env bash
set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
artifact_dir="$root_dir/artifacts"

uv run --locked python "$root_dir/scripts/validate_oracle.py"
uv run --locked python "$root_dir/scripts/validate_artifacts.py"

test -s "$artifact_dir/manifest.json"
test -s "$artifact_dir/catalog.json"
jq -e '.nodes and .sources and .metadata' "$artifact_dir/manifest.json" >/dev/null
jq -e '.nodes and .sources and .metadata' "$artifact_dir/catalog.json" >/dev/null

manifest_models=$(jq '[.nodes|to_entries[]|select(.key|startswith("model."))]|length' "$artifact_dir/manifest.json")
catalog_models=$(jq '[.nodes|to_entries[]|select(.key|startswith("model."))]|length' "$artifact_dir/catalog.json")
test "$manifest_models" -ge 27
test "$catalog_models" -ge 26
printf 'artifacts valid: manifest_models=%s catalog_models=%s\n' "$manifest_models" "$catalog_models"
