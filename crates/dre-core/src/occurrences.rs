//! Schedule occurrences: when each schedules.yml entry fires within a window, and the exact
//! command that runs each firing (`dre schedule ls`). Pure computation over the loaded project:
//! no profiles, plugins, network or writes. Storing, diffing and dispatching occurrences is the
//! caller's business.
//!
//! Every timing is expanded on naive wall-clock times, then each time is placed in the firing
//! timezone with DRE's own rules: a time in a DST gap fires at the first instant after the gap,
//! a time that happens twice fires once, on its first instance, and firings that land on the
//! same instant are one firing. That keeps DST out of the cron and rrule crates.
//!
//! The JSON document is a public, versioned contract (`docs/schedule-ls.md`,
//! `docs/schedule-ls.schema.json`): fields may be added in any release; removing or renaming one,
//! or changing what a hash covers, bumps [`FORMAT_VERSION`].

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use serde_json::{Map as JsonMap, Value as Json, json};
use sha2::{Digest, Sha256};

use crate::project::{Project, ScheduleEntry};
use crate::schedule;

/// The JSON document's format version (`dre_schedule_version`).
pub const FORMAT_VERSION: u64 = 1;
/// How far one call may reach.
pub const MAX_WINDOW_DAYS: i64 = 366;
/// The window when `--to` isn't given: five weeks, so a weekly refresh overlaps the last one.
pub const DEFAULT_WINDOW_DAYS: i64 = 35;
/// Most firings one rule may produce in a window.
const MAX_FIRINGS: usize = u16::MAX as usize;

/// Firings from `from` (inclusive) to `to` (exclusive).
#[derive(Debug, Clone, Copy)]
pub struct Window {
    pub from: DateTime<Utc>,
    pub to: DateTime<Utc>,
}

impl Window {
    /// Checks the order and the [`MAX_WINDOW_DAYS`] cap.
    pub fn new(from: DateTime<Utc>, to: DateTime<Utc>) -> Result<Self, String> {
        if to <= from {
            return Err(format!(
                "--to ({}) must be after --from ({})",
                rfc3339(to),
                rfc3339(from)
            ));
        }
        if to - from > Duration::days(MAX_WINDOW_DAYS) {
            return Err(format!(
                "the window from {} to {} is longer than {MAX_WINDOW_DAYS} days; ask for less, in several calls if needed",
                rfc3339(from),
                rfc3339(to)
            ));
        }
        Ok(Window { from, to })
    }
}

/// What `dre schedule ls` was asked for.
#[derive(Debug, Clone)]
pub struct Request {
    pub window: Window,
    /// `--schedule`: only these (empty: every schedule).
    pub schedules: Vec<String>,
    /// `-s`: only schedules whose Bindings include one of these reports.
    pub select: Option<String>,
    /// `--limit`: at most this many firings per schedule.
    pub limit: Option<usize>,
    /// `--split`: one occurrence per Binding instead of one per firing.
    pub split: bool,
}

pub use crate::run::rfc3339;

/// The timezone a schedule fires in: its own (or its timing's), then the project's, then UTC.
/// A report's timezone and the run-time overrides never move a firing.
pub fn firing_tz(project: &Project, e: &ScheduleEntry) -> Tz {
    e.timezone
        .as_ref()
        .or(project.timezone.as_ref())
        .and_then(|t| crate::dates::parse_tz(t).ok())
        .unwrap_or(Tz::UTC)
}

/// The Bindings a schedule runs, as `(report, set)`, in the project's report order.
pub fn bindings<'a>(project: &'a Project, name: &str) -> Vec<(&'a str, Option<&'a str>)> {
    project
        .reports
        .iter()
        .flat_map(|r| {
            r.bindings
                .iter()
                .filter(|b| b.schedules.iter().any(|s| s == name))
                .map(|b| (r.name.as_str(), b.set.as_deref()))
        })
        .collect()
}

