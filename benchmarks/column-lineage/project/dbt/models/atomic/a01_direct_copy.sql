-- depends_on: {{ ref('raw_orders') }}
select
    order_id
from {{ source('synthetic', 'raw_orders') }}
