select '{{ run.profile }}', '{{ run.source_type }}',
  '{{ target.catalog }}', '{{ profile('pg', role='source').host }}'
