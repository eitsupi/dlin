select
    order_id,
    account_id,
    amount_decimal,
    amount_double
from {{ ref('a02_rename_cast_expression') }}
