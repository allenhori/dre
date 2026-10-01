- The format for people. Recommend number formats for money and percentages, and `date_format`
  for dates, rather than formatting in SQL (REP-5).
- Tabs: one per `.sql` file, in YAML order, named with `tab_name` (REP-1).
- Totals rows and row formulas are declared in `columns:`; a row formula needs a placeholder
  column selected in SQL where it should go.
- A branded workbook goes through `template:`; see DRE's templates docs
  (https://github.com/get-dre/dre/blob/master/docs/templates.md).
- Values Excel can't hold exactly (more than 15 significant digits, dates before 1900) are
  written as text, with a warning.
