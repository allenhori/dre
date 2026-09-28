//! Lookups under `lookups/`: inlined or loaded by `ref()`, read by `lookup()`, checked by validate.

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

fn project(max_rows: Option<u32>, files: &[(&str, &str)]) -> TestProject {
    let project_yml = match max_rows {
        Some(n) => format!("name: acme\ndefault_profile: warehouse\nlookup_inline_max_rows: {n}\n"),
        None => "name: acme\ndefault_profile: warehouse\n".to_string(),
    };
    let mut all = vec![
        ("dre_project.yml", project_yml.as_str()),
        ("dependencies.yml", PLUGINS_YML),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, DUCK_PROFILES);
    p.duckdb(
        "data.duckdb",
        "create table sales as select * from (values ('AU', 10), ('NZ', 20), ('SG', 5)) t(country, amount);",
    );
    p
}

const COUNTRIES: &str =
    "code,name,active\nAU,Australia,yes\nNZ,New Zealand,no\nSG,\"Singapore, \"\"Lion\"\" City\",yes\n";
const REPORT: &str = "select s.country, c.name, c.active, s.amount\n\
                      from sales s join {{ ref('countries') }} c on c.code = s.country\n\
                      order by s.country\n";

#[test]
fn a_small_lookup_is_inlined_and_joins_like_a_table() {
    let p = project(
        None,
        &[
            ("lookups/countries.csv", COUNTRIES),
            ("lookups/countries.yml", "columns: {active: boolean}\n"),
            ("reports/r/r.yml", "queries: [q]\n"),
            ("reports/r/q.sql", REPORT),
        ],
    );
    p.dre("validate", &[]).ok().says("0 errors, 0 warnings");
    let r = p.dre("run", &["r"]);
    r.ok();
    assert!(!r.stdout.contains("Warning"), "{}", r.stdout);
    assert!(
        p.read("target/compiled/r/default/q.sql")
            .contains("(values\n  ('AU', 'Australia', true),")
    );
    assert_eq!(
        p.read("target/run/r/default/r.csv"),
        "country,name,active,amount\r\nAU,Australia,true,10\r\nNZ,New Zealand,false,20\r\nSG,\"Singapore, \"\"Lion\"\" City\",true,5\r\n"
    );
}

#[test]
fn a_large_lookup_is_bulk_loaded_into_a_temp_table_once() {
    let p = project(
        Some(2),
        &[
            ("lookups/countries.csv", COUNTRIES),
            ("reports/r/r.yml", "queries: [q, again]\n"),
            ("reports/r/q.sql", REPORT),
            (
                "reports/r/again.sql",
                "select count(*) as n from {{ ref('countries') }} c\n",
            ),
        ],
    );
    let r = p.dre("run", &["r"]);
    r.ok();
    // DuckDB has a bulk path, so there's nothing to warn about.
    assert!(!r.stdout.contains("Warning"), "{}", r.stdout);
    assert!(
        p.read("target/compiled/r/default/q.sql")
            .contains("join dre_lookup_countries c")
    );
    assert!(
        p.read("target/compiled/r/default/again.sql")
            .contains("from dre_lookup_countries c")
    );
}

#[test]
fn a_source_that_cant_load_gets_the_lookup_inlined_with_a_warning() {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme\ndefault_profile: fx\nlookup_inline_max_rows: 1\n",
            ),
            ("dependencies.yml", "plugins: [fixture, csv]\n"),
            ("lookups/countries.csv", COUNTRIES),
            ("reports/r/r.yml", "queries: [q]\n"),
            // The fixture only understands its own commands, so the ref is rendered but unused.
            ("reports/r/q.sql", "{% set c = ref('countries') %}rows 1\n"),
        ],
        "sources:\n  fx:\n    target: dev\n    targets:\n      dev: {type: fixture}\n",
    );
    p.dre("run", &["r"])
        .ok()
        .says("lookup `countries` has 3 rows; this source can't load it into a temp table, so it's inlined in the SQL. Data this size probably belongs in a table in the database");
}

#[test]
fn lookup_feeds_macros_and_every_file_type_reads() {
    let p = project(
        None,
        &[
            (
                "lookups/regions.json",
                r#"[{"code": "AU", "region": "APAC"}, {"code": "NZ", "region": "APAC"}]"#,
            ),
            (
                "lookups/tiers.jsonl",
                "{\"min\": 0, \"tier\": \"small\"}\n{\"min\": 15, \"tier\": \"large\"}\n",
            ),
            (
                "lookups/flags.yml",
                "columns: {enabled: boolean}\nrows:\n  - {flag: a, enabled: true}\n",
            ),
            (
                "macros/m.sql",
                "{% macro region_case(col) %}case {% for r in lookup('regions') %}when {{ col }} = '{{ r.code }}' then '{{ r.region }}' {% endfor %}else 'other' end{% endmacro %}\n",
            ),
            ("reports/r/r.yml", "queries: [q]\n"),
            (
                "reports/r/q.sql",
                "select s.country, {{ region_case('s.country') }} as region,\n\
                 (select max(t.tier) from {{ ref('tiers') }} t where cast(t.min as int) <= s.amount and t.tier = 'large') as big,\n\
                 (select count(*) from {{ ref('flags') }} f where f.enabled) as flags\n\
                 from sales s order by s.country\n",
            ),
        ],
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.read("target/run/r/default/r.csv"),
        "country,region,big,flags\r\nAU,APAC,,1\r\nNZ,APAC,large,1\r\nSG,other,,1\r\n"
    );
}

#[test]
fn validate_reports_lookup_problems() {
    let p = project(
        None,
        &[
            ("lookups/countries.csv", "code,pop\nAU,many\n"),
            ("lookups/countries.yml", "columns: {pop: integer}\n"),
            ("lookups/bad name.csv", "a\n1\n"),
            ("lookups/q.csv", "a\n1\n"),
            ("reports/r/r.yml", "queries: [q]\n"),
            ("reports/r/q.sql", "select * from {{ ref('countries') }} c\n"),
        ],
    );
    p.dre("validate", &[])
        .failed()
        .says("lookups/countries.csv: line 2, column `pop`: `many` isn't an integer")
        .says("lookup name `bad name` must be letters, digits and `_`")
        .says("lookup `q` has the same name as reports/r/q.sql");
}
