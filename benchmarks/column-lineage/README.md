# Column-lineage benchmark

This directory contains a small real dbt-generated DuckDB correctness and quick benchmark layer, plus a separate synthetic scalability layer. The oracle is independent of tool output.

## Setup and small benchmark

From `benchmarks/column-lineage`:

```sh
uv sync --locked
uv tool install --reinstall dlin-cli==0.2.4
uv tool install --reinstall --with jinja2 parrant==0.17.2
uv tool install --reinstall dbt-meta==0.3.8
cargo install hyperfine --version 1.19.0 --locked
```

The comparison CLIs are intentionally installed as isolated `uv tool` environments rather than project dependencies: Parrant 0.17.2 requires `sqlglot>=26.8,<27`, while dbt-meta 0.3.8 requires `sqlglot>=30` and the pinned dbt environment resolves SQLGlot 30.x.

Run the small benchmark with these four commands:

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/preflight_tools.py
uv run --locked python scripts/run_benchmarks.py
uv run --locked python scripts/summarize_results.py
```

The default measurement uses three runs and one warmup. Set `BENCHMARK_RUNS` or `BENCHMARK_WARMUP` to override them. Results are written under `results/local/preflight/` and `results/local/benchmark/`.

The runner passes the same manifest and catalog bytes to every tool. Preflight checks 10 representative commands; it is not a full 16-case correctness score. dlin cold and warm describe only its tool cache and do not drop the OS cache. Parrant timings include project parsing. dbt-meta build is measured separately from queries. Canva 0.1.7b2 is excluded because its public CLI produces invalid or empty lineage for this fixture.

## Synthetic scalability layer

This layer uses the real artifacts as templates for deterministic workload shapes. It is not representative of real projects. Regenerate the real artifacts, then list or generate a profile:

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/generate_scalability_artifacts.py --list-profiles
uv run --locked python scripts/generate_scalability_artifacts.py --profile wide-25
```

Manual profiles require `--allow-manual`. Outputs are written under ignored `results/local/scalability/`.

## Provenance and license

The fixture structure was informed by [GnosisChain dbt-cerebro](https://github.com/gnosischain/dbt-cerebro), MIT License, Copyright 2024 hdser. This project does not copy its SQL, model names, or data. The fixture material is released under the MIT License; see [LICENSE](LICENSE) and [NOTICE](NOTICE).
