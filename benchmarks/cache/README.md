# dlin cache benchmark

This suite measures dlin's three persistent caches without changing production
code or comparing dlin with another tool. It is intentionally separate from
`benchmarks/column-lineage`, whose responsibility is external-tool correctness
and comparison.

## Setup

From the repository root, build the release binary and install hyperfine:

```sh
cargo build --release --locked
cargo install hyperfine --version 1.19.0 --locked
```

Python standard library is sufficient; if an isolated environment is desired,
use `uv` (do not install packages with `pip`). Generate and validate a fixture:

```sh
python3 benchmarks/cache/scripts/generate_workload.py \
  --output benchmarks/cache/workloads/default --profile small --self-check
python3 benchmarks/cache/scripts/validate_workload.py \
  benchmarks/cache/workloads/default
```

The `medium` profile is an explicit larger workload:

```sh
python3 benchmarks/cache/scripts/generate_workload.py \
  --output benchmarks/cache/workloads/medium --profile medium --self-check
```

The small profile has 64 models and medium has 512. Generation refuses to
overwrite a non-empty directory. To replace a workload previously generated
by this suite, pass `--force`; the marker file `workload_metadata.json` is
required as a safety check:

```sh
python3 benchmarks/cache/scripts/generate_workload.py \
  --output benchmarks/cache/workloads/default --profile small --self-check --force
```

## Benchmark and validation

Run probes and hyperfine timings with the release binary (the default binary
path is `target/release/dlin`):

```sh
python3 benchmarks/cache/scripts/run_benchmarks.py \
  --workload benchmarks/cache/workloads/default \
  --binary target/release/dlin --runs 3 --warmup 1
```

Pass `--summary-file <PATH>` to write the concise semantic-validation report
as Markdown (the runner also prints the same report to its log). CI passes
`$GITHUB_STEP_SUMMARY` so the probe results are visible in the job summary;
timing values are intentionally not included there.

Use `--skip-timing` for a fast probe-only check, or pass a debug binary
explicitly for functional development checks. Results are written under the
ignored `benchmarks/cache/results/` directory:

* `run_metadata.json` records git HEAD, binary version and SHA-256, platform,
  input sizes, commands, cache metadata, probe status, and timing paths.
* `hyperfine/*.json` contains raw timing output.
* `cache/<scenario>/` contains the observed persistent cache files.
* `invalidation/` contains isolated SQL-project copies and functional
  invalidation metadata.

Each scenario uses its own cache directory and observes
`extraction_cache.json`, `manifest_graph_cache.json`, and
`column_lineage_cache.json` as applicable. The runner also verifies dlin's
generated cache-directory `.gitignore` content. `persistent-cold` means a forced
miss using `--refresh-cache`; `persistent-warm` means reuse after a preparation
run. These labels distinguish persistent cache state only: the OS/filesystem
cache is not flushed. `--no-cache` is the no-persistent-I/O probe.

The runner checks semantic JSON equivalence between no-cache, persistent-cold,
and persistent-warm output. It also records cache size, SHA-256, and mtime in
nanoseconds and asserts that observed cache files are unchanged by the warm
probe and timed warm runs. This is combined benchmark evidence, not direct
proof of an internal cache hit; direct hit guarantees belong in production
unit/integration tests. Timing is informational and has no pass/fail threshold.

For SQL and manifest scenarios, timing uses the small-output `summary -o json`
command while semantic probes use `graph -o json`. This keeps graph rendering
out of cache timings while comparing the observable DAG. The column scenario
uses the same compiled-SQL column query for both.

The runner also performs three SQL invalidation baselines on isolated copies:
a size-changing single-file ref edit, a macro body edit that adds a rendered
dependency, and a `vars.yml` edit that changes the final model ref. Each must
produce an equivalent cached/no-cache result and a changed graph/cache state;
these are functional checks, not timing thresholds. The generated SQL project
uses `vars.yml` without duplicating `vars` in `dbt_project.yml`.

The SQL and manifest scenarios use the small-output `summary` command to
measure model-level DAG construction without timing a large graph renderer.
The manifest scenario does not measure column-lineage or MCP typed-`Manifest`
replacement. The column scenario separately exercises a compiled-SQL column
query.

## Design context

The SQL extraction workload includes macros and `ref()`/`source()` calls so
Minijinja extraction is exercised. Existing measurements show Minijinja is the
dominant model-lineage cost, so this suite informs cache changes without
assuming a more elaborate incremental graph. A future semantic SQL cache must
hash the exact effective macro-prefix bytes passed to rendering; it should not
silently replace that input with an order-independent macro-set hash.

The generator is deterministic and writes no production cache. Workloads,
results, and generated caches are ignored by git. Keep timing comparisons
local and reproducible; benchmark thresholds are deliberately not CI gates.
The generated SQL project contains only SQL-mode inputs (no target manifest),
while the manifest project contains only `target/manifest.json`; this keeps
the model-level cache scenarios isolated from freshness and filesystem scans.
