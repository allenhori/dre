//! Schedule timings: their shape, and the checks shared by `dre validate` and
//! `dre schedule ls`. Core never fires schedules; `occurrences` works out when they would.

use chrono::NaiveTime;
use serde_json::{Map as JsonMap, Value as Json};
use serde_yaml_ng::{Mapping, Value};

pub const SCHEDULE_KEYS: &[&str] = &["cron", "every", "rrule", "starting", "at", "except", "also"];
/// Keys of a timings.yml entry: a timing and its timezone.
pub const TIMING_KEYS: &[&str] = &[
    "cron", "every", "rrule", "starting", "at", "except", "also", "timezone",
];

/// Validate one schedule block (`{cron}`, `{every, starting, at}` or `{rrule, starting, at}`,
/// each optionally with `except` and `also`).
/// Returns human-readable problems; empty means valid. Keys outside `SCHEDULE_KEYS` are the
/// caller's business (schedules.yml entries carry `select`/`report`/`set` next to them).
/// `what` names the block in messages: "a schedule needs exactly one of ...".
pub fn validate_block(m: &Mapping, what: &str) -> Vec<String> {
    let mut errs = Vec::new();
    let has = |k: &str| m.contains_key(k);
    let forms: Vec<&str> = ["cron", "every", "rrule"]
        .into_iter()
        .filter(|k| has(k))
        .collect();
    match forms.len() {
        0 => errs.push(format!("{what} needs exactly one of `cron`, `every` or `rrule`")),
        1 => {}
        _ => errs.push(format!(
            "{what} needs exactly one of `cron`, `every` or `rrule`, found {}",
            forms
                .iter()
                .map(|f| format!("`{f}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        )),
    }
    if let Some(v) = m.get("cron") {
        match v.as_str() {
            Some(s) => errs.extend(validate_cron(s).err()),
            None => errs.push("`cron` must be a string".into()),
        }
    }
    if let Some(v) = m.get("rrule") {
        match v.as_str() {
            Some(s) => errs.extend(validate_rrule(s).err()),
            None => errs.push("`rrule` must be a string".into()),
        }
    }
    if let Some(v) = m.get("every") {
        errs.extend(validate_every(v).err());
    }
    for key in ["starting", "at"] {
        if has(key) && has("cron") {
            errs.push(format!(
                "`{key}` doesn't apply to a `cron` schedule; the expression sets its days and time"
            ));
        }
    }
    for key in ["except", "also"] {
        let Some(v) = m.get(key) else { continue };
        let ok = v.as_sequence().is_some_and(|l| {
            l.iter().all(|d| {
                d.as_str()
                    .is_some_and(|d| chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").is_ok())
            })
        });
        if !ok {
            errs.push(format!("`{key}` must be a list of dates in YYYY-MM-DD form"));
        }
    }
    if has("also") && errs.is_empty() && time_of_day(&crate::project::yaml_map_to_json(m)).is_none() {
        errs.push(
            "`also` needs a schedule that fires at one time of day (e.g. `0 6 * * *` or `at: \"06:00\"`), so the added dates fire then"
                .into(),
        );
    }
    if let Some(v) = m.get("starting") {
        let ok = v
            .as_str()
            .is_some_and(|s| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok());
        if !ok {
            errs.push("`starting` must be a date in YYYY-MM-DD form".into());
        }
    }
    if let Some(v) = m.get("at") {
        let ok = v
            .as_str()
            .is_some_and(|s| s.len() == 5 && chrono::NaiveTime::parse_from_str(s, "%H:%M").is_ok());
        if !ok {
            errs.push("`at` must be a 24-hour time in HH:MM form".into());
        }
    }
    errs
}

fn validate_every(v: &Value) -> Result<(), String> {
    let Some(m) = v.as_mapping() else {
        return Err("`every` must be a map with one unit, e.g. `{days: 5}`".into());
    };
    if m.len() != 1 {
        return Err("`every` must have exactly one unit: `days`, `weeks` or `months`".into());
    }
    let (k, n) = m.iter().next().unwrap();
    let unit = k.as_str().unwrap_or("");
    if !matches!(unit, "days" | "weeks" | "months") {
        return Err(format!(
            "unknown `every` unit `{unit}`: use `days`, `weeks` or `months`"
        ));
    }
    match n.as_u64() {
        Some(n) if n > 0 => Ok(()),
        _ => Err(format!("`every.{unit}` must be a positive whole number")),
    }
}

const CRON_MACROS: &[&str] = &[
    "@yearly",
    "@annually",
    "@monthly",
    "@weekly",
    "@daily",
    "@midnight",
    "@hourly",
];

pub fn validate_cron(expr: &str) -> Result<(), String> {
    let expr = expr.trim();
    if expr.starts_with('@') {
        return if CRON_MACROS.contains(&expr) {
            Ok(())
        } else {
            Err(format!("invalid cron `{expr}`: unknown macro"))
        };
    }
    let fields: Vec<&str> = expr.split_whitespace().collect();
    if fields.len() != 5 {
        return Err(format!(
            "invalid cron `{expr}`: expected 5 fields (minute hour day-of-month month day-of-week), found {}",
            fields.len()
        ));
    }
    const MONTHS: &[&str] = &[
        "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
    ];
    const DAYS: &[&str] = &["sun", "mon", "tue", "wed", "thu", "fri", "sat"];
    let specs: [(&str, u32, u32, &[&str], u32); 5] = [
        ("minute", 0, 59, &[], 0),
        ("hour", 0, 23, &[], 0),
        ("day-of-month", 1, 31, &[], 0),
        ("month", 1, 12, MONTHS, 1),
        ("day-of-week", 0, 7, DAYS, 0),
    ];
    for (field, (name, lo, hi, names, base)) in fields.iter().zip(specs) {
        cron_field(field, lo, hi, names, base)
            .map_err(|e| format!("invalid cron `{expr}`: {name} field `{field}` {e}"))?;
    }
    Ok(())
}

fn cron_field(field: &str, lo: u32, hi: u32, names: &[&str], base: u32) -> Result<(), String> {
    let value = |s: &str| -> Result<u32, String> {
        if let Some(i) = names.iter().position(|n| n.eq_ignore_ascii_case(s)) {
            return Ok(i as u32 + base);
        }
        let n: u32 = s.parse().map_err(|_| format!("has an invalid value `{s}`"))?;
        if n < lo || n > hi {
            return Err(format!("has `{n}` outside {lo}-{hi}"));
        }
        Ok(n)
    };
    for part in field.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, s)) => (r, Some(s)),
            None => (part, None),
        };
        if let Some(s) = step {
            match s.parse::<u32>() {
                Ok(n) if n > 0 => {}
                _ => return Err(format!("has an invalid step `{s}`")),
            }
        }
        if range == "*" {
            continue;
        }
        match range.split_once('-') {
            Some((a, b)) => {
                let (a, b) = (value(a)?, value(b)?);
                if a > b {
                    return Err(format!("has a backwards range `{range}`"));
                }
            }
            None => {
                value(range)?;
            }
        }
    }
    Ok(())
}

/// RFC 5545 RRULE value validation (the rule itself, optionally prefixed `RRULE:`).
pub fn validate_rrule(expr: &str) -> Result<(), String> {
    let body = expr.trim();
    let body = body.strip_prefix("RRULE:").unwrap_or(body);
    let err = |m: String| Err(format!("invalid rrule `{expr}`: {m}"));
    let mut seen = std::collections::BTreeSet::new();
    let mut freq = false;
    for part in body.split(';').filter(|p| !p.is_empty()) {
        let Some((k, v)) = part.split_once('=') else {
            return err(format!("`{part}` is not KEY=VALUE"));
        };
        let k = k.to_ascii_uppercase();
        if !seen.insert(k.clone()) {
            return err(format!("`{k}` appears more than once"));
        }
        let list = |lo: i64, hi: i64, signed: bool| -> Result<(), String> {
            for x in v.split(',') {
                let n: i64 = x
                    .parse()
                    .map_err(|_| format!("`{k}` has an invalid value `{x}`"))?;
                let ok = if signed {
                    n != 0 && n.abs() >= lo && n.abs() <= hi
                } else {
                    n >= lo && n <= hi
                };
                if !ok {
                    return Err(format!("`{k}` value `{x}` is out of range"));
                }
            }
            Ok(())
        };
        let r = match k.as_str() {
            "FREQ" => {
                freq = true;
                if [
                    "SECONDLY", "MINUTELY", "HOURLY", "DAILY", "WEEKLY", "MONTHLY", "YEARLY",
                ]
                .contains(&v.to_ascii_uppercase().as_str())
                {
                    Ok(())
                } else {
                    Err(format!("unknown FREQ `{v}`"))
                }
            }
            "INTERVAL" | "COUNT" => match v.parse::<u64>() {
                Ok(n) if n > 0 => Ok(()),
                _ => Err(format!("`{k}` must be a positive whole number")),
            },
            "UNTIL" => {
                let ok = chrono::NaiveDate::parse_from_str(v, "%Y%m%d").is_ok()
                    || chrono::NaiveDateTime::parse_from_str(v.trim_end_matches('Z'), "%Y%m%dT%H%M%S")
                        .is_ok();
                if ok {
                    Ok(())
                } else {
                    Err(format!("`UNTIL` `{v}` is not a DATE or DATE-TIME"))
                }
            }
            "BYSECOND" => list(0, 60, false),
            "BYMINUTE" => list(0, 59, false),
            "BYHOUR" => list(0, 23, false),
            "BYMONTHDAY" => list(1, 31, true),
            "BYYEARDAY" => list(1, 366, true),
            "BYWEEKNO" => list(1, 53, true),
            "BYMONTH" => list(1, 12, false),
            "BYSETPOS" => list(1, 366, true),
            "BYDAY" => v
                .split(',')
                .try_for_each(|d| byday(d).ok_or(format!("`BYDAY` has an invalid day `{d}`"))),
            "WKST" => weekday(v)
                .then_some(())
                .ok_or(format!("`WKST` has an invalid day `{v}`")),
            _ => Err(format!("unknown rule part `{k}`")),
        };
        if let Err(m) = r {
            return err(m);
        }
    }
    if !freq {
        return err("`FREQ` is required".into());
    }
    if seen.contains("UNTIL") && seen.contains("COUNT") {
        return err("`UNTIL` and `COUNT` can't both be set".into());
    }
    Ok(())
}

fn weekday(d: &str) -> bool {
    ["MO", "TU", "WE", "TH", "FR", "SA", "SU"].contains(&d.to_ascii_uppercase().as_str())
}

fn byday(d: &str) -> Option<()> {
    let (num, day) = d.split_at(d.len().checked_sub(2)?);
    if !weekday(day) {
        return None;
    }
    if num.is_empty() {
        return Some(());
    }
    let n: i64 = num.trim_start_matches('+').parse().ok()?;
    (n != 0 && n.abs() <= 53).then_some(())
}

/// A rule's parts as `(KEY, VALUE)`, upper-cased, in the order written.
pub fn rule_parts(expr: &str) -> Result<Vec<(String, String)>, String> {
    validate_rrule(expr)?;
    let body = expr.trim();
    let body = body.strip_prefix("RRULE:").unwrap_or(body);
    Ok(body
        .split(';')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| (k.to_ascii_uppercase(), v.to_ascii_uppercase()))
        .collect())
}

