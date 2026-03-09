SELECT * FROM {{ source('raw', 'orders') }}
WHERE channel = 'retail'
