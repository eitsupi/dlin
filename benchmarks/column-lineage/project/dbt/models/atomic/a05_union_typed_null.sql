-- depends_on: {{ ref('raw_orders') }}
-- depends_on: {{ ref('raw_payments') }}
select
    order_id,
    amount
from {{ source('synthetic', 'raw_orders') }}
union all
select
    order_id,
    cast(null as double) as amount
from {{ source('synthetic', 'raw_payments') }}
