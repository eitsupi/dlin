-- depends_on: {{ ref('raw_orders') }}
select
    order_id,
    customer_id as account_id,
    cast(amount as decimal(12, 2)) as amount_decimal,
    amount * 2 as amount_double
from {{ source('synthetic', 'raw_orders') }}
