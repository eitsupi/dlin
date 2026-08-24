select
    1 as literal_value,
    current_date as today_value,
    count(*) as row_count
from (values (1)) as singleton(value)
