select '{{ run.report }}' as r, '{{ run.dat }}' as d,
  '{{ run.date.yyyymmdd }}' as ok, '{{ run.date.yymm }}' as bad
from runs run where run.dat is not null
