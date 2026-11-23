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
| volume-1k | 20 | 1150 | 1050 | 0.009827s | 0.005823s | 0.885222s | 3.020429s | 0.384170s |
| volume-10k | 200 | 10150 | 10050 | 0.044724s; 4.55x | 0.015544s; 2.67x | 3.060770s; 3.46x | 14.276568s; 4.73x | 0.426174s; 1.11x |
| volume-100k | 2000 | 100150 | 100050 | 0.088497s; 1.98x | 0.089155s; 5.74x | 27.149181s; 8.87x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_upstream_warm | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.030956s | 0.012863s | 0.030052s | 0.540897s | unsupported | 0.778049s | 0.307242s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.049238s; 1.59x | 0.020724s; 1.61x | 0.049009s; 1.63x | 0.587314s; 1.09x | unsupported | 1.090535s; 1.40x | 0.331526s; 1.08x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.139726s; 2.84x | 0.060200s; 2.90x | 0.143665s; 2.93x | 0.646744s; 1.10x | unsupported | 2.438612s; 2.24x | 0.343695s; 1.04x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.538854s; 3.86x | 0.262016s; 4.35x | 0.569011s; 3.96x | 0.931547s; 1.44x | unsupported | 7.463436s; 3.06x | 0.347853s; 1.01x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | dlin_upstream_warm | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.019669s | 0.010054s | 0.531934s | 0.724725s | 0.313194s |
| deep-16 | 16 | 425 | 400 | 0.035783s; 1.82x | 0.016479s; 1.64x | 0.634746s; 1.19x | 0.916313s; 1.26x | 0.320083s; 1.02x |
| deep-32 | 32 | 825 | 800 | 0.076775s; 2.15x | 0.031897s; 1.94x | 0.665727s; 1.05x | 1.157028s; 1.26x | 0.320205s; 1.00x |
| deep-64 | 64 | 1625 | 1600 | 0.151584s; 1.97x | 0.076885s; 2.41x | 0.810363s; 1.22x | 1.827201s; 1.58x | 0.326322s; 1.02x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | dlin_downstream_warm | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.014201s | 0.006532s | 0.568403s | 0.753852s | 0.316014s | 0.292735s |
| fanout-32 | 32 | 850 | 825 | 0.032499s; 2.29x | 0.011612s; 1.78x | 0.743300s; 1.31x | 1.215809s; 1.61x | 0.385005s; 1.22x | 0.352231s; 1.20x |
| fanout-128 | 128 | 3250 | 3225 | 0.135667s; 4.17x | 0.038715s; 3.33x | 1.477578s; 1.99x | 4.157436s; 3.42x | 0.479540s; 1.25x | 0.362365s; 1.03x |

## Method and limitations

- dlin scenarios without a suffix use `--no-cache` and include manifest loading, SQL analysis and the query. `_warm` scenarios reuse a cache filled once by the same query with `--refresh-cache`. OS cache drop is not performed.
- Parrant has no persistent cache; its timings include project parsing. dbt-meta build and query are separate scenarios; its end-to-end time is build plus query.
- Peak RSS is N/A. Canva is excluded. Whole-model unsupported scenarios and invalid/timeouts are shown as non-numeric statuses.
- volume-100k dbt_meta_build timed out; scenarios that depend on it are not-run and have null ratios.
