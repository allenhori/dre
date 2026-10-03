//! Runtime Jinja rendering and `run_query()`, end to end.

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

const LOCAL_FS: &str = "destinations:\n  local_fs:\n    targets:\n      dev: {type: local}\n";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\nvars: {level: project, region: AU}\n\
             reports:\n  finance:\n    +vars: {level: folder}\n",
        ),
        ("dependencies.yml", PLUGINS_YML),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, &format!("{DUCK_PROFILES}{LOCAL_FS}"));
    p.duckdb("data.duckdb", "create table regions as select * from (values ('apac', 10), ('emea', 20), ('amer', 30)) t(region, amount);");
    p
}

#[test]
fn sql_and_output_paths_share_one_context() {
    let p = project(&[
        (
            "reports/finance/monthly/monthly.yml",
            "queries: [q]\noutput:\n  destination:\n    profile: local_fs\n    path: \"out/{{ var('region') }}/{{ run.report }}-{{ run.date.yyyymmdd }}.csv\"\n",
        ),
        (
            "reports/finance/monthly/q.sql",
            "select '{{ run.report }}' as report, '{{ run.target }}' as target, '{{ connection.name }}' as profile,\n\
             '{{ run.date }}' as iso, '{{ run.date.ddmmyyyy }}' as dmy, '{{ run.date_format('%Y-W%V') }}' as week\n",
        ),
    ]);
    p.dre("run", &["monthly"]).ok();
    assert_eq!(
        p.read("out/AU/monthly-20260125.csv"),
        "report,target,profile,iso,dmy,week\r\nmonthly,dev,warehouse,2026-01-25,25012026,2026-W04\r\n"
    );
}

#[test]
fn var_precedence_is_cli_binding_report_folder_project_default() {
    let p = project(&[
        (
            "sets.yml",
            "client_a: {profile: warehouse, vars: {set_level: registry}}\n",
        ),
        (
            "reports/finance/monthly/monthly.yml",
            "queries: [q]\nvars: {report_level: report, level: report}\nsets:\n  - name: client_a\n    vars: {binding_level: binding}\n",
        ),
        (
            "reports/finance/monthly/q.sql",
            "select '{{ var('level') }}' as level, '{{ var('binding_level') }}' as b, '{{ var('set_level') }}' as s,\n\
             '{{ var('report_level') }}' as r, '{{ var('region') }}' as p, '{{ var('nowhere', 'dflt') }}' as d\n",
        ),
        ("reports/finance/folder_only/folder_only.yml", "queries: [fq]\n"),
        (
            "reports/finance/folder_only/fq.sql",
            "select '{{ var('level') }}' as level\n",
        ),
    ]);
    p.dre("run", &["monthly"]).ok();
    assert_eq!(
        p.read("target/run/monthly/client_a/monthly.csv"),
        "level,b,s,r,p,d\r\nreport,binding,registry,report,AU,dflt\r\n"
    );
    p.dre("run", &["monthly", "--var", "level=cli"]).ok();
    assert!(
        p.read("target/run/monthly/client_a/monthly.csv")
            .contains("\r\ncli,")
    );
    p.dre("run", &["folder_only"]).ok();
    assert_eq!(
        p.read("target/run/folder_only/default/folder_only.csv"),
        "level\r\nfolder\r\n"
    );
}

#[test]
fn env_var_renders_or_fails_the_binding() {
    let p = project(&[
        ("reports/ops/e/e.yml", "queries: [eq]\n"),
        (
            "reports/ops/e/eq.sql",
            "select '{{ env_var('DRE_TEST_REGION') }}' as region\n",
        ),
    ]);
    p.dre_env("run", &["e"], &[("DRE_TEST_REGION", "apac")]).ok();
    assert_eq!(p.read("target/run/e/default/e.csv"), "region\r\napac\r\n");
    p.dre_env("run", &["e"], &[("DRE_TEST_REGION", "")]).ok();
    // Unset entirely: validate already catches it, so the run refuses to start.
    p.dre("run", &["e"]).failed().says("DRE_TEST_REGION");
}

