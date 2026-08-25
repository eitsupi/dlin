# Column-lineage correctness fixtures

This directory is a small, reproducible correctness corpus for dbt column-lineage tools. It is original synthetic material, contains no PII, and does not copy SQL or data from a public sample project. The SQL fixtures do not mention any lineage tool or encode tool-specific expected differences.

## Scope

The atomic fixtures cover direct projection, rename/cast/expression, source-free expressions, two-source unions, typed-NULL unions, unqualified and qualified stars, DuckDB list/struct `UNNEST`, nested struct fields, date tokens, and row-value aliases. Integration fixtures cover an eight-hop projection chain, a combined multi-source union, 50- and 127-column projections, a nested pipeline, and downstream fanout.

The oracle is independent from tool output: [oracle/cases.json](oracle/cases.json) records canonical source/target expectations. Transform labels are intentionally outside the v1 score. A tool that cannot represent a case should be recorded as unsupported by the caller; this corpus does not embed tool-specific rules.

## Reproduce

All Python and dbt commands go through the pinned uv project. `uvx` and unmanaged system Python are prohibited; use only the declared `pyproject.toml`/`uv.lock` environment and `uv run --locked`.

```sh
cd benchmarks/column-lineage
uv sync --locked
uv run --locked dbt --version --project-dir project/dbt
./scripts/regenerate_artifacts.sh
./scripts/validate_artifacts.sh
```

`regenerate_artifacts.sh` sets the required `COLUMN_LINEAGE_FIXTURE_DUCKDB_PATH` environment variable to a fixture-local absolute DuckDB path, removes that exact database before execution, and runs `dbt clean`, `dbt build`, and `dbt docs generate` from a clean dbt target. It copies the raw `manifest.json` and `catalog.json` into the ignored `artifacts/` directory. An exit trap removes the temporary database, target, logs, and script bytecode on both success and failure; it does not remove `.venv`. Each source-backed model has a SQL `depends_on` comment for its seed, while its executable `FROM` remains a source reference; this makes clean `dbt build` order the seeds before the models without changing the lineage SQL. There are no dbt packages, so no mutable `dbt deps` step is needed. The working DuckDB, target, logs, and run-local artifacts are ignored.

The fixture uses dbt-core 1.12.2 and dbt-duckdb 1.11.0, pinned in `pyproject.toml` and `uv.lock`. Dependencies are not vendored; their attribution and source URLs are in [NOTICE](NOTICE).

## Tool preflight

Run the clean artifact generator, then the thin three-tool preflight:

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/preflight_tools.py
```

The preflight uses the same manifest and catalog for dlin, Parrant, and dbt-meta. It runs representative I01 upstream and I05 downstream queries, stores raw outputs under `results/local/preflight/`, and writes a tool-specific validity summary to `status.json`.

For a quick first measurement, run the three steps below. The benchmark uses hyperfine with three runs and one warmup by default. Set `BENCHMARK_RUNS` or `BENCHMARK_WARMUP` to increase them.

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/preflight_tools.py
uv run --locked python scripts/run_benchmarks.py
uv run --locked python scripts/summarize_results.py
```

Results are written under `results/local/benchmark/`. dlin reports cold and warm cache scenarios. Parrant includes project parsing in each query measurement because it has no persistent cache. dbt-meta measures lineage build separately from queries against its generated artifact.

## Run-local artifacts

`artifacts/manifest.json` and `artifacts/catalog.json` are raw, ignored outputs of one setup run. The benchmark runner must pass those exact two files to every tool in that run and record their hashes in its run metadata. Runtime metadata in dbt artifacts is therefore part of the observed run input; this fixture does not commit golden artifacts. For branched cases, `expected_terminal_sources` is the required set; `expected_model_path` is an advisory representative path unless the case is a single-chain case such as I01. Consumers must not require one common path for every branch.

## Structural inspiration

The fixture structure was informed by [GnosisChain dbt-cerebro](https://github.com/gnosischain/dbt-cerebro), MIT License, Copyright 2024 hdser. It does not copy SQL, model names, or data.

## License

The fixture material is released under the MIT License. See [LICENSE](LICENSE) and [NOTICE](NOTICE).
