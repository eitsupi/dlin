select order_id, account_id, amount_decimal, amount_double
from {{ ref('i01_deep_01') }}