#[test]
fn macros_are_callable_from_any_query() {
    let p = project(&[
        (
            "macros/money.sql",
            "{% macro cents(col) %}({{ col }} * 100)::int{% endmacro %}\n",
        ),
        ("reports/ops/m/m.yml", "queries: [mq]\n"),
        (
            "reports/ops/m/mq.sql",
            "select region, {{ cents('amount') }} as cents from regions order by region\n",
        ),
    ]);
    p.dre("run", &["m"]).ok();
    assert_eq!(
        p.read("target/compiled/m/default/mq.sql"),
        "select region, (amount * 100)::int as cents from regions order by region\n"
    );
    assert_eq!(
        p.read("target/run/m/default/m.csv"),
        "region,cents\r\namer,3000\r\napac,1000\r\nemea,2000\r\n"
    );
}

#[test]
fn output_name_and_output_path_override_the_location_for_one_run() {
    let p = project(&[
        (
            "reports/ops/o/o.yml",
            "queries: [oq]\noutput:\n  destination: {profile: local_fs, path: out/regular.csv}\n",
        ),
        ("reports/ops/o/oq.sql", "select 1 as n\n"),
    ]);
    p.dre("run", &["o", "--output-name", "special.csv"]).ok();
    assert!(p.path("out/special.csv").exists() && p.path("target/run/o/default/special.csv").exists());
    assert!(!p.path("out/regular.csv").exists());
    p.dre("run", &["o", "--output-path", "elsewhere/x/final.csv"])
        .ok();
    assert!(p.path("elsewhere/x/final.csv").exists());
}

#[test]
fn run_query_feeds_rendering_on_the_bindings_own_session_even_in_dry_run() {
    let p = project(&[
        (
            "macros/pivot.sql",
            "{% macro region_columns() %}\
             {%- set res = run_query(\"select region from regions order by region\") -%}\
             {%- for row in res.rows -%}sum(case when region = '{{ row.region }}' then amount end) as {{ row[0] }}{{ ', ' if not loop.last }}{%- endfor -%}\
             {% endmacro %}\n",
        ),
        (
            "reports/ops/pivot/pivot.yml",
            "queries:\n  - {query: setup, tab: false}\n  - pq\n",
        ),
        // A temp table created earlier in the same Binding is visible to run_query().
        (
            "reports/ops/pivot/setup.sql",
            "create temp table extra as select 1 as x\n",
        ),
        (
            "reports/ops/pivot/pq.sql",
            "select {{ region_columns() }} from regions\n",
        ),
    ]);
    p.dre("run", &["pivot", "--dry-run"]).ok();
    assert_eq!(
        p.read("target/compiled/pivot/default/pq.sql"),
        "select sum(case when region = 'amer' then amount end) as amer, sum(case when region = 'apac' then amount end) as apac, \
         sum(case when region = 'emea' then amount end) as emea from regions\n"
    );
    assert!(
        !p.path("target/run/pivot/default/pivot.csv").exists(),
        "dry run must not execute the report"
    );
    p.dre("run", &["pivot"]).ok();
    assert_eq!(
        p.read("target/run/pivot/default/pivot.csv"),
        "amer,apac,emea\r\n30,10,20\r\n"
    );
}

#[test]
fn run_query_row_cap_is_enforced_and_configurable() {
    let p = project(&[
        ("reports/ops/cap/cap.yml", "queries: [cq]\n"),
        (
            "reports/ops/cap/cq.sql",
            "select 1 as n\n-- {{ run_query('select * from range(20)') | length }}\n",
        ),
        ("reports/ops/cap2/cap2.yml", "queries: [cq2]\n"),
        (
            "reports/ops/cap2/cq2.sql",
            "select {{ run_query('select * from range(20)', max_rows=50) | length }} as n\n",
        ),
    ]);
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nrun_query_max_rows: 5\n",
    );
    p.dre("run", &["cap"])
        .failed()
        .says("run_query() returned more than 5 rows")
        .says("max_rows=")
        .says("reports/ops/cap/cq.sql:2");
    p.dre("run", &["cap2"]).ok();
    assert_eq!(p.read("target/run/cap2/default/cap2.csv"), "n\r\n20\r\n");
}

