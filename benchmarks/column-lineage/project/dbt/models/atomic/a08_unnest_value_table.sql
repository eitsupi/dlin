-- depends_on: {{ ref('raw_events') }}
with event_lists as (
    select
        event_id,
        list_value(numeric_value, numeric_value + 1) as values_list
    from {{ source('synthetic', 'raw_events') }}
)
select
    event_id,
    value_table.value as event_value
from event_lists,
    unnest(values_list) as value_table(value)
