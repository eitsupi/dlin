{%- for src in var("order_sources") -%}
SELECT * FROM {{ ref('stg_' ~ src ~ '_orders') }}
{% if not loop.last %}UNION ALL{% endif %}
{%- endfor -%}
