# Synthetic scalability results

Each time is the median of ten runs after two warmups, with a 60-second timeout on every run, and each ratio compares the medians of consecutive profiles. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.

## Run context

Tools: dlin 0.3.0-rc.2, parrant 0.21.1, dbt-meta 0.3.10. Runs/warmup/timeout: 10/2/60 seconds. Environment: Linux-5.15.167.4-microsoft-standard-WSL2-x86_64-with-glibc2.41, x86_64, AMD Ryzen 7 7735HS with Radeon Graphics.

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
| volume-1k | 20 | 1150 | 1050 | 0.006159s | 0.004963s | 0.671533s | 1.224555s | 0.336573s |
| volume-10k | 200 | 10150 | 10050 | 0.012701s; 2.06x | 0.014786s; 2.98x | 2.357241s; 3.51x | 7.822864s; 6.39x | 0.371212s; 1.10x |
| volume-100k | 2000 | 100150 | 100050 | 0.076240s; 6.00x | 0.074587s; 5.04x | 24.514628s; 10.40x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_upstream_warm | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.011778s | 0.008105s | 0.011490s | 0.553009s | not_benchmarked | 0.738950s | 0.332710s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.015797s; 1.34x | 0.009083s; 1.12x | 0.018379s; 1.60x | 0.573694s; 1.04x | not_benchmarked | 0.884138s; 1.20x | 0.334235s; 1.00x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.028010s; 1.77x | 0.013119s; 1.44x | 0.032970s; 1.79x | 0.669007s; 1.17x | not_benchmarked | 1.602577s; 1.81x | 0.342903s; 1.03x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.047008s; 1.68x | 0.022275s; 1.70x | 0.054895s; 1.66x | 0.813441s; 1.22x | not_benchmarked | 4.893203s; 3.05x | 0.339338s; 0.99x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | dlin_upstream_warm | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.010581s | 0.006273s | 0.541961s | 0.736368s | 0.333303s |
| deep-16 | 16 | 425 | 400 | 0.018395s; 1.74x | 0.010346s; 1.65x | 0.589945s; 1.09x | 0.788199s; 1.07x | 0.339883s; 1.02x |
| deep-32 | 32 | 825 | 800 | 0.037271s; 2.03x | 0.019197s; 1.86x | 0.676518s; 1.15x | 0.938882s; 1.19x | 0.332720s; 0.98x |
| deep-64 | 64 | 1625 | 1600 | 0.087243s; 2.34x | 0.054455s; 2.84x | 0.822737s; 1.22x | 1.416833s; 1.51x | 0.340525s; 1.02x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | dlin_downstream_warm | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.008460s | 0.005121s | 0.548366s | 0.755769s | 0.350458s | 0.341921s |
| fanout-32 | 32 | 850 | 825 | 0.020852s; 2.46x | 0.011427s; 2.23x | 0.685644s; 1.25x | 0.962933s; 1.27x | 0.334387s; 0.95x | 0.335741s; 0.98x |
| fanout-128 | 128 | 3250 | 3225 | 0.059685s; 2.86x | 0.031244s; 2.73x | 1.143352s; 1.67x | 2.507205s; 2.60x | 0.347852s; 1.04x | 0.345718s; 1.03x |

## Method and limitations

- dlin scenarios without a suffix use `--no-cache` and include manifest loading, SQL analysis and the query. `_warm` scenarios reuse a cache filled once by the same query with `--refresh-cache`. OS cache drop is not performed.
- Parrant has no persistent cache; its timings include project parsing. dbt-meta build and query are separate scenarios; its end-to-end time is build plus query.
- Parrant is installed with SQLGlot 26.33.0. dbt-meta is installed with `sqlglot[c]` 30.33.0, the compiled SQLGlot its documentation recommends; the preflight checks both.
- A build-then-query tool answers repeated queries from its artifact, so for repeated use compare dbt-meta's query time with dlin's warm-cache time; for a single query, compare the end-to-end times.
- Peak RSS is N/A. Canva is excluded. `unsupported` means the tool has no such query; `not_benchmarked` means the tool runs it but its output cannot be validated (Parrant's whole-model selection returns text, not per-column JSON). Invalid results and timeouts are also shown as non-numeric statuses.
- The median keeps a single slow run, such as one disturbed by other work on the machine, from moving the reported time. `results.json` also keeps every run time with the mean, standard deviation, minimum and maximum.
- volume-100k dbt_meta_build timed out; scenarios that depend on it are not-run and have null ratios.
