//! Per-query connections and dbt-style sources: each query runs on its own connection's session
//! (one per connection, in YAML order), `source()` renders and decides the connection, `-s source:`
//! selects, and the manifest and `dre ls` show it all offline.

mod common;

use common::TestProject;

const PROFILES: &str = "\
connections:
  duck_a:
    targets:
      dev: {type: duckdb, path: a.duckdb}
      prod: {type: duckdb, path: a_prod.duckdb}
  duck_b:
    targets:
      dev: {type: duckdb, path: b.duckdb}
      prod: {type: duckdb, path: b_prod.duckdb}
destinations:
  prod_only:
    targets:
      prod: {type: local}
";

const SOURCES: &str = "\
version: 2
sources:
  - name: shop
    profile: duck_a
    schema: \"{{ 'sales' if target.name == 'prod' else 'main' }}\"
    tables:
      - name: orders
        identifier: raw_orders
        columns:
          - {name: id, data_type: integer}
          - {name: amount, data_type: \"decimal(10,2)\"}
      - name: refunds
  - name: crm
    profile: \"{{ var('crm_connection', 'duck_b') }}\"
    schema: main
    tables:
      - name: Accounts
    quoting: {identifier: true}
  - name: shared
    schema: main
    tables:
      - name: regions
";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        ("dre_project.yml", "name: acme\ndefault_profile: duck_a\n"),
        ("dependencies.yml", "plugins: [duckdb, csv, xlsx]\n"),
        ("sources/shop.yml", SOURCES),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb(
        "a.duckdb",
        "create table raw_orders (id integer, amount decimal(10,2)); insert into raw_orders values (1, 9.5), (2, 20);\
         create table regions as select 'apac' as region;",
    );
    p.duckdb(
        "b.duckdb",
        "create table \"Accounts\" (id integer, name varchar); insert into \"Accounts\" values (1, 'Acme');\
         create table regions as select 'emea' as region;",
    );
    p
}

