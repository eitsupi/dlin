-- depends_on: {{ ref('raw_orders') }}
select
    date '2026-01-01' as report_date,
    ordered_at,
    ordered_at + interval '1 day' as next_day
from {{ source('synthetic', 'raw_orders') }}
