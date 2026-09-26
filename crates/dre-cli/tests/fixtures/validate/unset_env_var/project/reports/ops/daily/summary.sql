select '{{ env_var('DRE_TEST_SURELY_UNSET') }}' as a,
  '{{ env_var('DRE_TEST_SURELY_UNSET', 'x') }}' as b,
  '{{ env_var('DRE_TEST_IS_SET') }}' as c
