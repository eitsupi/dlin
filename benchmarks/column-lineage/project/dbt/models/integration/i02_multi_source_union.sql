select order_id, amount
from {{ ref('a04_union_two_sources') }}
union all
select order_id, amount
from {{ ref('a05_union_typed_null') }}
