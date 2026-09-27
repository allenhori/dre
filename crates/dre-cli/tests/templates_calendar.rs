//! Calendar values in templates: the run's timezone (UTC by default), `run.date` as a date you
//! can navigate, `run.now`, and the constructors, as the compiled SQL shows them.

mod common;

use chrono::Utc;
use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

fn project(extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        ("dependencies.yml", PLUGINS_YML),
        ("reports/finance/monthly/monthly.yml", "queries: [q]\n"),
    ];
    files.extend_from_slice(extra);
    if !files.iter().any(|(f, _)| *f == "dre_project.yml") {
        files.push((
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ));
    }
    TestProject::new(&files, DUCK_PROFILES)
}

fn compiled(p: &TestProject, args: &[&str], env: &[(&str, &str)]) -> String {
    let mut a = vec!["-s", "monthly"];
    a.extend_from_slice(args);
    p.dre_env("compile", &a, env).ok();
    p.read("target/compiled/monthly/default/q.sql")
}

#[test]
fn dates_render_as_literals_from_the_run_date() {
    let sql = "\
select '{{ run.date }}' as today,
  '{{ run.date.prev_month.start.date }}' as from_day,
  '{{ run.date.prev_month.end.date }}' as to_day,
  '{{ run.date.prev_month.start }}' as from_ts,
  '{{ run.date.prev_month.end }}' as to_ts,
  '{{ run.date.prev_month.next.start }}' as next_start,
  '{{ run.date.add(months=-1).month_end }}' as clamped,
  '{{ run.date.week_start }}' as wk,
  '{{ month_of(2024, 2).end.date }}' as feb,
  '{{ week_of(2026, 1).start.date }}' as w1,
  '{{ ('2026-07-04' | as_date).quarter_end }}' as q,
  '{{ run.date.yyyymmdd }}' as old_style
";
    let p = project(&[("reports/finance/monthly/q.sql", sql)]);
    // The harness runs with DRE_RUN_DATE=2026-01-25, a Sunday.
    assert_eq!(
        compiled(&p, &[], &[]),
        "\
select '2026-01-25' as today,
  '2025-12-01' as from_day,
  '2025-12-31' as to_day,
  '2025-12-01 00:00:00' as from_ts,
  '2025-12-31 23:59:59.999999' as to_ts,
  '2026-01-01 00:00:00' as next_start,
  '2025-12-31' as clamped,
  '2026-01-19' as wk,
  '2024-02-29' as feb,
  '2025-12-29' as w1,
  '2026-09-30' as q,
  '20260125' as old_style
"
    );
}

#[test]
fn week_settings_change_weeks() {
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\nweek_start: sunday\nweek_numbering: us\n",
        ),
        (
            "reports/finance/monthly/q.sql",
            "select '{{ run.date.week_start }}', {{ run.date.week }}, '{{ week_of(2026, 1).start.date }}'\n",
        ),
    ]);
    assert_eq!(compiled(&p, &[], &[]), "select '2026-01-25', 5, '2025-12-28'\n");
}

#[test]
fn bad_week_settings_are_validate_errors() {
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\nweek_start: tuesday\nweek_numbering: fiscal\n",
        ),
        ("reports/finance/monthly/q.sql", "select 1\n"),
    ]);
    p.dre("validate", &[])
        .failed()
        .says("`week_start` must be `monday` or `sunday`")
        .says("`week_numbering` must be `iso` or `us`");
}

#[test]
fn today_is_utc_by_default_whatever_the_host_zone() {
    let p = project(&[(
        "reports/finance/monthly/q.sql",
        "select '{{ run.date }}', '{{ run.timezone }}'\n",
    )]);
    // Kiritimati is UTC+14: its date differs from UTC's for most of each day.
    let before = Utc::now().date_naive();
    let out = compiled(&p, &[], &[("DRE_RUN_DATE", ""), ("TZ", "Pacific/Kiritimati")]);
    let after = Utc::now().date_naive();
    assert!(
        out == format!("select '{before}', 'UTC'\n") || out == format!("select '{after}', 'UTC'\n"),
        "{out}"
    );
}

#[test]
fn run_date_is_today_in_the_run_timezone() {
    let p = project(&[(
        "reports/finance/monthly/q.sql",
        "select '{{ run.date }}', '{{ run.timezone }}', '{{ run.now.timezone }}'\n",
    )]);
    // UTC+14 and UTC-12 are 26 hours apart: their dates always differ.
    for tz in ["Pacific/Kiritimati", "Etc/GMT+12"] {
        let zone: chrono_tz::Tz = tz.parse().unwrap();
        let before = Utc::now().with_timezone(&zone).date_naive();
        let out = compiled(&p, &["--timezone", tz], &[("DRE_RUN_DATE", "")]);
        let after = Utc::now().with_timezone(&zone).date_naive();
        assert!(
            out == format!("select '{before}', '{tz}', '{tz}'\n")
                || out == format!("select '{after}', '{tz}', '{tz}'\n"),
            "{tz}: {out}"
        );
    }
    // DRE_RUN_DATE still wins.
    assert_eq!(
        compiled(&p, &["--timezone", "Pacific/Kiritimati"], &[]),
        "select '2026-01-25', 'Pacific/Kiritimati', 'Pacific/Kiritimati'\n"
    );
}