fn part<'a>(parts: &'a [(String, String)], key: &str) -> Option<&'a str> {
    parts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

/// The one time of day a timing fires at, if it has one: `at`, a cron expression with a single
/// minute and hour, or a rule's single `BYHOUR`/`BYMINUTE` (00:00 when nothing sets it).
pub fn time_of_day(timing: &JsonMap<String, Json>) -> Option<NaiveTime> {
    let s = |k: &str| timing.get(k).and_then(Json::as_str);
    let at = s("at").and_then(|t| NaiveTime::parse_from_str(t, "%H:%M").ok());
    let one = |v: Option<&str>, default: u32| -> Option<u32> {
        match v {
            None => Some(default),
            Some(v) => v.parse().ok(),
        }
    };
    if let Some(expr) = s("cron") {
        let expr = match expr.trim() {
            "@yearly" | "@annually" | "@monthly" | "@weekly" | "@daily" | "@midnight" => "0 0 * * *",
            e => e,
        };
        let f: Vec<&str> = expr.split_whitespace().collect();
        let (m, h) = (f.first()?.parse().ok()?, f.get(1)?.parse().ok()?);
        return NaiveTime::from_hms_opt(h, m, 0);
    }
    if let Some(rule) = s("rrule") {
        let parts = rule_parts(rule).ok()?;
        let (h, m) = (part(&parts, "BYHOUR"), part(&parts, "BYMINUTE"));
        if matches!(part(&parts, "FREQ"), Some("HOURLY" | "MINUTELY" | "SECONDLY")) && h.is_none() {
            return None;
        }
        if h.is_some() || m.is_some() {
            return NaiveTime::from_hms_opt(one(h, 0)?, one(m, 0)?, 0);
        }
        return Some(at.unwrap_or(NaiveTime::MIN));
    }
    Some(at.unwrap_or(NaiveTime::MIN))
}

