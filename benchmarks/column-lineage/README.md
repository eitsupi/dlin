# Column-lineage benchmark

This directory contains a small real dbt-generated DuckDB correctness and quick benchmark layer, plus a separate synthetic scalability layer. The oracle is a manually specified independent oracle, separate from tool output.

## Setup and small benchmark

From `benchmarks/column-lineage`:

```sh
uv sync --locked
uv tool install --reinstall dlin-cli==0.3.0rc2
uv tool install --reinstall --with sqlglot==26.33.0 parrant==0.21.1
uv tool install --reinstall --with 'sqlglot[c]==30.33.0' dbt-meta==0.3.10
cargo install hyperfine --version 1.19.0 --locked
```

The comparison CLIs are intentionally installed as isolated `uv tool` environments rather than project dependencies: Parrant 0.21.1 requires `sqlglot>=26.8,<27`, while dbt-meta 0.3.10 requires `sqlglot>=30` and the pinned dbt environment resolves SQLGlot 30.x. SQLGlot affects both speed and results, so the install commands pin it. dbt-meta is installed with `sqlglot[c]`, the compiled SQLGlot that its documentation recommends; Parrant does not recommend an accelerator. The preflight checks each tool's SQLGlot version and whether it is compiled.

Run the small benchmark with these four commands:

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/preflight_tools.py
uv run --locked python scripts/run_benchmarks.py
uv run --locked python scripts/summarize_results.py
```

The default measurement uses three runs and one warmup. Set `BENCHMARK_RUNS` or `BENCHMARK_WARMUP` to override them. Results are written under `results/local/preflight/` and `results/local/benchmark/`.

The runner passes the same manifest and catalog bytes to every tool. Preflight checks 10 representative commands; it is not a full 16-case correctness score. In the fixture, the `raw_orders` seed and the source declared over it share one physical relation and `a02` depends on both; dlin reports `raw_orders.amount` as an `ambiguous_model` boundary listing both nodes, and the preflight accepts that or a known source column. dlin cold and warm describe only its tool cache and do not drop the OS cache. Parrant timings include project parsing. dbt-meta build is measured separately from queries. Canva 0.1.7b2 is excluded because its public CLI produces invalid or empty lineage for this fixture.

## Synthetic scalability layer

This layer uses the real artifacts as templates for deterministic workload shapes. It is not representative of real projects. Regenerate the real artifacts, then list or generate a profile:

```sh
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/generate_scalability_artifacts.py --list-profiles
uv run --locked python scripts/generate_scalability_artifacts.py --profile wide-25
```

Manual profiles require `--allow-manual`. Outputs are written under ignored `results/local/scalability/`.

After preflight, run a bounded scale measurement with `uv run --locked python scripts/run_scalability_benchmarks.py --profile wide-25 --runs 1 --warmup 1`; each tool's cache/preparation boundary is recorded in `run_metadata.json`. dlin is measured with `--no-cache` and, as `_warm` scenarios, with a cache filled once by the same query.

Publish the formal local scale results with `uv run --locked python scripts/summarize_scalability_results.py`.

## Checking how dlin scales

For dlin development, `scripts/measure_dlin_scaling.py` times one or more dlin binaries on the wide, deep and fan-out profiles, without a cache and with a warm cache, and prints the time ratio to the previous profile of the same family next to the ratio of their scales: the wide and deep profiles double at each step and the fan-out profiles grow fourfold. A time ratio at or below the scale ratio means the query time grows linearly or better. It also checks that every binary and cache mode returns the same JSON value (formatting and key order aside; array order counts), so a change meant only for speed can be checked not to change results:

```sh
cargo build --release --bin dlin
uv run --locked python scripts/measure_dlin_scaling.py \
  --dlin before=/path/to/previous/dlin \
  --dlin after=../../target/release/dlin
```

It generates missing profiles from the real artifacts, so run `./scripts/regenerate_artifacts.sh` first. It is not a published comparison and does not run the preflight.

## Provenance and license

The fixture structure was informed by [GnosisChain dbt-cerebro](https://github.com/gnosischain/dbt-cerebro), MIT License, Copyright 2024 hdser. This project does not copy its SQL, model names, or data. The fixture material is released under the MIT License; see [LICENSE](LICENSE) and [NOTICE](NOTICE).