/// Every instant `timing` (a schedule block: `cron`/`rrule`/`every`, `starting`, `at`,
/// `except`, `also`) fires in `tz` within `window`, in order.
pub fn expand(timing: &JsonMap<String, Json>, tz: Tz, window: &Window) -> Result<Vec<DateTime<Utc>>, String> {
    // Wide enough for any UTC offset; the result is cut to the window at the end.
    let lo = window.from.naive_utc() - Duration::days(2);
    let hi = window.to.naive_utc() + Duration::days(2);
    let s = |k: &str| timing.get(k).and_then(Json::as_str);
    let starting = s("starting").and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok());
    let at = s("at").and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok());
    let mut times = if let Some(expr) = s("cron") {
        cron_times(expr, lo, hi)?
    } else if let Some(rule) = s("rrule") {
        rule_times(rule, starting, at, lo, hi)?
    } else if let Some(every) = timing.get("every").and_then(Json::as_object) {
        let (unit, n) = every.iter().next().ok_or("`every` has no unit")?;
        let freq = match unit.as_str() {
            "days" => "DAILY",
            "weeks" => "WEEKLY",
            "months" => "MONTHLY",
            u => return Err(format!("unknown `every` unit `{u}`")),
        };
        if starting.is_none() {
            return Err("`every` needs `starting`".into());
        }
        let rule = format!("FREQ={freq};INTERVAL={}", n.as_u64().unwrap_or(1));
        rule_times(&rule, starting, at, lo, hi)?
    } else {
        return Err("a schedule needs one of `cron`, `every` or `rrule`".into());
    };
    let dates = |k: &str| -> BTreeSet<NaiveDate> {
        timing
            .get(k)
            .and_then(Json::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| {
                d.as_str()
                    .and_then(|d| NaiveDate::parse_from_str(d, "%Y-%m-%d").ok())
            })
            .collect()
    };
    let except = dates("except");
    times.retain(|t| !except.contains(&t.date()));
    let also = dates("also");
    if !also.is_empty() {
        let time =
            schedule::time_of_day(timing).ok_or("`also` needs a schedule that fires at one time of day")?;
        times.extend(also.iter().map(|d| d.and_time(time)));
    }
    let fired: BTreeSet<DateTime<Utc>> = times
        .into_iter()
        .map(|t| place(tz, t))
        .filter(|t| *t >= window.from && *t < window.to)
        .collect();
    Ok(fired.into_iter().collect())
}

/// A wall-clock time in `tz`: the earlier instance when it happens twice, the first instant after
/// the gap when it doesn't happen at all.
pub fn place(tz: Tz, t: NaiveDateTime) -> DateTime<Utc> {
    let mut m = t;
    // No zone skips more than a day.
    for _ in 0..=24 * 60 {
        if let Some(x) = tz.from_local_datetime(&m).earliest() {
            return x.with_timezone(&Utc);
        }
        m += Duration::minutes(1);
    }
    Utc.from_utc_datetime(&t)
}

const CRON_MACROS: &[(&str, &str)] = &[
    ("@yearly", "0 0 1 1 *"),
    ("@annually", "0 0 1 1 *"),
    ("@monthly", "0 0 1 * *"),
    ("@weekly", "0 0 * * 0"),
    ("@daily", "0 0 * * *"),
    ("@midnight", "0 0 * * *"),
    ("@hourly", "0 * * * *"),
];

/// The classic cron rule for the day fields: when either starts with `*`, a day must match both
/// (one of them being every day); otherwise a day matching either is enough.
fn cron_times(expr: &str, lo: NaiveDateTime, hi: NaiveDateTime) -> Result<Vec<NaiveDateTime>, String> {
    schedule::validate_cron(expr)?;
    let expr = expr.trim();
    let expr = CRON_MACROS
        .iter()
        .find(|(m, _)| *m == expr)
        .map(|(_, e)| *e)
        .unwrap_or(expr)
        .to_ascii_uppercase();
    let fields: Vec<&str> = expr.split_whitespace().collect();
    let both = fields[2].starts_with('*') || fields[4].starts_with('*');
    let cron = croner::parser::CronParser::builder()
        .seconds(croner::parser::Seconds::Disallowed)
        .year(croner::parser::Year::Disallowed)
        .dom_and_dow(both)
        .build()
        .parse(&expr)
        .map_err(|e| format!("can't expand cron `{expr}`: {e}"))?;
    let mut out = Vec::new();
    for t in cron.iter_from(lo, croner::Direction::Forward) {
        if t > hi {
            break;
        }
        out.push(t);
        // A minutely cron over the longest window is ~530,000 firings; past that it's a
        // runaway, not a schedule.
        if out.len() > MAX_FIRINGS * 8 {
            return Err(format!("cron `{expr}` fires too often to list"));
        }
    }
    Ok(out)
}

