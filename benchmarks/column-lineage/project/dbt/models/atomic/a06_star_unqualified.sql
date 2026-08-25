-- depends_on: {{ ref('raw_orders') }}
select *
from {{ source('synthetic', 'raw_orders') }}
