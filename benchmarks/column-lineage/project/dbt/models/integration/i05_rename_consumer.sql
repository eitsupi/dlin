select amount as renamed_amount
from {{ ref('i05_fanout_base') }}