/// An RFC 5545 rule anchored at `starting` (else just before the window: only rules that don't
/// need an anchor get here) and `at` (unless the rule sets the hour or minute itself).
fn rule_times(
    rule: &str,
    starting: Option<NaiveDate>,
    at: Option<NaiveTime>,
    lo: NaiveDateTime,
    hi: NaiveDateTime,
) -> Result<Vec<NaiveDateTime>, String> {
    let parts = schedule::rule_parts(rule).map_err(|e| format!("invalid rrule `{rule}`: {e}"))?;
    let sets_time = parts.iter().any(|(k, _)| k == "BYHOUR" || k == "BYMINUTE");
    let time = if sets_time {
        NaiveTime::MIN
    } else {
        at.unwrap_or(NaiveTime::MIN)
    };
    let start = starting.unwrap_or(lo.date()).and_time(time);
    // Wall-clock times throughout: UNTIL is too, so it's written the way the crate wants it.
    let body: Vec<String> = parts
        .iter()
        .map(|(k, v)| match k.as_str() {
            "UNTIL" if v.len() == 8 => format!("UNTIL={v}T235959Z"),
            "UNTIL" => format!("UNTIL={}Z", v.trim_end_matches('Z')),
            _ => format!("{k}={v}"),
        })
        .collect();
    let text = format!(
        "DTSTART:{}Z\nRRULE:{}",
        start.format("%Y%m%dT%H%M%S"),
        body.join(";")
    );
    let set: rrule::RRuleSet = text
        .parse()
        .map_err(|e| format!("can't expand rrule `{rule}`: {e}"))?;
    let utc = |n: NaiveDateTime| rrule::Tz::UTC.from_utc_datetime(&n);
    let found = set.after(utc(lo)).before(utc(hi)).all(u16::MAX);
    if found.limited && found.dates.len() >= MAX_FIRINGS {
        return Err(format!(
            "rrule `{rule}` fires more than {MAX_FIRINGS} times in the window; ask for a shorter one"
        ));
    }
    Ok(found.dates.iter().map(|d| d.naive_utc()).collect())
}

/// SHA-256 (hex) of a canonical JSON value, under this format version.
fn hash(v: &Json) -> String {
    let text = format!(
        "dre_schedule_v{FORMAT_VERSION}\n{}",
        crate::manifest::canonical(v)
    );
    crate::manifest::hex(&Sha256::digest(text.as_bytes()))
}

/// What a schedule's definition hash covers: its resolved timing (named or inline), firing
/// timezone, vars, whether it's enabled, and the Bindings it runs.
fn definition(project: &Project, e: &ScheduleEntry) -> Json {
    json!({
        "timing": e.timing,
        "schedule": e.schedule,
        "timezone": firing_tz(project, e).name(),
        "vars": e.vars,
        "enabled": e.enabled,
        "bindings": bindings(project, &e.name)
            .iter()
            .map(|(r, s)| json!({"report": r, "set": s}))
            .collect::<Vec<_>>(),
    })
}

/// A schedule's definition hash: equal for an equal definition within a format version.
pub fn definition_hash(project: &Project, e: &ScheduleEntry) -> String {
    hash(&definition(project, e))
}

/// One hash over every schedule in the project (whatever was asked for), so a refresh can tell
/// that nothing changed.
pub fn project_hash(project: &Project) -> String {
    let all: BTreeMap<&str, String> = project
        .schedules
        .iter()
        .map(|e| (e.name.as_str(), definition_hash(project, e)))
        .collect();
    hash(&json!(all))
}

/// Which schedules a request covers, in schedules.yml order.
fn chosen<'a>(project: &'a Project, req: &Request) -> Result<Vec<&'a ScheduleEntry>, String> {
    for n in &req.schedules {
        if !project.schedules.iter().any(|e| &e.name == n) {
            return Err(crate::run::unknown_schedule(project, n));
        }
    }
    let reports: Option<BTreeSet<&str>> = match &req.select {
        None => None,
        Some(s) => match crate::selector::resolve(project, s) {
            Ok(r) if r.is_empty() => return Err(format!("selector `{s}` matches no report")),
            Ok(r) => Some(r.into_iter().map(|r| r.name.as_str()).collect()),
            Err(e) => return Err(e.to_string()),
        },
    };
    Ok(project
        .schedules
        .iter()
        .filter(|e| req.schedules.is_empty() || req.schedules.contains(&e.name))
        .filter(|e| match &reports {
            None => true,
            Some(rs) => bindings(project, &e.name).iter().any(|(r, _)| rs.contains(r)),
        })
        .collect())
}

