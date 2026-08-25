# Synthetic scalability results

These are three-run, one-warmup measurements with a 120-second inner timeout. They use synthetic artifacts derived from the real dbt fixture and do not represent real projects. No single winner is declared.

## Run context

Tools: dlin 0.2.4, parrant 0.17.2, dbt-meta 0.3.8. Runs/warmup/timeout: 3/1/120 seconds. Environment: Linux-5.15.167.4-microsoft-standard-WSL2-x86_64-with-glibc2.41, x86_64, AMD Ryzen 7 7735HS with Radeon Graphics, 16125345792 bytes memory.

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
| volume-1k | 20 | 1150 | 1050 | 0.008037s | 1.011006s | 2.600733s | 0.324942s |
| volume-10k | 200 | 10150 | 10050 | 0.016858s; 2.10x | 3.559181s; 3.52x | 23.527860s; 9.05x | 0.480708s; 1.48x |
| volume-100k | 2000 | 100150 | 100050 | 0.079769s; 4.73x | 29.322004s; 8.24x | timeout | not-run |

## wide

| Profile | width | Columns | Edges | dlin_upstream | dlin_whole_model | parrant_upstream | parrant_whole_model | dbt_meta_build | dbt_meta_upstream | dbt_meta_whole_model |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| wide-25 | 25 | 225 | 200 | 0.012831s | 0.014442s | 0.792939s | unsupported | 1.071268s | 0.344645s | unsupported |
| wide-50 | 50 | 450 | 400 | 0.017411s; 1.36x | 0.016418s; 1.14x | 0.864989s; 1.09x | unsupported | 1.989613s; 1.86x | 0.417926s; 1.21x | unsupported |
| wide-100 | 100 | 900 | 800 | 0.037615s; 2.16x | 0.039038s; 2.38x | 0.930081s; 1.08x | unsupported | 3.615152s; 1.82x | 0.335300s; 0.80x | unsupported |
| wide-200 | 200 | 1800 | 1600 | 0.066637s; 1.77x | 0.062858s; 1.61x | 1.123607s; 1.21x | unsupported | 11.153320s; 3.09x | 0.342574s; 1.02x | unsupported |

## deep

| Profile | depth | Columns | Edges | dlin_upstream | parrant_upstream | dbt_meta_build | dbt_meta_upstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| deep-8 | 8 | 225 | 200 | 0.016049s | 0.791434s | 1.008255s | 0.326323s |
| deep-16 | 16 | 425 | 400 | 0.025038s; 1.56x | 1.029533s; 1.30x | 1.306944s; 1.30x | 0.372818s; 1.14x |
| deep-32 | 32 | 825 | 800 | 0.072096s; 2.88x | 1.013096s; 0.98x | 1.669166s; 1.28x | 0.340745s; 0.91x |
| deep-64 | 64 | 1625 | 1600 | 0.078054s; 1.08x | 1.341478s; 1.32x | 3.131852s; 1.88x | 0.426979s; 1.25x |

## fanout

| Profile | branches | Columns | Edges | dlin_downstream | parrant_downstream | dbt_meta_build | dbt_meta_upstream | dbt_meta_downstream |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| fanout-8 | 8 | 250 | 225 | 0.015918s | 0.925005s | 1.301579s | 0.332028s | 0.306449s |
| fanout-32 | 32 | 850 | 825 | 0.029183s; 1.83x | 0.968826s; 1.05x | 1.682434s; 1.29x | 0.649053s; 1.95x | 0.628661s; 2.05x |
| fanout-128 | 128 | 3250 | 3225 | 0.237599s; 8.14x | 1.901773s; 1.96x | 6.238881s; 3.71x | 0.389932s; 0.60x | 0.418051s; 0.66x |

## Method and limitations

- dlin uses `--no-cache`; its cold/warm cache distinction is not used here and OS cache drop is not performed.
- Parrant timings include project parsing. dbt-meta build and query are separate scenarios.
- Peak RSS is N/A. Canva is excluded. Whole-model unsupported scenarios and invalid/timeouts are shown as non-numeric statuses.
- volume-100k dbt-meta build timed out; its dependent query is not-run and has null ratios.
