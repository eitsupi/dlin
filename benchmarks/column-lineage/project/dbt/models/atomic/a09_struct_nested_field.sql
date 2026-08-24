-- depends_on: {{ ref('raw_events') }}
with event_structs as (
    select
        event_id,
        struct_pack(event_id := event_id, amount := numeric_value) as payload
    from {{ source('synthetic', 'raw_events') }}
)
select
    event_id,
    payload.amount as event_amount
from event_structs
