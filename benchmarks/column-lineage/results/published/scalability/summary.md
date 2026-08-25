# Synthetic scalability results

These are three-run, one-warmup measurements with a 120-second inner timeout. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.

## Run context

Tools: dlin 0.2.4, parrant 0.17.2, dbt-meta 0.3.8. Runs/warmup/timeout: 3/1/120 seconds. Environment: Linux-5.15.167.4-microsoft-standard-WSL2-x86_64-with-glibc2.41, x86_64, AMD Ryzen 7 7735HS with Radeon Graphics.

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

| Profile | background models | Columns | Edges | dlin_upstream | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| volume-1k | 20 | 1150 | 1050 | 0.009157s | 1.540295s | 3.548717s | 0.512070s |
| volume-10k | 200 | 10150 | 10050 | 0.020570s; 2.25x | 5.136934s; 3.34x | 25.531999s; 7.19x | 0.402738s; 0.79x |
| volume-100k | 2000 | 100150 | 100050 | 0.088917s; 4.32x | 37.393285s; 7.28x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.029077s | 0.031925s | 0.825821s | unsupported | 1.143254s | 0.354071s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.018255s; 0.63x | 0.017755s; 0.56x | 0.892789s; 1.08x | unsupported | 1.693516s; 1.48x | 0.353733s; 1.00x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.032028s; 1.75x | 0.032162s; 1.81x | 1.093827s; 1.23x | unsupported | 4.981859s; 2.94x | 0.379840s; 1.07x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.067828s; 2.12x | 0.067048s; 2.08x | 1.281098s; 1.17x | unsupported | 15.993353s; 3.21x | 0.381255s; 1.00x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.014178s | 0.984854s | 1.174343s | 0.409025s |
| deep-16 | 16 | 425 | 400 | 0.021917s; 1.55x | 0.981372s; 1.00x | 1.416096s; 1.21x | 0.403152s; 0.99x |
| deep-32 | 32 | 825 | 800 | 0.034861s; 1.59x | 1.172405s; 1.19x | 2.125483s; 1.50x | 0.440895s; 1.09x |
| deep-64 | 64 | 1625 | 1600 | 0.115333s; 3.31x | 1.424046s; 1.21x | 4.864931s; 2.29x | 0.410804s; 0.93x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.016572s | 1.049345s | 1.424150s | 0.354753s | 0.358406s |
| fanout-32 | 32 | 850 | 825 | 0.041397s; 2.50x | 1.034147s; 0.99x | 2.290984s; 1.61x | 0.423089s; 1.19x | 0.386344s; 1.08x |
| fanout-128 | 128 | 3250 | 3225 | 0.114055s; 2.76x | 1.777160s; 1.72x | 6.469463s; 2.82x | 0.358779s; 0.85x | 0.362651s; 0.94x |

## Method and limitations

- dlin uses `--no-cache`; its cold/warm cache distinction is not used here and OS cache drop is not performed.
- Parrant timings include project parsing. dbt-meta build and query are separate scenarios.
- Peak RSS is N/A. Canva is excluded. Whole-model unsupported scenarios and invalid/timeouts are shown as non-numeric statuses.
- volume-100k dbt-meta build timed out; its dependent query is not-run and has null ratios.
