with exploded as (
    select event_id, event_value
    from {{ ref('a08_unnest_value_table') }}
),
structured as (
    select event_id, event_amount
    from {{ ref('a09_struct_nested_field') }}
),
row_values as (
    select event_id, event_value as row_value
    from {{ ref('a11_row_value_alias') }}
)
select
    exploded.event_id,
    exploded.event_value,
    structured.event_amount,
    row_values.row_value,
    concat(
        exploded.event_value::varchar,
        '|',
        structured.event_amount::varchar,
        '|',
        row_values.row_value::varchar
    ) as combined_value
from exploded
left join structured using (event_id)
left join row_values using (event_id)
