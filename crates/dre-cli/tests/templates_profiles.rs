//! `target.*`, `profile()` and `columns()` in templates.

mod common;

use common::{PLUGINS_YML, TestProject};

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: data.duckdb, catalog: client_a_catalog, schema: sales_dev, password: \"{{ env_var('DRE_SECRET_WH') }}\"}
      prod: {type: duckdb, path: data.duckdb, catalog: client_a_catalog, schema: sales_prod}
  cloud:
    target: prod
    targets:
      prod: {type: snowflake, account: acme, api_key: abc123, warehouse: small}
  shared:
    target: dev
    targets:
      dev: {type: duckdb, path: other.duckdb}
destinations:
  reports_s3:
    target: prod
    targets:
      prod: {type: local, bucket: acme-reports}
  shared:
    target: dev
    targets:
      dev: {type: local}
";

fn project(sql: &str) -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            (
                "reports/finance/monthly/monthly.yml",
                "queries: [q]\noutput:\n  format: csv\n  destination:\n    profile: reports_s3\n    path: \"out/{{ profile('reports_s3').bucket }}/{{ target.schema }}.csv\"\n",
            ),
            ("reports/finance/monthly/q.sql", sql),
        ],
        PROFILES,
    );
    p.duckdb(
        "data.duckdb",
        "create table orders (id integer, amount decimal(10,2), placed date, _etl_ts timestamp);\
         insert into orders values (1, 9.5, '2026-01-02', now());",
    );
    p
}

const SECRET: (&str, &str) = ("DRE_SECRET_WH", "s3cr3t-value");

fn compiled(p: &TestProject, args: &[&str]) -> String {
    let mut a = vec!["-s", "monthly"];
    a.extend_from_slice(args);
    p.dre_env("compile", &a, &[SECRET]).ok();
    p.read("target/compiled/monthly/default/q.sql")
}

#[test]
fn target_fields_follow_the_active_target() {
    let p = project(
        "select * from {{ target.catalog }}.{{ target.schema }}.orders -- {{ target.name }} {{ target.type }} {{ target.profile }}\n",
    );
    assert_eq!(
        compiled(&p, &[]),
        "select * from client_a_catalog.sales_dev.orders -- dev duckdb warehouse\n"
    );
    assert_eq!(
        compiled(&p, &["--target", "prod"]),
        "select * from client_a_catalog.sales_prod.orders -- prod duckdb warehouse\n"
    );
}

#[test]
fn profile_reads_any_profile_and_output_paths_can_use_both() {
    let p = project(
        "select '{{ profile('cloud').account }}', '{{ profile('reports_s3').type }}', '{{ profile('shared', role='destination').type }}'\n",
    );
    assert_eq!(compiled(&p, &[]), "select 'acme', 'local', 'local'\n");
    let r = p.dre_env("run", &["-s", "monthly", "--dry-run", "-s", "monthly"], &[SECRET]);
    r.ok();
    p.dre_env("validate", &["-s", "monthly"], &[SECRET])
        .ok()
        .says("out/acme-reports/sales_dev.csv");
}

#[test]
fn unknown_and_ambiguous_profiles_are_errors() {
    let p = project("select '{{ profile('nope').x }}'\n");
    p.dre_env("compile", &[], &[SECRET])
        .failed()
        .says("no destination profile `nope`");
    let p = project("select '{{ profile('shared').type }}'\n");
    p.dre_env("compile", &[], &[SECRET])
        .failed()
        .says("`shared` is both a source and a destination profile")
        .says("role='source'");
    let p = project("select '{{ target.colour }}'\n");
    p.dre_env("compile", &[], &[SECRET])
        .failed()
        .says("profile `warehouse` (target `dev`) has no field `colour`");
}

