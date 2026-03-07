SELECT
    c.customer_id,
    c.first_name,
    c.last_name,
    c.email,
    co.name as country_name,
    COUNT(o.order_id) as order_count,
    SUM(o.total_amount) as lifetime_value
FROM {{ ref('stg_customers') }} c
LEFT JOIN {{ ref('orders') }} o ON c.customer_id = o.customer_id
LEFT JOIN {{ ref('countries') }} co ON c.country_code = co.code
GROUP BY 1, 2, 3, 4, 5
