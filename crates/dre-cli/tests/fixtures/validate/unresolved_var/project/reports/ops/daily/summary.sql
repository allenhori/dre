select *
from {{ var('schema') }}.accounts
where fund = '{{ var('fund_id') }}'
  and region = '{{ var('region', 'AU') }}'
