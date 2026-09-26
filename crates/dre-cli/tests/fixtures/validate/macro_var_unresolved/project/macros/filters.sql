{% macro region_filter() %}region = '{{ var('region') }}'{% endmacro %}
{% macro unused() %}{{ var('never_needed') }}{% endmacro %}