#[test]
fn timezone_precedence_cli_env_schedule_report_folder_project() {
    let files = |report_tz: &str| {
        vec![
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\ntimezone: Europe/London\n\
                 reports:\n  finance:\n    +timezone: America/New_York\n"
                    .to_string(),
            ),
            (
                "schedules.yml",
                "- {name: sched, report: monthly, cron: \"0 6 * * *\", timezone: Asia/Tokyo}\n".to_string(),
            ),
            (
                "reports/finance/monthly/monthly.yml",
                format!("queries: [q]\n{report_tz}"),
            ),
            (
                "reports/finance/monthly/q.sql",
                "select '{{ run.timezone }}'\n".to_string(),
            ),
        ]
    };
    let make = |report_tz: &str| {
        let f = files(report_tz);
        let refs: Vec<(&str, &str)> = f.iter().map(|(a, b)| (*a, b.as_str())).collect();
        project(&refs)
    };
    let p = make("");
    assert_eq!(compiled(&p, &[], &[]), "select 'America/New_York'\n");
    let p = make("timezone: Australia/Sydney\n");
    assert_eq!(compiled(&p, &[], &[]), "select 'Australia/Sydney'\n");
    assert_eq!(
        compiled(&p, &[], &[("DRE_TIMEZONE", "Africa/Cairo")]),
        "select 'Africa/Cairo'\n"
    );
    assert_eq!(
        compiled(
            &p,
            &["--timezone", "Asia/Kolkata"],
            &[("DRE_TIMEZONE", "Africa/Cairo")]
        ),
        "select 'Asia/Kolkata'\n"
    );
    // A schedule's zone sits above the report's, below the environment.
    p.dre("run", &["--schedule", "sched", "--dry-run"]).ok();
    assert_eq!(
        p.read("target/compiled/monthly/default/q.sql"),
        "select 'Asia/Tokyo'\n"
    );

    // The project's own default applies when nothing nearer sets one.
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\ntimezone: Europe/London\n",
        ),
        ("reports/finance/monthly/q.sql", "select '{{ run.timezone }}'\n"),
    ]);
    assert_eq!(compiled(&p, &[], &[]), "select 'Europe/London'\n");
}

#[test]
fn invalid_timezones_are_reported() {
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\ntimezone: Mars/Base\n",
        ),
        ("reports/finance/monthly/q.sql", "select 1\n"),
    ]);
    p.dre("validate", &[])
        .failed()
        .says("dre_project.yml:3")
        .says("`Mars/Base` isn't a timezone");

    let p = project(&[("reports/finance/monthly/q.sql", "select 1\n")]);
    let r = p.dre("compile", &["--timezone", "Nowhere/City"]);
    assert_eq!(r.code, 2, "{r:?}");
    r.says("--timezone: `Nowhere/City` isn't a timezone");
    let r = p.dre_env("compile", &[], &[("DRE_TIMEZONE", "Nowhere/City")]);
    assert_eq!(r.code, 2, "{r:?}");
    r.says("DRE_TIMEZONE: `Nowhere/City` isn't a timezone");
    p.write(
        "reports/finance/monthly/monthly.yml",
        "queries: [q]\ntimezone: 42\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("report `monthly`: `timezone` must be a string");
}

#[test]
fn run_results_record_the_timezone() {
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\ntimezone: Australia/Sydney\n",
        ),
        ("reports/finance/monthly/q.sql", "select 1 as n\n"),
    ]);
    p.dre("run", &["-s", "monthly"]).ok();
    let r = p.json("target/run/monthly/default/run_results.json");
    assert_eq!(r["timezone"], "Australia/Sydney");
    assert_eq!(r["run_date"], "2026-01-25");
    let r = p.dre(
        "run",
        &["-s", "monthly", "--timezone", "UTC", "--log-format", "json"],
    );
    r.ok();
    assert!(
        r.stdout.contains("\"timezone\":\"UTC\"") || r.stderr.contains("\"timezone\":\"UTC\""),
        "{r:?}"
    );
}

#[test]
fn unknown_run_date_attributes_are_caught_by_validate() {
    let p = project(&[(
        "reports/finance/monthly/q.sql",
        "select '{{ run.date.prev_month.start }}', '{{ run.now }}', '{{ run.date.fortnight }}'\n",
    )]);
    p.dre("validate", &[])
        .failed()
        .says("`run.date.fortnight` isn't part of the run context");
}

#[test]
fn raise_error_stops_rendering_with_its_message() {
    let p = project(&[(
        "reports/finance/monthly/q.sql",
        "{% if var('x', 'bad') == 'bad' %}{{ raise_error('x must be set to something good') }}{% endif %}select 1\n",
    )]);
    p.dre("compile", &[])
        .failed()
        .says("x must be set to something good");
    p.dre("compile", &["--var", "x=good"]).ok();
}
