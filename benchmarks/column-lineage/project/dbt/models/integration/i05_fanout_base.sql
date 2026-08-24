-- depends_on: {{ ref('raw_orders') }}
select amount
from {{ source('synthetic', 'raw_orders') }}
