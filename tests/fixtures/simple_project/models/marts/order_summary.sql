SELECT
    o.order_id,
    t.total_amount
FROM {{ ref('stg_orders') }} o
LEFT JOIN ({{ order_totals() }}) t ON o.order_id = t.order_id