/// The command that runs one firing: `dre run --schedule <name>`, narrowed to one Binding under
/// `--split`. Never a target, profile or credentials: the deployment decides those.
fn invocation(
    name: &str,
    binding: Option<(&str, Option<&str>)>,
    at: DateTime<Utc>,
    run_date: NaiveDate,
) -> Json {
    let mut argv = vec![
        "dre".to_string(),
        "run".into(),
        "--schedule".into(),
        name.to_string(),
    ];
    if let Some((report, set)) = binding {
        argv.extend(["-s".into(), report.to_string()]);
        if let Some(set) = set {
            argv.extend(["--set".into(), set.to_string()]);
        }
    }
    json!({
        "argv": argv,
        "env": {"DRE_RUN_AT": rfc3339(at), "DRE_RUN_DATE": run_date.to_string()},
    })
}

fn binding_json(report: &str, set: Option<&str>) -> Json {
    json!({"report": report, "set": set, "binding": set.unwrap_or("default")})
}

/// The `dre schedule ls` document. An error is a usage problem (an unknown schedule, a selector
/// that matches nothing); a schedule that can't be expanded is listed under `problems`.
pub fn document(project: &Project, req: &Request) -> Result<Json, String> {
    let entries = chosen(project, req)?;
    let mut schedules = JsonMap::new();
    let mut occurrences: Vec<(DateTime<Utc>, String, Json)> = Vec::new();
    let mut problems = Vec::new();
    for e in entries {
        let tz = firing_tz(project, e);
        let bs = bindings(project, &e.name);
        schedules.insert(
            e.name.clone(),
            json!({
                "definition_hash": definition_hash(project, e),
                "enabled": e.enabled,
                "timing": e.timing,
                "timezone": tz.name(),
                "vars": e.vars,
                "bindings": bs.iter().map(|(r, s)| binding_json(r, *s)).collect::<Vec<_>>(),
            }),
        );
        if !e.enabled {
            continue;
        }
        let strict = schedule::strictness(&e.schedule);
        if !strict.is_empty() {
            for (code, message) in strict {
                problems.push(json!({"schedule": e.name, "code": code, "message": message}));
            }
            continue;
        }
        let fired = match expand(&e.schedule, tz, &req.window) {
            Ok(f) => f,
            Err(message) => {
                problems
                    .push(json!({"schedule": e.name, "code": "schedule-not-expandable", "message": message}));
                continue;
            }
        };
        for at in fired.into_iter().take(req.limit.unwrap_or(usize::MAX)) {
            let local = at.with_timezone(&tz);
            let run_date = local.date_naive();
            let occurrence = |key: &str, bindings: Json, invocation: Json| {
                json!({
                    "key": key,
                    "schedule": e.name,
                    "timing": e.timing,
                    "fires_at": rfc3339(at),
                    "fires_at_local": local.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
                    "timezone": tz.name(),
                    "run_date": run_date.to_string(),
                    "bindings": bindings,
                    "vars": e.vars,
                    "invocation": invocation,
                })
            };
            let key = format!("{}/{}", e.name, rfc3339(at));
            if req.split {
                for (r, s) in &bs {
                    let key = format!("{key}/{r}/{}", s.unwrap_or("default"));
                    let o = occurrence(
                        &key,
                        json!([binding_json(r, *s)]),
                        invocation(&e.name, Some((r, *s)), at, run_date),
                    );
                    occurrences.push((at, key, o));
                }
            } else {
                let all = bs.iter().map(|(r, s)| binding_json(r, *s)).collect::<Vec<_>>();
                let o = occurrence(&key, json!(all), invocation(&e.name, None, at, run_date));
                occurrences.push((at, key, o));
            }
        }
    }
    occurrences.sort_by(|a, b| (a.0, &a.1).cmp(&(b.0, &b.1)));
    Ok(json!({
        "dre_schedule_version": FORMAT_VERSION,
        "dre_version": crate::version(),
        "project": project.name,
        "window": {"from": rfc3339(req.window.from), "to": rfc3339(req.window.to)},
        "split": req.split,
        "complete": req.schedules.is_empty() && req.select.is_none(),
        "project_hash": project_hash(project),
        "schedules": schedules,
        "occurrences": occurrences.into_iter().map(|(_, _, o)| o).collect::<Vec<_>>(),
        "problems": problems,
    }))
}