#[test]
fn each_tab_runs_on_its_own_connection_in_yaml_order() {
    let p = project(&[
        (
            "reports/ops/mix/mix.yml",
            "queries:\n  - {query: setup_a, tab: false}\n  - orders\n  - accounts\n  - {query: regions_b, profile: duck_b}\n  - recent\noutput: {format: xlsx}\n",
        ),
        (
            "reports/ops/mix/setup_a.sql",
            "create temp table recent as select * from {{ source('shop', 'orders') }} where id > 1",
        ),
        (
            "reports/ops/mix/orders.sql",
            "select id, amount, '{{ connection.name }}' as conn from {{ source('shop', 'orders') }} order by id",
        ),
        (
            "reports/ops/mix/accounts.sql",
            "select name, '{{ connection.name }}' as conn from {{ source('crm', 'Accounts') }}",
        ),
        (
            "reports/ops/mix/regions_b.sql",
            "select region from {{ source('shared', 'regions') }}",
        ),
        // Back on duck_a: the temp table from the first query is still there.
        ("reports/ops/mix/recent.sql", "select count(*) as n from recent"),
    ]);
    p.dre("run", &["mix"]).ok();
    // The quoted identifier, and the unquoted two-part name.
    assert_eq!(
        p.read("target/compiled/mix/default/accounts.sql"),
        "select name, 'duck_b' as conn from main.\"Accounts\""
    );
    assert_eq!(
        p.read("target/compiled/mix/default/orders.sql"),
        "select id, amount, 'duck_a' as conn from main.raw_orders order by id"
    );
    let r = p.json("target/run/mix/default/run_results.json");
    assert_eq!(r["connections"], serde_json::json!(["duck_a", "duck_b"]));
    let tabs: Vec<(String, String)> = r["result_sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["name"].as_str().unwrap().to_string(),
                s["connection"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    assert_eq!(
        tabs,
        [
            ("orders".to_string(), "duck_a".to_string()),
            ("accounts".into(), "duck_b".into()),
            ("regions_b".into(), "duck_b".into()),
            ("recent".into(), "duck_a".into()),
        ]
    );
    assert_eq!(r["result_sets"][2]["rows"], 1);
    // Strict YAML order across connections, in the SQL log.
    let log = p.read("logs/dre.log");
    let at = |s: &str| {
        log.find(s)
            .unwrap_or_else(|| panic!("{s} not in the log:\n{log}"))
    };
    assert!(at("setup_a.sql") < at("orders.sql"));
    assert!(at("orders.sql") < at("accounts.sql"));
    assert!(at("accounts.sql") < at("regions_b.sql"));
    assert!(at("regions_b.sql") < at("recent.sql"));
}

#[test]
fn temp_tables_stay_on_their_own_connection() {
    let p = project(&[
        (
            "reports/ops/iso/iso.yml",
            "queries:\n  - {query: setup, tab: false}\n  - {query: elsewhere, profile: duck_b}\n",
        ),
        (
            "reports/ops/iso/setup.sql",
            "create temp table made_on_a as select 1 as n",
        ),
        ("reports/ops/iso/elsewhere.sql", "select * from made_on_a"),
    ]);
    p.dre("validate", &[])
        .ok()
        .says("warning[setup-on-other-connection]")
        .says("setup query `setup` (`tab: false`) runs on connection `duck_a`");
    p.dre("run", &["iso"]).failed().says("made_on_a");
}

#[test]
fn a_lookup_is_loaded_into_each_session_that_uses_it() {
    let p = project(&[
        ("lookups/codes.csv", "code\nx\ny\nz\n"),
        (
            "reports/ops/lk/lk.yml",
            "queries:\n  - a\n  - {query: b, profile: duck_b}\n",
        ),
        (
            "reports/ops/lk/a.sql",
            "select count(*) as n from {{ ref('codes') }}",
        ),
        (
            "reports/ops/lk/b.sql",
            "select count(*) as n from {{ ref('codes') }}",
        ),
    ]);
    p.write(
        "dre_project.yml",
        "name: acme\ndefault_profile: duck_a\nlookup_inline_max_rows: 1\n",
    );
    p.dre("run", &["lk"]).ok();
    let log = p.read("logs/dre.log");
    assert_eq!(
        log.matches("rows loaded through the plugin's `load` request")
            .count(),
        2,
        "loaded once per session:\n{log}"
    );
    assert_eq!(p.read("target/run/lk/default/lk_a.csv"), "n\r\n3\r\n");
    assert_eq!(p.read("target/run/lk/default/lk_b.csv"), "n\r\n3\r\n");
}

#[test]
fn source_fields_and_profile_values_render_for_the_target() {
    let p = project(&[
        ("reports/ops/o/o.yml", "queries: [q]\n"),
        (
            "reports/ops/o/q.sql",
            "select * from {{ source('shop', 'orders') }} join {{ source('shop', 'refunds') }} using (id)",
        ),
    ]);
    p.dre("compile", &["o"]).ok();
    assert_eq!(
        p.read("target/compiled/o/default/q.sql"),
        "select * from main.raw_orders join main.refunds using (id)"
    );
    p.dre("compile", &["o", "--target", "prod"]).ok();
    assert_eq!(
        p.read("target/compiled/o/default/q.sql"),
        "select * from sales.raw_orders join sales.refunds using (id)"
    );
    // A source's Jinja `profile` decides the query's connection.
    p.write("reports/ops/c/c.yml", "queries: [cq]\n");
    p.write(
        "reports/ops/c/cq.sql",
        "select '{{ connection.name }}' as c, * from {{ source('crm', 'Accounts') }}",
    );
    p.dre("compile", &["c"]).ok();
    assert!(
        p.read("target/compiled/c/default/cq.sql")
            .starts_with("select 'duck_b' as c")
    );
    p.dre("compile", &["c", "--var", "crm_connection=duck_a"]).ok();
    assert!(
        p.read("target/compiled/c/default/cq.sql")
            .starts_with("select 'duck_a' as c")
    );
}

#[test]
fn source_works_in_macros_refs_and_run_query() {
    let p = project(&[
        (
            "macros/m.sql",
            "{% macro orders_rel() %}{{ source('shop', 'orders') }}{% endmacro %}",
        ),
        ("reports/ops/m/m.yml", "queries: [mq]\n"),
        ("reports/shared/base.sql", "select id from {{ orders_rel() }}"),
        (
            "reports/ops/m/mq.sql",
            "{% set n = run_query('select count(*) as n from ' ~ source('crm', 'Accounts')) %}\
             select {{ n[0].n }} as accounts, count(*) as orders from {{ ref('base') }}",
        ),
    ]);
    // `crm` is on duck_b, but the query's connection comes from shop (duck_a): two connections
    // in one query is an error, even through run_query().
    p.dre("validate", &[])
        .failed()
        .says("error[connection-conflict]")
        .says("source `crm.Accounts` is on connection `duck_b`, but source `shop.orders` is on connection `duck_a`");
    // With the source on the query's connection, run_query() goes where its source is.
    p.write(
        "dre_project.yml",
        "name: acme\ndefault_profile: duck_a\nvars: {crm_connection: duck_a}\n",
    );
    p.duckdb(
        "a.duckdb",
        "create table \"Accounts\" (id integer); insert into \"Accounts\" values (7), (8);",
    );
    p.dre("run", &["m"]).ok();
    assert_eq!(p.read("target/run/m/default/m.csv"), "accounts,orders\r\n2,2\r\n");
    let m = p.json("target/manifest.json");
    assert_eq!(
        m["reports"]["m"]["bindings"][0]["queries"][0]["depends_on"]["sources"],
        serde_json::json!(["crm.Accounts", "shop.orders"])
    );
}

#[test]
fn a_source_the_parse_pass_cant_see_is_an_error_at_render() {
    let p = project(&[
        ("reports/ops/h/h.yml", "queries: [hq]\n"),
        (
            "reports/ops/h/hq.sql",
            "{% if run_query('select 1 as x')[0].x == 1 %}select * from {{ source('shop', 'orders') }}{% else %}select 1{% endif %}",
        ),
    ]);
    // validate compiles, and compiling runs `run_query()`, so it already sees the hidden source.
    p.dre("validate", &[])
        .failed()
        .says("was reached while rendering `hq`, but the parse pass didn't find it");
    p.dre("run", &["h"]).failed().says(
        "`source('shop', 'orders')` was reached while rendering `hq`, but the parse pass didn't find it",
    );
}

#[test]
fn select_by_source_in_run_compile_validate_and_ls() {
    let p = project(&[
        ("reports/ops/o/o.yml", "queries: [oq]\n"),
        (
            "reports/ops/o/oq.sql",
            "select * from {{ source('shop', 'orders') }}",
        ),
        ("reports/ops/r/r.yml", "queries: [rq]\n"),
        (
            "reports/ops/r/rq.sql",
            "select * from {{ source('shop', 'refunds') }}",
        ),
        ("reports/ops/n/n.yml", "queries: [nq]\n"),
        ("reports/ops/n/nq.sql", "select 1 as n"),
    ]);
    let names = |out: &str| -> Vec<String> {
        out.lines()
            .skip(1)
            .map(|l| l.split_whitespace().next().unwrap().to_string())
            .collect()
    };
    let r = p.dre("ls", &["-s", "source:shop"]);
    r.ok();
    assert_eq!(names(&r.stdout), ["o", "r"]);
    let r = p.dre("ls", &["-s", "source:shop.orders"]);
    r.ok();
    assert_eq!(names(&r.stdout), ["o"]);
    p.dre("ls", &["-s", "source:nope"])
        .failed()
        .says("no source `nope`");
    p.dre("ls", &["-s", "source:shop.nope"])
        .failed()
        .says("source `shop` has no table `nope`");
    p.dre("compile", &["-s", "source:shop.orders"]).ok();
    assert!(p.path("target/compiled/o/default/oq.sql").exists());
    assert!(!p.path("target/compiled/r").exists());
    p.dre("validate", &["-s", "source:shop.orders"])
        .ok()
        .says("Query  oq on duck_a (duckdb), reads shop.orders");
    p.dre("run", &["-s", "source:shop.orders"]).ok();
    assert!(p.path("target/run/o/default/o.csv").exists());
    assert!(!p.path("target/run/n").exists());

    // The sources listing flags the unused ones.
    let r = p.dre("ls", &["--resource-type", "source"]);
    r.ok();
    assert!(r.stdout.contains("shop.orders"), "{}", r.stdout);
    let line = |name: &str| {
        r.stdout
            .lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("{name} missing:\n{}", r.stdout))
            .to_string()
    };
    assert!(line("shop.orders").ends_with("o"), "{}", r.stdout);
    assert!(line("shop.orders").contains("duck_a"), "{}", r.stdout);
    assert!(line("crm.Accounts").ends_with("(unused)"), "{}", r.stdout);
    assert!(line("shared.regions").contains("(the query's)"), "{}", r.stdout);
    let r = p.dre(
        "ls",
        &[
            "--resource-type",
            "source",
            "-s",
            "source:shop",
            "--output",
            "json",
        ],
    );
    r.ok();
    let doc: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(doc["sources"].as_object().unwrap().len(), 1);
    assert_eq!(
        doc["sources"]["shop"]["tables"]["orders"]["used_by"],
        serde_json::json!(["o"])
    );
    assert_eq!(
        doc["sources"]["shop"]["tables"]["refunds"]["used_by"],
        serde_json::json!(["r"])
    );
}

#[test]
fn the_manifest_follows_the_inputs_and_needs_no_profiles() {
    let p = project(&[
        ("reports/ops/o/o.yml", "queries: [oq]\n"),
        (
            "reports/ops/o/oq.sql",
            "select * from {{ source('shop', 'orders') }}",
        ),
    ]);
    // Offline: no profiles.yml at all.
    let profiles = p.dir.path().join("profiles/profiles.yml");
    let saved = std::fs::read_to_string(&profiles).unwrap();
    std::fs::remove_file(&profiles).unwrap();
    let ls = |args: &[&str]| {
        let mut a = vec!["--output", "json"];
        a.extend_from_slice(args);
        let r = p.dre("ls", &a);
        r.ok();
        r.stdout
    };
    let dev = ls(&[]);
    assert_eq!(dev, ls(&[]), "the same inputs give the same bytes");
    let prod = ls(&["--target", "prod"]);
    assert_ne!(dev, prod);
    let dev: serde_json::Value = serde_json::from_str(&dev).unwrap();
    let prod: serde_json::Value = serde_json::from_str(&prod).unwrap();
    assert_eq!(dev["sources"]["shop"]["schema"], "main");
    assert_eq!(prod["sources"]["shop"]["schema"], "sales");
    let q = &dev["reports"]["o"]["bindings"][0]["queries"][0];
    assert_eq!(q["connection"], "duck_a");
    assert_eq!(q["depends_on"]["sources"], serde_json::json!(["shop.orders"]));
    assert_eq!(
        dev["reports"]["o"]["depends_on"]["sources"],
        serde_json::json!(["shop.orders"])
    );
    // --var reaches the sources section too, as it does the run.
    let with_var: serde_json::Value = serde_json::from_str(&ls(&["--var", "crm_connection=duck_a"])).unwrap();
    assert_eq!(dev["sources"]["crm"]["profile"], "duck_b");
    assert_eq!(with_var["sources"]["crm"]["profile"], "duck_a");

    std::fs::write(&profiles, saved).unwrap();
    // A query whose parse pass fails marks its report invalid; the rest still compiles.
    p.write("reports/ops/bad/bad.yml", "queries: [bq]\n");
    p.write(
        "reports/ops/bad/bq.sql",
        "select * from {{ source('shop', 'nope') }}",
    );
    p.dre("compile", &["o"]).ok();
    p.dre("compile", &["bad"])
        .failed()
        .says("source `shop` has no table `nope`");
    let m = p.json("target/manifest.json");
    assert_eq!(m["reports"]["bad"]["valid"], false);
    assert!(
        m["reports"]["bad"]["errors"][0]
            .as_str()
            .unwrap()
            .contains("source `shop` has no table `nope`")
    );
    assert_eq!(m["reports"]["o"]["valid"], true);
}

#[test]
fn a_dev_run_fails_on_a_prod_only_destination() {
    let p = project(&[
        (
            "reports/ops/d/d.yml",
            "queries: [dq]\noutput:\n  destination: {profile: prod_only, path: \"out/{{ target.name }}.csv\"}\n",
        ),
        ("reports/ops/d/dq.sql", "select 1 as n"),
    ]);
    p.duckdb("a_prod.duckdb", "select 1;");
    p.dre("run", &["d"])
        .failed()
        .says("destination `prod_only` has no `dev` entry (it has: prod)");
    assert!(!p.path("out/dev.csv").exists());
    assert!(!p.path("target/run").exists());
    p.dre("run", &["d", "--target", "prod"]).ok();
    assert_eq!(p.read("out/prod.csv"), "n\r\n1\r\n");
}

#[test]
fn validate_live_checks_declared_source_columns() {
    let p = project(&[
        ("reports/ops/o/o.yml", "queries: [oq]\n"),
        (
            "reports/ops/o/oq.sql",
            "select * from {{ source('shop', 'orders') }}",
        ),
    ]);
    p.dre("validate", &["--live"]).ok();
    p.write(
        "sources/shop.yml",
        &SOURCES.replace(
            "          - {name: amount, data_type: \"decimal(10,2)\"}\n",
            "          - {name: amount, data_type: varchar}\n          - {name: placed, data_type: date}\n          - {name: geo, data_type: geography}\n",
        ),
    );
    p.dre("validate", &["--live"])
        .failed()
        .says("source `shop.orders`: column `amount` is declared `varchar`, but main.raw_orders on connection `duck_a` returns Decimal128(10, 2)")
        .says("source `shop.orders`: declared column `placed` isn't in main.raw_orders on connection `duck_a`");
}

/// DuckDB and Postgres tabs in one workbook. Runs when `DRE_TEST_POSTGRES=host:port` points at a
/// server with user, password and database `dre` (CI's integration job).
#[test]
fn duckdb_and_postgres_tabs_in_one_workbook() {
    let Ok(server) = std::env::var("DRE_TEST_POSTGRES") else {
        eprintln!("skipped: set DRE_TEST_POSTGRES=host:port to run");
        return;
    };
    let (host, port) = server.split_once(':').unwrap();
    common::test_plugins(&["dre-plugin-postgres"]);
    let p = project(&[
        ("dependencies.yml", "plugins: [duckdb, postgres, csv, xlsx]\n"),
        (
            "reports/ops/wb/wb.yml",
            "queries:\n  - {query: pg_setup, tab: false, profile: pg}\n  - orders\n  - {query: pg_tab, profile: pg}\noutput: {format: xlsx}\n",
        ),
        (
            "reports/ops/wb/pg_setup.sql",
            "create temp table pg_made as select 'postgres' as engine, current_database() as db",
        ),
        (
            "reports/ops/wb/orders.sql",
            "select count(*) as n from {{ source('shop', 'orders') }}",
        ),
        ("reports/ops/wb/pg_tab.sql", "select engine, db from pg_made"),
    ]);
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        format!(
            "{PROFILES}  pg:\n    targets:\n      dev: {{type: postgres, host: {host}, port: {port}, user: dre, password: dre, database: dre, sslmode: disable}}\n"
        )
        .replace("destinations:\n  prod_only:\n    targets:\n      prod: {type: local}\n", "")
            + "destinations:\n  prod_only:\n    targets:\n      prod: {type: local}\n",
    )
    .unwrap();
    p.dre("run", &["wb"]).ok();
    let r = p.json("target/run/wb/default/run_results.json");
    assert_eq!(r["connections"], serde_json::json!(["pg", "duck_a"]));
    assert_eq!(r["result_sets"][0]["connection"], "duck_a");
    assert_eq!(r["result_sets"][1]["connection"], "pg");
    assert_eq!(r["result_sets"][1]["rows"], 1);

    // Declared columns of a Postgres source, checked by validate --live.
    p.write("reports/ops/mk/mk.yml", "queries: [mk]\nprofile: pg\n");
    p.write(
        "reports/ops/mk/mk.sql",
        "create table if not exists spec011_items as select 1::int as id, 'a'::text as label;\nselect * from spec011_items",
    );
    p.dre("run", &["mk"]).ok();
    p.write(
        "sources/pg.yml",
        "sources:\n  - name: pgsrc\n    profile: pg\n    schema: public\n    tables:\n      - name: spec011_items\n        columns:\n          - {name: id, data_type: integer}\n          - {name: label, data_type: text}\n",
    );
    p.write("reports/ops/rd/rd.yml", "queries: [rd]\n");
    p.write(
        "reports/ops/rd/rd.sql",
        "select * from {{ source('pgsrc', 'spec011_items') }}",
    );
    p.dre("validate", &["--live", "-s", "rd"]).ok();
    p.write(
        "sources/pg.yml",
        "sources:\n  - name: pgsrc\n    profile: pg\n    schema: public\n    tables:\n      - name: spec011_items\n        columns:\n          - {name: id, data_type: date}\n          - {name: gone}\n",
    );
    p.dre("validate", &["--live", "-s", "rd"])
        .failed()
        .says("column `id` is declared `date`, but public.spec011_items on connection `pg` returns Int32")
        .says("declared column `gone` isn't in public.spec011_items");
}

