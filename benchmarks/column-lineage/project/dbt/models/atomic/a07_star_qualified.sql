-- depends_on: {{ ref('raw_left') }}
-- depends_on: {{ ref('raw_right') }}
select l.*
from {{ source('synthetic', 'raw_left') }} as l
join {{ source('synthetic', 'raw_right') }} as r using (id)
