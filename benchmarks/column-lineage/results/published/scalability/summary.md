# Synthetic scalability results

These are three-run, one-warmup measurements with a 120-second inner timeout. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.

## Run context

Tools: dlin 0.3.0-rc.1, parrant 0.21.1, dbt-meta 0.3.10. Runs/warmup/timeout: 3/1/120 seconds. Environment: Linux-5.15.167.4-microsoft-standard-WSL2-x86_64-with-glibc2.41, x86_64, AMD Ryzen 7 7735HS with Radeon Graphics.

## Reproduce

```sh
cd benchmarks/column-lineage
./scripts/regenerate_artifacts.sh
uv run --locked python scripts/preflight_tools.py
uv run --locked python scripts/run_scalability_benchmarks.py \
  --profile volume-1k \
  --profile volume-10k \
  --profile volume-100k \
  --profile wide-25 \
  --profile wide-50 \
  --profile wide-100 \
  --profile wide-200 \
  --profile deep-8 \
  --profile deep-16 \
  --profile deep-32 \
  --profile deep-64 \
  --profile fanout-8 \
  --profile fanout-32 \
  --profile fanout-128
uv run --locked python scripts/summarize_scalability_results.py
```

## volume

| Profile | background models | Columns | Edges | dlin_upstream | dlin_upstream_warm | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| volume-1k | 20 | 1150 | 1050 | 0.008047s | 0.005833s | 0.775623s | 1.310003s | 0.321353s |
| volume-10k | 200 | 10150 | 10050 | 0.016347s; 2.03x | 0.012056s; 2.07x | 2.550198s; 3.29x | 8.747536s; 6.68x | 0.389287s; 1.21x |
| volume-100k | 2000 | 100150 | 100050 | 0.078038s; 4.77x | 0.075740s; 6.28x | 24.393734s; 9.57x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_upstream_warm | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.025791s | 0.012935s | 0.027624s | 0.531231s | not_benchmarked | 0.752896s | 0.324415s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.051977s; 2.02x | 0.023898s; 1.85x | 0.054238s; 1.96x | 0.588158s; 1.11x | not_benchmarked | 0.899814s; 1.20x | 0.332608s; 1.03x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.136483s; 2.63x | 0.058209s; 2.44x | 0.148201s; 2.73x | 0.636443s; 1.08x | not_benchmarked | 1.686563s; 1.87x | 0.318893s; 0.96x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.486669s; 3.57x | 0.222538s; 3.82x | 0.512962s; 3.46x | 0.938523s; 1.47x | not_benchmarked | 5.230257s; 3.10x | 0.381411s; 1.20x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | dlin_upstream_warm | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.022468s | 0.010095s | 0.588110s | 0.835676s | 0.351503s |
| deep-16 | 16 | 425 | 400 | 0.040601s; 1.81x | 0.021430s; 2.12x | 0.640418s; 1.09x | 0.891692s; 1.07x | 0.334749s; 0.95x |
| deep-32 | 32 | 825 | 800 | 0.081844s; 2.02x | 0.036071s; 1.68x | 0.892511s; 1.39x | 1.121522s; 1.26x | 0.369524s; 1.10x |
| deep-64 | 64 | 1625 | 1600 | 0.173651s; 2.12x | 0.134790s; 3.74x | 0.897894s; 1.01x | 2.159215s; 1.93x | 0.462627s; 1.25x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | dlin_downstream_warm | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.013929s | 0.007252s | 0.919287s | 0.869951s | 0.379401s | 0.386778s |
| fanout-32 | 32 | 850 | 825 | 0.038274s; 2.75x | 0.012856s; 1.77x | 0.765322s; 0.83x | 1.137736s; 1.31x | 0.365593s; 0.96x | 0.361600s; 0.93x |
| fanout-128 | 128 | 3250 | 3225 | 0.125099s; 3.27x | 0.034354s; 2.67x | 1.291354s; 1.69x | 3.278969s; 2.88x | 0.440160s; 1.20x | 0.430789s; 1.19x |

## Method and limitations

- dlin scenarios without a suffix use `--no-cache` and include manifest loading, SQL analysis and the query. `_warm` scenarios reuse a cache filled once by the same query with `--refresh-cache`. OS cache drop is not performed.
- Parrant has no persistent cache; its timings include project parsing. dbt-meta build and query are separate scenarios; its end-to-end time is build plus query.
- Parrant is installed with SQLGlot 26.33.0. dbt-meta is installed with `sqlglot[c]` 30.33.0, the compiled SQLGlot its documentation recommends; the preflight checks both.
- A build-then-query tool can answer repeated queries from its artifact: dbt-meta's query alone can be faster than a dlin query without a cache. Compare dlin's warm-cache time with it for repeated use, and the end-to-end times for a single query.
- Peak RSS is N/A. Canva is excluded. `unsupported` means the tool has no such query; `not_benchmarked` means the tool runs it but its output cannot be validated (Parrant's whole-model selection returns text, not per-column JSON). Invalid results and timeouts are also shown as non-numeric statuses.
- volume-100k dbt_meta_build timed out; scenarios that depend on it are not-run and have null ratios.
