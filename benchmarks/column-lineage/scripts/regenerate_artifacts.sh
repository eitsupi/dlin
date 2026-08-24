#!/usr/bin/env bash
set -euo pipefail

root_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
project_dir="$root_dir/project/dbt"
artifact_dir="$root_dir/artifacts"
fixture_db="$root_dir/project/column_lineage_correctness.duckdb"

cleanup() {
  local status=$?
  trap - EXIT
  rm -f -- "$fixture_db" || true
  rm -rf -- \
    "$project_dir/target" \
    "$project_dir/logs" \
    "$root_dir/scripts/__pycache__"
  exit "$status"
}

trap cleanup EXIT
rm -f -- "$fixture_db"
export COLUMN_LINEAGE_FIXTURE_DUCKDB_PATH="$fixture_db"

cd "$project_dir"
uv run --locked dbt clean
uv run --locked dbt build
uv run --locked dbt docs generate

mkdir -p "$artifact_dir"
cp -- target/manifest.json "$artifact_dir/manifest.json"
cp -- target/catalog.json "$artifact_dir/catalog.json"

cd "$root_dir"
uv run --locked python scripts/validate_oracle.py
uv run --locked python scripts/validate_artifacts.py
