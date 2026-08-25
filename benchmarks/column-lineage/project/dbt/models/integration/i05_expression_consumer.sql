select amount * 100 as scaled_amount
from {{ ref('i05_fanout_base') }}