/// Problems that are warnings on 0.1.x and errors from 0.2.0: a timing whose occurrences would
/// depend on when you look (no anchor), or one finer than a minute. `dre schedule ls` lists such
/// a schedule under `problems`, without occurrences. Each is `(code, message)`.
pub fn strictness(timing: &JsonMap<String, Json>) -> Vec<(&'static str, String)> {
    const LATER: &str = "this becomes an error in DRE 0.2.0";
    let mut out = Vec::new();
    let anchored = timing.contains_key("starting");
    if timing.contains_key("every") && !anchored {
        out.push((
            "schedule-needs-anchor",
            format!("`every` needs `starting` (its first date): it counts from it, so its occurrences would depend on when you look; {LATER}"),
        ));
    }
    let Some(parts) = timing
        .get("rrule")
        .and_then(Json::as_str)
        .and_then(|r| rule_parts(r).ok())
    else {
        return out;
    };
    let freq = part(&parts, "FREQ").unwrap_or("");
    if matches!(freq, "SECONDLY" | "MINUTELY") {
        out.push((
            "schedule-too-frequent",
            format!("`FREQ={freq}` is finer than DRE schedules go: use `FREQ=HOURLY` with `BYMINUTE`, or a cron expression; {LATER}"),
        ));
    }
    if part(&parts, "BYSECOND").is_some() {
        out.push((
            "schedule-seconds",
            format!("`BYSECOND` isn't supported: schedules fire on whole minutes; {LATER}"),
        ));
    }
    if !anchored {
        let interval = part(&parts, "INTERVAL")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(1);
        let day_from_start = matches!(freq, "WEEKLY" | "MONTHLY" | "YEARLY")
            && ["BYDAY", "BYMONTHDAY", "BYYEARDAY", "BYWEEKNO"]
                .iter()
                .all(|k| part(&parts, k).is_none());
        let why = if interval > 1 {
            Some("`INTERVAL` counts from it")
        } else if part(&parts, "COUNT").is_some() {
            Some("`COUNT` counts from it")
        } else if day_from_start {
            Some("the rule takes its day from it")
        } else {
            None
        };
        if let Some(why) = why {
            out.push((
                "schedule-needs-anchor",
                format!("this rule needs `starting` (its first date): {why}, so its occurrences would depend on when you look; {LATER}"),
            ));
        }
    }
    out
}

/// A warning when nothing gives a timing a time of day, so it fires at midnight.
pub fn no_time(timing: &JsonMap<String, Json>) -> Option<String> {
    if timing.contains_key("at") || timing.contains_key("cron") {
        return None;
    }
    if let Some(parts) = timing
        .get("rrule")
        .and_then(Json::as_str)
        .and_then(|r| rule_parts(r).ok())
    {
        let daily_or_coarser = matches!(
            part(&parts, "FREQ"),
            Some("DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY")
        );
        if !daily_or_coarser || part(&parts, "BYHOUR").is_some() {
            return None;
        }
    }
    Some(
        "no time of day is given, so it fires at 00:00 in its timezone; set `at: \"HH:MM\"` to say when"
            .into(),
    )
}