#[test]
fn secret_fields_are_refused_and_never_compiled() {
    // From a DRE_SECRET_* variable.
    let p = project("select '{{ target.password }}'\n");
    let r = p.dre_env("compile", &[], &[SECRET]);
    r.failed()
        .says("`password` of profile `warehouse` holds a secret");
    assert!(!r.stdout.contains("s3cr3t-value") && !r.stderr.contains("s3cr3t-value"));
    assert!(!p.path("target/compiled/monthly/default/q.sql").exists());
    // No `snowflake` plugin to ask, so a secret-looking name counts.
    let p = project("select '{{ profile('cloud').api_key }}'\n");
    let r = p.dre_env("compile", &[], &[SECRET]);
    r.failed().says("`api_key` of profile `cloud` holds a secret");
    assert!(!r.stdout.contains("abc123") && !r.stderr.contains("abc123"));
    // Other fields of the same profile are fine.
    let p = project("select '{{ profile('cloud').warehouse }}'\n");
    assert_eq!(compiled(&p, &[]), "select 'small'\n");
}

#[test]
fn columns_lists_a_relations_columns_once_per_file() {
    let p = project(
        "select {% for c in columns('orders') %}{{ c.name }} /* {{ c.type }} */{% if not loop.last %}, {% endif %}{% endfor %}\n\
         from orders\n\
         -- {{ columns('orders') | length }} {{ columns(ref('sub')) | map(attribute='name') | join(',') }}\n",
    );
    p.write(
        "reports/finance/monthly/sub.sql",
        "select id, amount * 2 as doubled from orders\n",
    );
    let out = compiled(&p, &[]);
    assert_eq!(
        out,
        "select id /* Int32 */, amount /* Decimal128(10, 2) */, placed /* Date32 */, _etl_ts /* Timestamp(µs) */\n\
         from orders\n\
         -- 4 id,doubled\n"
    );
    let log = p.read("logs/dre.log");
    assert_eq!(
        log.matches("from orders as _dre_cols where 1=0").count(),
        1,
        "one introspection query per relation:\n{log}"
    );
    // And the report still runs.
    p.dre_env("run", &["-s", "monthly"], &[SECRET]).ok();
}

#[test]
fn columns_of_a_missing_relation_is_a_clear_error() {
    let p = project("select {{ columns('no_such_table') }}\n");
    p.dre_env("compile", &[], &[SECRET])
        .failed()
        .says("`columns('no_such_table')`");
}

#[test]
fn a_run_renders_each_query_after_the_ones_before_it_ran() {
    let p = project(
        "select {% for c in columns('recent') %}{{ c.name }}{% if not loop.last %}, {% endif %}{% endfor %} from recent\n",
    );
    p.write(
        "reports/finance/monthly/monthly.yml",
        "queries:\n  - {query: setup, tab: false}\n  - q\noutput: {format: csv}\n",
    );
    p.write(
        "reports/finance/monthly/setup.sql",
        "create temp table recent as select id, amount from orders\n",
    );
    p.dre_env("run", &["-s", "monthly"], &[SECRET]).ok();
    assert_eq!(
        p.read("target/compiled/monthly/default/q.sql"),
        "select id, amount from recent\n"
    );
    assert_eq!(
        p.read("target/run/monthly/default/monthly.csv"),
        "id,amount\r\n1,9.50\r\n"
    );
}

#[test]
fn columns_sees_a_relation_recreated_by_an_earlier_query() {
    let cols = "select '{% for c in columns('t') %}{{ c.name }} {% endfor %}' as cols\n";
    let p = project(cols);
    p.write(
        "reports/finance/monthly/monthly.yml",
        "queries:\n  - {query: make, tab: false}\n  - q\n  - {query: remake, tab: false}\n  - q2\noutput: {format: csv}\n",
    );
    p.write(
        "reports/finance/monthly/make.sql",
        "create temp table t as select 1 as id\n",
    );
    p.write(
        "reports/finance/monthly/remake.sql",
        "create or replace temp table t as select 1 as id, 2 as extra\n",
    );
    p.write("reports/finance/monthly/q2.sql", cols);
    p.dre_env("run", &["-s", "monthly"], &[SECRET]).ok();
    assert_eq!(
        p.read("target/compiled/monthly/default/q.sql"),
        "select 'id ' as cols\n"
    );
    assert_eq!(
        p.read("target/compiled/monthly/default/q2.sql"),
        "select 'id extra ' as cols\n"
    );
}
