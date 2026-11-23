# Synthetic scalability results

These are three-run, one-warmup measurements with a 120-second inner timeout. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.

## Run context

Tools: dlin 0.3.0-rc.2, parrant 0.21.1, dbt-meta 0.3.10. Runs/warmup/timeout: 3/1/120 seconds. Environment: Linux-5.15.167.4-microsoft-standard-WSL2-x86_64-with-glibc2.41, x86_64, AMD Ryzen 7 7735HS with Radeon Graphics.

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
| volume-1k | 20 | 1150 | 1050 | 0.007492s | 0.006014s | 1.115308s | 1.327084s | 0.339674s |
| volume-10k | 200 | 10150 | 10050 | 0.012925s; 1.73x | 0.012207s; 2.03x | 2.495647s; 2.24x | 9.538371s; 7.19x | 0.464381s; 1.37x |
| volume-100k | 2000 | 100150 | 100050 | 0.083276s; 6.44x | 0.082528s; 6.76x | 25.174748s; 10.09x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_upstream_warm | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.012048s | 0.007154s | 0.016839s | 0.624431s | not_benchmarked | 0.824282s | 0.390332s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.018910s; 1.57x | 0.013532s; 1.89x | 0.023000s; 1.37x | 0.643397s; 1.03x | not_benchmarked | 1.090446s; 1.32x | 0.392545s; 1.01x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.036022s; 1.90x | 0.016559s; 1.22x | 0.036996s; 1.61x | 0.751878s; 1.17x | not_benchmarked | 1.825581s; 1.67x | 0.421972s; 1.07x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.050524s; 1.40x | 0.024868s; 1.50x | 0.063122s; 1.71x | 0.930807s; 1.24x | not_benchmarked | 5.392553s; 2.95x | 0.369244s; 0.88x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | dlin_upstream_warm | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.011333s | 0.008212s | 0.609147s | 0.829950s | 0.382406s |
| deep-16 | 16 | 425 | 400 | 0.020917s; 1.85x | 0.010962s; 1.33x | 0.668580s; 1.10x | 0.909379s; 1.10x | 0.417846s; 1.09x |
| deep-32 | 32 | 825 | 800 | 0.044724s; 2.14x | 0.022282s; 2.03x | 0.773363s; 1.16x | 1.124643s; 1.24x | 0.392436s; 0.94x |
| deep-64 | 64 | 1625 | 1600 | 0.113077s; 2.53x | 0.062594s; 2.81x | 0.937985s; 1.21x | 1.573464s; 1.40x | 0.366985s; 0.94x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | dlin_downstream_warm | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.012378s | 0.009485s | 0.614660s | 0.817926s | 0.407835s | 0.375949s |
| fanout-32 | 32 | 850 | 825 | 0.023851s; 1.93x | 0.011630s; 1.23x | 0.781886s; 1.27x | 1.067700s; 1.31x | 0.375307s; 0.92x | 0.376379s; 1.00x |
| fanout-128 | 128 | 3250 | 3225 | 0.057059s; 2.39x | 0.029600s; 2.55x | 1.165360s; 1.49x | 2.611746s; 2.45x | 0.351889s; 0.94x | 0.368657s; 0.98x |

## Method and limitations

- dlin scenarios without a suffix use `--no-cache` and include manifest loading, SQL analysis and the query. `_warm` scenarios reuse a cache filled once by the same query with `--refresh-cache`. OS cache drop is not performed.
- Parrant has no persistent cache; its timings include project parsing. dbt-meta build and query are separate scenarios; its end-to-end time is build plus query.
- Parrant is installed with SQLGlot 26.33.0. dbt-meta is installed with `sqlglot[c]` 30.33.0, the compiled SQLGlot its documentation recommends; the preflight checks both.
- A build-then-query tool answers repeated queries from its artifact, so for repeated use compare dbt-meta's query time with dlin's warm-cache time; for a single query, compare the end-to-end times.
- Peak RSS is N/A. Canva is excluded. `unsupported` means the tool has no such query; `not_benchmarked` means the tool runs it but its output cannot be validated (Parrant's whole-model selection returns text, not per-column JSON). Invalid results and timeouts are also shown as non-numeric statuses.
- volume-100k dbt_meta_build timed out; scenarios that depend on it are not-run and have null ratios.