#[test]
fn quoting_uses_each_engines_quote_character() {
    common::test_plugins(&["dre-plugin-postgres"]);
    let p = project(&[
        ("dependencies.yml", "plugins: [duckdb, postgres, csv, xlsx]\n"),
        (
            "sources/pg.yml",
            "sources:\n  - name: pgq\n    profile: pg\n    schema: 'My \"Schema'\n    quoting: {schema: true, identifier: true}\n    tables:\n      - name: Orders\n",
        ),
        ("reports/ops/q/q.yml", "queries: [pq]\n"),
        (
            "reports/ops/q/pq.sql",
            "select * from {{ source('pgq', 'Orders') }}",
        ),
    ]);
    // Compiling asks the plugin for its quote character; nothing connects.
    std::fs::write(
        p.dir.path().join("profiles/profiles.yml"),
        PROFILES.replacen(
            "connections:\n",
            "connections:\n  pg:\n    targets:\n      dev: {type: postgres, host: localhost, port: 1, user: x, database: x}\n",
            1,
        ),
    )
    .unwrap();
    p.dre("compile", &["q"]).ok();
    assert_eq!(
        p.read("target/compiled/q/default/pq.sql"),
        "select * from \"My \"\"Schema\".\"Orders\""
    );
}

#[test]
fn a_var_that_breaks_one_report_doesnt_stop_another() {
    let p = project(&[
        (
            "macros/guard.sql",
            "{% macro period_ok(p) %}{% if p not in ['last_week', 'last_month'] %}{{ raise_error('var `period` must be last_week or last_month, not ' ~ p) }}{% endif %}{{ p }}{% endmacro %}",
        ),
        (
            "reports/ops/g/g.yml",
            "queries: [gq]\nvars: {period: last_week}\n",
        ),
        (
            "reports/ops/g/gq.sql",
            "select '{{ period_ok(var('period')) }}' as p, '{{ period(var('period')).start }}' as d",
        ),
        ("reports/ops/other/other.yml", "queries: [other_q]\n"),
        ("reports/ops/other/other_q.sql", "select 1 as n"),
    ]);
    p.dre("run", &["other", "--var", "period=fortnight"]).ok();
    // The guard's own message, not the error it guards against.
    p.dre("run", &["g", "--var", "period=fortnight"])
        .failed()
        .says("var `period` must be last_week or last_month, not fortnight");
    p.dre("run", &["g"]).ok();
    // The manifest marks it invalid for these inputs, and validate says why.
    p.dre("validate", &["--var", "period=fortnight"])
        .failed()
        .says("var `period` must be last_week or last_month, not fortnight");
}