#[test]
fn ref_inlines_another_sql_file_rendered_in_the_same_context() {
    let p = project(&[
        ("reports/finance/monthly/monthly.yml", "queries: [totals]\n"),
        // Shared SQL: used only through ref(), so neither file is an unmanaged report.
        (
            "reports/shared/base_regions.sql",
            "-- all regions\nselect region, amount from regions where amount >= {{ var('min', 0) }};\n",
        ),
        (
            "reports/shared/big_regions.sql",
            "select * from {{ ref('base_regions') }} r where r.region <> '{{ var('level') }}'\n",
        ),
        (
            "reports/finance/monthly/totals.sql",
            "with big as {{ ref('big_regions') }}\nselect count(*) as n, sum(amount) as total from big\n",
        ),
    ]);
    p.dre("validate", &[]).ok().says("0 warnings");
    p.dre("run", &["monthly", "--var", "min=15"]).ok();
    assert_eq!(
        p.read("target/run/monthly/default/monthly.csv"),
        "n,total\r\n2,50\r\n"
    );
    assert!(
        p.read("target/compiled/monthly/default/totals.sql")
            .contains("where amount >= 15")
    );
}

#[test]
fn macros_use_the_connection_to_generate_sql() {
    let p = project(&[
        (
            "macros/introspect.sql",
            "{% macro columns_of(table) %}\
             {%- set r = run_query(\"select column_name from information_schema.columns where table_name = '\" ~ table ~ \"' order by ordinal_position\") -%}\
             {{ r.column('column_name') | join(', ') }}\
             {%- endmacro %}\n\
             {% macro sum_per_value(table, col, measure) %}\
             {%- for v in run_query('select distinct ' ~ col ~ ' from ' ~ table ~ ' order by 1').column(0) -%}\
             sum(case when {{ col }} = '{{ v }}' then {{ measure }} end) as {{ v }}{{ ', ' if not loop.last }}\
             {%- endfor -%}\
             {%- endmacro %}\n",
        ),
        (
            "reports/ops/m/m.yml",
            "queries: [cols, pivot]\noutput: {format: csv}\n",
        ),
        (
            "reports/ops/m/cols.sql",
            "select '{{ columns_of('regions') }}' as cols\n",
        ),
        (
            "reports/ops/m/pivot.sql",
            "select {{ sum_per_value('regions', 'region', 'amount') }} from regions\n",
        ),
    ]);
    p.dre("run", &["m"]).ok();
    assert_eq!(
        p.read("target/compiled/m/default/pivot.sql"),
        "select sum(case when region = 'amer' then amount end) as amer, sum(case when region = 'apac' then amount end) as apac, sum(case when region = 'emea' then amount end) as emea from regions\n"
    );
    assert!(
        p.read("target/compiled/m/default/cols.sql")
            .contains("'region, amount'")
    );
}

#[test]
fn ref_problems_are_reported() {
    let p = project(&[
        ("reports/ops/m/m.yml", "queries: [a]\n"),
        ("reports/ops/m/a.sql", "select * from {{ ref('b') }} x\n"),
        ("reports/shared/b.sql", "select * from {{ ref('a') }} y\n"),
        ("reports/ops/u/u.yml", "queries: [u1]\n"),
        ("reports/ops/u/u1.sql", "select * from {{ ref('nope') }} t\n"),
    ]);
    p.dre("validate", &[])
        .failed()
        .says("reports/ops/u/u1.sql:1: `ref('nope')`: there's no `nope.sql` under reports/ and no lookup `nope` under lookups/")
        .says("reports/shared/b.sql:1: `ref()` cycle: a → b → a");
}

#[test]
fn ref_needs_one_statement_and_catches_dynamic_cycles_at_run_time() {
    let p = project(&[
        ("reports/ops/n/n.yml", "queries: [n1]\n"),
        ("reports/ops/n/n1.sql", "select * from {{ ref('two') }} t\n"),
        ("reports/shared/two.sql", "select 1 as x; select 2 as x\n"),
        // A name built at run time can't be checked statically.
        ("reports/ops/d/d.yml", "queries: [d1]\n"),
        ("reports/ops/d/d1.sql", "select * from {{ ref('d' ~ '1') }} t\n"),
    ]);
    p.dre("run", &["n"])
        .failed()
        .says("`ref('two')` needs reports/shared/two.sql to hold exactly one statement, but it has 2");
    p.dre("run", &["d"]).failed().says("`ref()` cycle: d1 → d1");
}
