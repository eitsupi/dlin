-- depends_on: {{ ref('raw_events') }}
with rows_with_payload as (
    select
        struct_pack(event_id := event_id, value := numeric_value) as row_value
    from {{ source('synthetic', 'raw_events') }}
)
select
    row_value.event_id as event_id,
    row_value.value as event_value
from rows_with_payload
