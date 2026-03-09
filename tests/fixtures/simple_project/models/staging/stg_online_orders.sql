SELECT * FROM {{ source('raw', 'orders') }}
WHERE channel = 'online'
