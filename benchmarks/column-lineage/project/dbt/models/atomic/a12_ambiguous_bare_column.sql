{{ config(materialized='ephemeral') }}

-- depends_on: {{ ref('raw_left') }}
-- depends_on: {{ ref('raw_right') }}

select
    shared_value
from {{ source('synthetic', 'raw_left') }} as left_side
join {{ source('synthetic', 'raw_right') }} as right_side using (id)
