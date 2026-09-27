//! Calendar values for templates: `run.date`, `run.now`, `date()`, `datetime()` and the
//! periods built from them. Everything is worked out at render time, so compiled SQL holds
//! literals and a rerun with the same `DRE_RUN_DATE` renders the same text.
//!
//! Three types:
//! - `Date`: a calendar day. Renders `YYYY-MM-DD`.
//! - `DateTime`: an instant, shown in a timezone. Renders `YYYY-MM-DD HH:MM:SS[.ffffff]`.
//! - `Period`: a run of whole days (a week, a month, ...). `start` and `end` are its first
//!   and last instants; `start.date` and `end.date` its first and last days.
//!
//! A `Calendar` (the run's timezone, first day of the week and week numbering) travels with
//! every value, so navigation and conversions agree with the project's settings.

use std::fmt;
use std::sync::Arc;

use chrono::{
    DateTime as ChronoDateTime, Datelike, Days, Duration, LocalResult, Months, NaiveDate, NaiveDateTime,
    NaiveTime, Offset, TimeZone, Timelike, Utc, Weekday,
};
use chrono_tz::Tz;
use minijinja::value::{Kwargs, Object, ObjectRepr, Value};
use minijinja::{Environment, Error, ErrorKind, State};

/// The first day of a week.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekStart {
    Monday,
    Sunday,
}

impl WeekStart {
    pub fn parse(s: &str) -> Option<WeekStart> {
        match s {
            "monday" => Some(WeekStart::Monday),
            "sunday" => Some(WeekStart::Sunday),
            _ => None,
        }
    }
    fn weekday(self) -> Weekday {
        match self {
            WeekStart::Monday => Weekday::Mon,
            WeekStart::Sunday => Weekday::Sun,
        }
    }
}

/// How weeks are numbered: `iso` (week 1 holds the year's first Thursday) or `us` (week 1
/// holds 1 January).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WeekNumbering {
    Iso,
    Us,
}

impl WeekNumbering {
    pub fn parse(s: &str) -> Option<WeekNumbering> {
        match s {
            "iso" => Some(WeekNumbering::Iso),
            "us" => Some(WeekNumbering::Us),
            _ => None,
        }
    }
}

/// The settings every date value carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Calendar {
    pub tz: Tz,
    pub week_start: WeekStart,
    pub numbering: WeekNumbering,
}

impl Default for Calendar {
    fn default() -> Self {
        Calendar {
            tz: Tz::UTC,
            week_start: WeekStart::Monday,
            numbering: WeekNumbering::Iso,
        }
    }
}

/// Parse an IANA timezone name (`Australia/Sydney`, `UTC`).
pub fn parse_tz(name: &str) -> Result<Tz, String> {
    name.parse::<Tz>().map_err(|_| {
        format!("`{name}` isn't a timezone; use an IANA name such as `UTC` or `Australia/Sydney`")
    })
}

impl Calendar {
    /// Today in this calendar's timezone.
    pub fn today(&self) -> NaiveDate {
        Utc::now().with_timezone(&self.tz).date_naive()
    }

    /// The first instant of `d` here: midnight, or the first valid time after a DST gap.
    fn start_of(&self, d: NaiveDate) -> ChronoDateTime<Tz> {
        let mut t = d.and_time(NaiveTime::MIN);
        loop {
            if let Some(x) = self.tz.from_local_datetime(&t).earliest() {
                return x;
            }
            t += Duration::minutes(15);
        }
    }

    /// The last microsecond of `d` here.
    fn end_of(&self, d: NaiveDate) -> ChronoDateTime<Tz> {
        self.start_of(d + Days::new(1)) - Duration::microseconds(1)
    }

    fn week_start_of(&self, d: NaiveDate) -> NaiveDate {
        let back =
            (7 + d.weekday().num_days_from_monday() - self.week_start.weekday().num_days_from_monday()) % 7;
        d - Days::new(back as u64)
    }

    /// `(week_year, week)` in this calendar's numbering.
    fn week_number(&self, d: NaiveDate) -> (i32, u32) {
        match self.numbering {
            WeekNumbering::Iso => (d.iso_week().year(), d.iso_week().week()),
            WeekNumbering::Us => {
                let first = self.week_start_of(NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap());
                (d.year(), ((d - first).num_days() / 7) as u32 + 1)
            }
        }
    }

    /// The first day of week `w` of `year`, in this calendar's numbering.
    fn week_of(&self, year: i32, w: u32) -> Result<NaiveDate, String> {
        let bad = || format!("week_of({year}, {w}): there's no week {w} in {year}");
        match self.numbering {
            WeekNumbering::Iso => {
                let monday = NaiveDate::from_isoywd_opt(year, w, Weekday::Mon).ok_or_else(bad)?;
                Ok(self.week_start_of(monday))
            }
            WeekNumbering::Us => {
                let jan1 = NaiveDate::from_ymd_opt(year, 1, 1).ok_or_else(bad)?;
                let start = self.week_start_of(jan1) + Days::new(7 * (w.max(1) as u64 - 1));
                if w == 0 || start.year() > year {
                    return Err(bad());
                }
                Ok(start)
            }
        }
    }
}

fn err(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, msg.into())
}

fn quarter_start(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), (d.month() - 1) / 3 * 3 + 1, 1).unwrap()
}

fn month_start(d: NaiveDate) -> NaiveDate {
    d.with_day(1).unwrap()
}

fn year_start(d: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(d.year(), 1, 1).unwrap()
}

fn add_months(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    if n >= 0 {
        d.checked_add_months(Months::new(n as u32))
    } else {
        d.checked_sub_months(Months::new(n.unsigned_abs() as u32))
    }
}

fn add_days(d: NaiveDate, n: i64) -> Option<NaiveDate> {
    if n >= 0 {
        d.checked_add_days(Days::new(n as u64))
    } else {
        d.checked_sub_days(Days::new(n.unsigned_abs()))
    }
}

/// `strftime` with an error instead of a panic for a bad format string.
fn strftime(f: impl fmt::Display) -> Result<String, Error> {
    use fmt::Write;
    let mut out = String::new();
    write!(out, "{f}").map_err(|_| err("invalid date format string"))?;
    Ok(out)
}

// ---------------------------------------------------------------------------------------------
// Date
// ---------------------------------------------------------------------------------------------

/// Every attribute of a `Date`, for `dre validate`'s check of `run.date.<name>`.
pub const DATE_ATTRS: &[&str] = &[
    "year",
    "month",
    "day",
    "quarter",
    "week",
    "week_year",
    "weekday",
    "day_of_year",
    "days_in_month",
    "prev_day",
    "next_day",
    "week_start",
    "week_end",
    "month_start",
    "month_end",
    "quarter_start",
    "quarter_end",
    "year_start",
    "year_end",
    "this_week",
    "prev_week",
    "next_week",
    "this_month",
    "prev_month",
    "next_month",
    "this_quarter",
    "prev_quarter",
    "next_quarter",
    "this_year",
    "prev_year",
    "next_year",
    "start",
    "end",
    "date",
    "unix",
    "unix_ms",
    "iso",
    "yyyymmdd",
    "ddmmyyyy",
    "yyyy",
    "mm",
    "dd",
    "add",
    "format",
    "as_period",
];

#[derive(Debug, Clone)]
pub struct Date {
    d: NaiveDate,
    cal: Calendar,
}

impl Date {
    pub fn value(d: NaiveDate, cal: Calendar) -> Value {
        Value::from_object(Date { d, cal })
    }

    fn at(&self, d: NaiveDate) -> Value {
        Date::value(d, self.cal)
    }

    fn period(&self, kind: Kind, first: NaiveDate) -> Value {
        Period::value(Period::of(kind, first, self.cal))
    }
}

impl fmt::Display for Date {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.d.format("%Y-%m-%d"))
    }
}

impl Object for Date {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let d = self.d;
        let cal = self.cal;
        let s = |f: &str| Some(Value::from(d.format(f).to_string()));
        Some(match key.as_str()? {
            "year" => Value::from(d.year()),
            "month" => Value::from(d.month()),
            "day" => Value::from(d.day()),
            "quarter" => Value::from((d.month() - 1) / 3 + 1),
            "week" => Value::from(cal.week_number(d).1),
            "week_year" => Value::from(cal.week_number(d).0),
            "weekday" => Value::from(d.weekday().number_from_monday()),
            "day_of_year" => Value::from(d.ordinal()),
            "days_in_month" => {
                Value::from((add_months(month_start(d), 1).unwrap() - month_start(d)).num_days())
            }
            "prev_day" => self.at(d.pred_opt()?),
            "next_day" => self.at(d.succ_opt()?),
            "week_start" => self.at(cal.week_start_of(d)),
            "week_end" => self.at(cal.week_start_of(d) + Days::new(6)),
            "month_start" => self.at(month_start(d)),
            "month_end" => self.at(add_months(month_start(d), 1)?.pred_opt()?),
            "quarter_start" => self.at(quarter_start(d)),
            "quarter_end" => self.at(add_months(quarter_start(d), 3)?.pred_opt()?),
            "year_start" => self.at(year_start(d)),
            "year_end" => self.at(NaiveDate::from_ymd_opt(d.year(), 12, 31)?),
            "this_week" => self.period(Kind::Week, cal.week_start_of(d)),
            "prev_week" => self.period(Kind::Week, cal.week_start_of(d) - Days::new(7)),
            "next_week" => self.period(Kind::Week, cal.week_start_of(d) + Days::new(7)),
            "this_month" => self.period(Kind::Month, month_start(d)),
            "prev_month" => self.period(Kind::Month, add_months(month_start(d), -1)?),
            "next_month" => self.period(Kind::Month, add_months(month_start(d), 1)?),
            "this_quarter" => self.period(Kind::Quarter, quarter_start(d)),
            "prev_quarter" => self.period(Kind::Quarter, add_months(quarter_start(d), -3)?),
            "next_quarter" => self.period(Kind::Quarter, add_months(quarter_start(d), 3)?),
            "this_year" => self.period(Kind::Year, year_start(d)),
            "prev_year" => self.period(Kind::Year, add_months(year_start(d), -12)?),
            "next_year" => self.period(Kind::Year, add_months(year_start(d), 12)?),
            "start" => DateTime::value(cal.start_of(d), cal),
            "end" => DateTime::value(cal.end_of(d), cal),
            "date" => self.at(d),
            "unix" => Value::from(cal.start_of(d).timestamp()),
            "unix_ms" => Value::from(cal.start_of(d).timestamp_millis()),
            "iso" => return s("%Y-%m-%d"),
            "yyyymmdd" => return s("%Y%m%d"),
            "ddmmyyyy" => return s("%d%m%Y"),
            "yyyy" => return s("%Y"),
            "mm" => return s("%m"),
            "dd" => return s("%d"),
            _ => return None,
        })
    }

    fn call_method(
        self: &Arc<Self>,
        _: &State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "format" => {
                let (fmt,): (&str,) = minijinja::value::from_args(args)?;
                Ok(Value::from(strftime(self.d.format(fmt))?))
            }
            "add" => {
                let (kw,): (Kwargs,) = minijinja::value::from_args(args)?;
                let get = |k: &str| -> Result<i64, Error> { Ok(kw.get::<Option<i64>>(k)?.unwrap_or(0)) };
                let (years, months, weeks, days) =
                    (get("years")?, get("months")?, get("weeks")?, get("days")?);
                kw.assert_all_used()?;
                let d = add_months(self.d, years * 12 + months)
                    .and_then(|d| add_days(d, weeks * 7 + days))
                    .ok_or_else(|| err("date out of range"))?;
                Ok(self.at(d))
            }
            // `d.day` is the day of the month; the one-day period is `d.as_period()`.
            "as_period" => Ok(self.period(Kind::Day, self.d)),
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

// ---------------------------------------------------------------------------------------------
// DateTime
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DateTime {
    t: ChronoDateTime<Tz>,
    cal: Calendar,
}

impl DateTime {
    pub fn value(t: ChronoDateTime<Tz>, cal: Calendar) -> Value {
        Value::from_object(DateTime { t, cal })
    }

    /// `run.now`: an instant shown in the run's timezone.
    pub fn now(at: ChronoDateTime<Utc>, cal: Calendar) -> Value {
        DateTime::value(at.with_timezone(&cal.tz), cal)
    }

    fn fraction(&self) -> &'static str {
        if self.t.nanosecond() == 0 { "" } else { ".%6f" }
    }
}

impl fmt::Display for DateTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.t.format(&format!("%Y-%m-%d %H:%M:%S{}", self.fraction()))
        )
    }
}

impl Object for DateTime {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let t = self.t;
        Some(match key.as_str()? {
            "date" => Date::value(t.date_naive(), self.cal),
            "time" => Value::from(t.format(&format!("%H:%M:%S{}", self.fraction())).to_string()),
            "utc" => DateTime::value(t.with_timezone(&Tz::UTC), self.cal),
            "iso" => {
                let f = if t.nanosecond() == 0 {
                    "%Y-%m-%dT%H:%M:%S%:z"
                } else {
                    "%Y-%m-%dT%H:%M:%S%.6f%:z"
                };
                Value::from(t.format(f).to_string())
            }
            "unix" => Value::from(t.timestamp()),
            "unix_ms" => Value::from(t.timestamp_millis()),
            "timezone" => Value::from(t.timezone().name()),
            "offset" => Value::from(t.offset().fix().to_string()),
            "year" => Value::from(t.year()),
            "month" => Value::from(t.month()),
            "day" => Value::from(t.day()),
            "hour" => Value::from(t.hour()),
            "minute" => Value::from(t.minute()),
            "second" => Value::from(t.second()),
            _ => return None,
        })
    }

    fn call_method(
        self: &Arc<Self>,
        _: &State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        match method {
            "format" => {
                let (fmt,): (&str,) = minijinja::value::from_args(args)?;
                Ok(Value::from(strftime(self.t.format(fmt))?))
            }
            "tz" => {
                let (name,): (&str,) = minijinja::value::from_args(args)?;
                let tz = parse_tz(name).map_err(err)?;
                Ok(DateTime::value(self.t.with_timezone(&tz), self.cal))
            }
            "add" => {
                let (kw,): (Kwargs,) = minijinja::value::from_args(args)?;
                let get = |k: &str| -> Result<i64, Error> { Ok(kw.get::<Option<i64>>(k)?.unwrap_or(0)) };
                let (years, months, weeks, days) =
                    (get("years")?, get("months")?, get("weeks")?, get("days")?);
                let (hours, minutes, seconds) = (get("hours")?, get("minutes")?, get("seconds")?);
                kw.assert_all_used()?;
                // Calendar parts move the wall clock (a day later is the same clock time, even
                // across a daylight-saving change); clock parts move the instant.
                let moved = if years == 0 && months == 0 && weeks == 0 && days == 0 {
                    self.t
                } else {
                    let local = self.t.naive_local();
                    let day = add_months(local.date(), years * 12 + months)
                        .and_then(|d| add_days(d, weeks * 7 + days))
                        .ok_or_else(|| err("date out of range"))?;
                    let wall = day.and_time(local.time());
                    let tz = self.t.timezone();
                    let offset = self.t.offset().fix();
                    match tz.from_local_datetime(&wall) {
                        LocalResult::Single(t) => t,
                        // A clock time that happens twice: keep the offset it had, if it can.
                        LocalResult::Ambiguous(a, b) => {
                            if b.offset().fix() == offset {
                                b
                            } else {
                                a
                            }
                        }
                        // A clock time the change skips: read it with the old offset, so 02:30
                        // becomes 03:30 when 02:00 jumps to 03:00.
                        LocalResult::None => tz.from_utc_datetime(&(wall - offset)),
                    }
                };
                let t =
                    moved + Duration::hours(hours) + Duration::minutes(minutes) + Duration::seconds(seconds);
                Ok(DateTime::value(t, self.cal))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }

    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&**self, f)
    }
}

// ---------------------------------------------------------------------------------------------
// Period
// ---------------------------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Day,
    Week,
    Month,
    Quarter,
    Year,
    /// Any run of days (`date_range()`): `next`/`prev` move by its length.
    Days(u64),
}

#[derive(Debug, Clone)]
pub struct Period {
    kind: Kind,
    first: NaiveDate,
    last: NaiveDate,
    cal: Calendar,
}

impl Period {
    fn value(p: Period) -> Value {
        Value::from_object(p)
    }

    fn of(kind: Kind, first: NaiveDate, cal: Calendar) -> Period {
        let last = match kind {
            Kind::Day => first,
            Kind::Week => first + Days::new(6),
            Kind::Month => add_months(first, 1).unwrap() - Days::new(1),
            Kind::Quarter => add_months(first, 3).unwrap() - Days::new(1),
            Kind::Year => add_months(first, 12).unwrap() - Days::new(1),
            Kind::Days(n) => first + Days::new(n - 1),
        };
        Period {
            kind,
            first,
            last,
            cal,
        }
    }

    fn shift(&self, forward: bool) -> Option<Period> {
        let first = match (self.kind, forward) {
            (Kind::Day, f) => add_days(self.first, if f { 1 } else { -1 })?,
            (Kind::Week, f) => add_days(self.first, if f { 7 } else { -7 })?,
            (Kind::Month, f) => add_months(self.first, if f { 1 } else { -1 })?,
            (Kind::Quarter, f) => add_months(self.first, if f { 3 } else { -3 })?,
            (Kind::Year, f) => add_months(self.first, if f { 12 } else { -12 })?,
            (Kind::Days(n), f) => add_days(self.first, if f { n as i64 } else { -(n as i64) })?,
        };
        Some(Period::of(self.kind, first, self.cal))
    }
}

impl Object for Period {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Plain
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let cal = self.cal;
        Some(match key.as_str()? {
            "start" => DateTime::value(cal.start_of(self.first), cal),
            "end" => DateTime::value(cal.end_of(self.last), cal),
            "first_day" => Date::value(self.first, cal),
            "last_day" => Date::value(self.last, cal),
            "next" => Period::value(self.shift(true)?),
            "prev" => Period::value(self.shift(false)?),
            "days" => Value::from((self.last - self.first).num_days() + 1),
            _ => return None,
        })
    }

    /// ISO 8601 interval notation, so a Period dropped into SQL by mistake is easy to spot.
    fn render(self: &Arc<Self>, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}/{}",
            self.first.format("%Y-%m-%d"),
            self.last.format("%Y-%m-%d")
        )
    }
}

// ---------------------------------------------------------------------------------------------
// Constructors
// ---------------------------------------------------------------------------------------------

fn parse_date(s: &str) -> Option<NaiveDate> {
    let s = s.trim();
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .or_else(|_| NaiveDate::parse_from_str(s, "%Y%m%d"))
        .ok()
        .or_else(|| {
            s.get(..10)
                .and_then(|p| NaiveDate::parse_from_str(p, "%Y-%m-%d").ok())
        })
}

fn parse_datetime(s: &str, cal: Calendar) -> Option<ChronoDateTime<Tz>> {
    let s = s.trim();
    if let Ok(t) = ChronoDateTime::parse_from_rfc3339(s) {
        return Some(t.with_timezone(&cal.tz));
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f%:z",
        "%Y-%m-%d %H:%M:%S%:z",
        "%Y-%m-%d %H:%M%:z",
    ] {
        if let Ok(t) = ChronoDateTime::parse_from_str(s, f) {
            return Some(t.with_timezone(&cal.tz));
        }
    }
    for f in [
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%dT%H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M",
    ] {
        if let Ok(t) = NaiveDateTime::parse_from_str(s, f) {
            return cal.tz.from_local_datetime(&t).earliest();
        }
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .map(|d| cal.start_of(d))
}

/// A Date from a value: a Date, a DateTime (its day), or a string.
fn to_date(v: &Value, cal: Calendar) -> Result<NaiveDate, Error> {
    if let Some(d) = v.downcast_object_ref::<Date>() {
        return Ok(d.d);
    }
    if let Some(t) = v.downcast_object_ref::<DateTime>() {
        return Ok(t.t.date_naive());
    }
    let _ = cal;
    v.as_str()
        .and_then(parse_date)
        .ok_or_else(|| err(format!("`{v}` isn't a date; use `YYYY-MM-DD`")))
}

fn to_datetime(v: &Value, cal: Calendar) -> Result<ChronoDateTime<Tz>, Error> {
    if let Some(t) = v.downcast_object_ref::<DateTime>() {
        return Ok(t.t);
    }
    if let Some(d) = v.downcast_object_ref::<Date>() {
        return Ok(cal.start_of(d.d));
    }
    v.as_str().and_then(|s| parse_datetime(s, cal)).ok_or_else(|| {
        err(format!(
            "`{v}` isn't a date and time; use `YYYY-MM-DD HH:MM:SS`, optionally with an offset"
        ))
    })
}

fn ymd(y: i32, m: u32, d: u32) -> Result<NaiveDate, Error> {
    NaiveDate::from_ymd_opt(y, m, d).ok_or_else(|| err(format!("{y}-{m:02}-{d:02} isn't a date")))
}

/// The named periods `period()` knows.
pub const PERIOD_PRESETS: &[&str] = &[
    "today",
    "yesterday",
    "this_week",
    "last_week",
    "this_month",
    "last_month",
    "mtd",
    "this_quarter",
    "last_quarter",
    "qtd",
    "this_year",
    "last_year",
    "ytd",
    "last_n_days",
    "last_n_months",
];

/// A named period relative to `as_of`: `last_month`, `mtd`, `last_n_days` (with `n`), ...
/// `mtd`, `qtd` and `ytd` run through `as_of` itself; `last_n_days` ends the day before it.
fn preset(name: &str, as_of: NaiveDate, n: Option<i64>, cal: Calendar) -> Result<Period, String> {
    let d = as_of;
    let week = cal.week_start_of(d);
    let range = |first: NaiveDate, last: NaiveDate| {
        Period::of(Kind::Days((last - first).num_days() as u64 + 1), first, cal)
    };
    let need_n = || match n {
        Some(n) if n > 0 => Ok(n),
        _ => Err(format!(
            "period('{name}') needs a positive `n`, e.g. period('{name}', n=7)"
        )),
    };
    let bad = || "date out of range".to_string();
    Ok(match name {
        "today" => Period::of(Kind::Day, d, cal),
        "yesterday" => Period::of(Kind::Day, d.pred_opt().ok_or_else(bad)?, cal),
        "this_week" => Period::of(Kind::Week, week, cal),
        "last_week" => Period::of(Kind::Week, week - Days::new(7), cal),
        "this_month" => Period::of(Kind::Month, month_start(d), cal),
        "last_month" => Period::of(Kind::Month, add_months(month_start(d), -1).ok_or_else(bad)?, cal),
        "mtd" => range(month_start(d), d),
        "this_quarter" => Period::of(Kind::Quarter, quarter_start(d), cal),
        "last_quarter" => Period::of(
            Kind::Quarter,
            add_months(quarter_start(d), -3).ok_or_else(bad)?,
            cal,
        ),
        "qtd" => range(quarter_start(d), d),
        "this_year" => Period::of(Kind::Year, year_start(d), cal),
        "last_year" => Period::of(Kind::Year, add_months(year_start(d), -12).ok_or_else(bad)?, cal),
        "ytd" => range(year_start(d), d),
        "last_n_days" => {
            let n = need_n()?;
            range(add_days(d, -n).ok_or_else(bad)?, d.pred_opt().ok_or_else(bad)?)
        }
        "last_n_months" => {
            let n = need_n()?;
            let first = add_months(month_start(d), -n).ok_or_else(bad)?;
            range(first, month_start(d).pred_opt().ok_or_else(bad)?)
        }
        _ => {
            return Err(format!(
                "period('{name}'): unknown period; use one of {}",
                PERIOD_PRESETS.join(", ")
            ));
        }
    })
}

/// Add `date()`, `datetime()`, `period()`, `date_range()`, `month_of()`, `quarter_of()`,
/// `year_of()`, `week_of()` and the `as_date` / `as_datetime` filters. `run_date` is what
/// `period()` counts from by default.
pub fn register(env: &mut Environment<'static>, cal: Calendar, run_date: NaiveDate) {
    env.add_function(
        "period",
        move |name: String, kwargs: Kwargs| -> Result<Value, Error> {
            let as_of: Option<Value> = kwargs.get("as_of")?;
            let n: Option<i64> = kwargs.get("n")?;
            kwargs.assert_all_used()?;
            let d = match as_of {
                Some(v) => to_date(&v, cal)?,
                None => run_date,
            };
            Ok(Period::value(preset(&name, d, n, cal).map_err(err)?))
        },
    );
    env.add_function(
        "date",
        move |a: Value, m: Option<u32>, d: Option<u32>| -> Result<Value, Error> {
            match (m, d) {
                (Some(m), Some(d)) => {
                    let y =
                        i32::try_from(a).map_err(|_| err("date(year, month, day) takes three numbers"))?;
                    Ok(Date::value(ymd(y, m, d)?, cal))
                }
                (None, None) => Ok(Date::value(to_date(&a, cal)?, cal)),
                _ => Err(err("date() takes a `YYYY-MM-DD` string or (year, month, day)")),
            }
        },
    );
    env.add_function("datetime", move |a: Value| -> Result<Value, Error> {
        Ok(DateTime::value(to_datetime(&a, cal)?, cal))
    });
    env.add_filter("as_date", move |a: Value| -> Result<Value, Error> {
        Ok(Date::value(to_date(&a, cal)?, cal))
    });
    env.add_filter("as_datetime", move |a: Value| -> Result<Value, Error> {
        Ok(DateTime::value(to_datetime(&a, cal)?, cal))
    });
    env.add_function("date_range", move |a: Value, b: Value| -> Result<Value, Error> {
        let (first, last) = (to_date(&a, cal)?, to_date(&b, cal)?);
        if last < first {
            return Err(err(format!(
                "date_range({first}, {last}): the end is before the start"
            )));
        }
        let n = (last - first).num_days() as u64 + 1;
        Ok(Period::value(Period::of(Kind::Days(n), first, cal)))
    });
    env.add_function("month_of", move |y: i32, m: u32| -> Result<Value, Error> {
        Ok(Period::value(Period::of(Kind::Month, ymd(y, m, 1)?, cal)))
    });
    env.add_function("quarter_of", move |y: i32, q: u32| -> Result<Value, Error> {
        if !(1..=4).contains(&q) {
            return Err(err(format!("quarter_of({y}, {q}): the quarter must be 1 to 4")));
        }
        Ok(Period::value(Period::of(
            Kind::Quarter,
            ymd(y, (q - 1) * 3 + 1, 1)?,
            cal,
        )))
    });
    env.add_function("year_of", move |y: i32| -> Result<Value, Error> {
        Ok(Period::value(Period::of(Kind::Year, ymd(y, 1, 1)?, cal)))
    });
    env.add_function("week_of", move |y: i32, w: u32| -> Result<Value, Error> {
        Ok(Period::value(Period::of(
            Kind::Week,
            cal.week_of(y, w).map_err(err)?,
            cal,
        )))
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cal(tz: &str) -> Calendar {
        Calendar {
            tz: parse_tz(tz).unwrap(),
            ..Calendar::default()
        }
    }

    fn render_with(c: Calendar, today: &str, src: &str) -> String {
        let mut env = Environment::new();
        env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
        let today = parse_date(today).unwrap();
        register(&mut env, c, today);
        let d = Date::value(today, c);
        env.render_str(src, minijinja::context! { d => d })
            .unwrap_or_else(|e| format!("ERROR: {e:#}"))
    }

    fn render(today: &str, src: &str) -> String {
        render_with(Calendar::default(), today, src)
    }

    #[test]
    fn date_navigation_table() {
        let cases = [
            ("2026-03-15", "{{ d }}", "2026-03-15"),
            (
                "2026-03-15",
                "{{ d.prev_day }} {{ d.next_day }}",
                "2026-03-14 2026-03-16",
            ),
            ("2026-03-01", "{{ d.prev_day }}", "2026-02-28"),
            ("2024-03-01", "{{ d.prev_day }}", "2024-02-29"),
            ("2026-01-31", "{{ d.add(months=1) }}", "2026-02-28"),
            ("2024-01-31", "{{ d.add(months=1) }}", "2024-02-29"),
            (
                "2024-02-29",
                "{{ d.add(years=1) }} {{ d.add(years=-4) }}",
                "2025-02-28 2020-02-29",
            ),
            (
                "2026-03-15",
                "{{ d.add(days=-15) }} {{ d.add(weeks=2) }}",
                "2026-02-28 2026-03-29",
            ),
            (
                "2026-03-15",
                "{{ d.month_start }} {{ d.month_end }}",
                "2026-03-01 2026-03-31",
            ),
            (
                "2026-02-10",
                "{{ d.month_end }} {{ d.days_in_month }}",
                "2026-02-28 28",
            ),
            (
                "2026-05-20",
                "{{ d.quarter }} {{ d.quarter_start }} {{ d.quarter_end }}",
                "2 2026-04-01 2026-06-30",
            ),
            (
                "2026-05-20",
                "{{ d.year_start }} {{ d.year_end }} {{ d.day_of_year }}",
                "2026-01-01 2026-12-31 140",
            ),
            // 2026-03-15 is a Sunday.
            (
                "2026-03-15",
                "{{ d.weekday }} {{ d.week_start }} {{ d.week_end }}",
                "7 2026-03-09 2026-03-15",
            ),
            (
                "2026-03-15",
                "{{ d.year }}-{{ d.month }}-{{ d.day }}",
                "2026-3-15",
            ),
            (
                "2026-03-15",
                "{{ d.yyyymmdd }} {{ d.ddmmyyyy }} {{ d.yyyy }}{{ d.mm }}{{ d.dd }} {{ d.iso }}",
                "20260315 15032026 20260315 2026-03-15",
            ),
            ("2026-03-15", "{{ d.format('%d/%m/%Y') }}", "15/03/2026"),
        ];
        for (today, src, want) in cases {
            assert_eq!(render(today, src), want, "{src} on {today}");
        }
    }

    #[test]
    fn periods_table() {
        let p = |x: &str| format!("{{{{ {x}.start.date }}}}..{{{{ {x}.end.date }}}} {{{{ {x}.days }}}}");
        let cases = [
            ("2026-03-15", p("d.prev_month"), "2026-02-01..2026-02-28 28"),
            ("2026-01-10", p("d.prev_month"), "2025-12-01..2025-12-31 31"),
            ("2026-12-10", p("d.next_month"), "2027-01-01..2027-01-31 31"),
            ("2026-03-15", p("d.this_month"), "2026-03-01..2026-03-31 31"),
            ("2026-02-15", p("d.prev_quarter"), "2025-10-01..2025-12-31 92"),
            ("2026-02-15", p("d.next_quarter"), "2026-04-01..2026-06-30 91"),
            ("2026-02-15", p("d.prev_year"), "2025-01-01..2025-12-31 365"),
            ("2024-02-15", p("d.this_year"), "2024-01-01..2024-12-31 366"),
            ("2026-03-15", p("d.prev_week"), "2026-03-02..2026-03-08 7"),
            ("2026-03-15", p("d.next_week"), "2026-03-16..2026-03-22 7"),
            ("2026-03-15", p("d.as_period()"), "2026-03-15..2026-03-15 1"),
            ("2026-03-15", p("d.prev_month.prev"), "2026-01-01..2026-01-31 31"),
            ("2026-03-15", p("d.prev_month.next"), "2026-03-01..2026-03-31 31"),
            ("2026-03-15", p("month_of(2024, 2)"), "2024-02-01..2024-02-29 29"),
            (
                "2026-03-15",
                p("quarter_of(2026, 4)"),
                "2026-10-01..2026-12-31 92",
            ),
            ("2026-03-15", p("year_of(2025)"), "2025-01-01..2025-12-31 365"),
            (
                "2026-03-15",
                p("date_range('2026-01-05', '2026-01-11').next"),
                "2026-01-12..2026-01-18 7",
            ),
            ("2026-03-15", "{{ d.prev_month }}".into(), "2026-02-01/2026-02-28"),
            (
                "2026-03-15",
                "{{ d.prev_month.start }} {{ d.prev_month.end }}".into(),
                "2026-02-01 00:00:00 2026-02-28 23:59:59.999999",
            ),
            (
                "2026-03-15",
                "{{ d.prev_month.next.start }}".into(),
                "2026-03-01 00:00:00",
            ),
            (
                "2026-03-15",
                "{{ d.prev_month.first_day }} {{ d.prev_month.last_day }}".into(),
                "2026-02-01 2026-02-28",
            ),
        ];
        for (today, src, want) in cases {
            assert_eq!(render(today, &src), want, "{src} on {today}");
        }
    }

    #[test]
    fn iso_and_us_week_numbers() {
        let iso = Calendar::default();
        let us = Calendar {
            week_start: WeekStart::Sunday,
            numbering: WeekNumbering::Us,
            ..Calendar::default()
        };
        let w = "{{ d.week_year }}-W{{ d.week }}";
        // ISO: 2021-01-03 (Sunday) is in 2020's week 53; 2024-12-30 is in 2025's week 1.
        assert_eq!(render_with(iso, "2021-01-03", w), "2020-W53");
        assert_eq!(render_with(iso, "2021-01-04", w), "2021-W1");
        assert_eq!(render_with(iso, "2024-12-30", w), "2025-W1");
        // US: the week holding 1 January is week 1, weeks start on Sunday.
        assert_eq!(render_with(us, "2021-01-02", w), "2021-W1");
        assert_eq!(render_with(us, "2021-01-03", w), "2021-W2");
        assert_eq!(render_with(us, "2024-12-31", w), "2024-W53");
        let wk = "{{ week_of(2026, 1).start.date }} {{ week_of(2026, 1).end.date }}";
        assert_eq!(render_with(iso, "2026-03-15", wk), "2025-12-29 2026-01-04");
        assert_eq!(render_with(us, "2026-03-15", wk), "2025-12-28 2026-01-03");
        assert_eq!(
            render_with(iso, "2026-03-15", "{{ week_of(2020, 53).start.date }}"),
            "2020-12-28"
        );
        assert!(render_with(iso, "2026-03-15", "{{ week_of(2021, 53) }}").starts_with("ERROR"));
        // Sunday weeks move week_start/week_end and week periods.
        assert_eq!(
            render_with(
                us,
                "2026-03-18",
                "{{ d.week_start }} {{ d.week_end }} {{ d.prev_week.start.date }}"
            ),
            "2026-03-15 2026-03-21 2026-03-08"
        );
    }

    #[test]
    fn constructors_and_errors() {
        assert_eq!(
            render(
                "2026-03-15",
                "{{ date('2026-07-04') }} {{ date(2026, 7, 4).weekday }}"
            ),
            "2026-07-04 6"
        );
        assert_eq!(
            render("2026-03-15", "{{ ('2026-07-04' | as_date).month_end }}"),
            "2026-07-31"
        );
        assert_eq!(render("2026-03-15", "{{ '20260704' | as_date }}"), "2026-07-04");
        assert!(render("2026-03-15", "{{ date('2026-02-30') }}").contains("isn't a date"));
        assert!(render("2026-03-15", "{{ date(2026, 2, 30) }}").contains("isn't a date"));
        assert!(render("2026-03-15", "{{ 'soon' | as_date }}").contains("isn't a date"));
        assert!(render("2026-03-15", "{{ quarter_of(2026, 5) }}").contains("1 to 4"));
        assert!(
            render("2026-03-15", "{{ date_range('2026-02-01', '2026-01-01') }}").contains("before the start")
        );
        assert!(render("2026-03-15", "{{ d.add(fortnights=1) }}").starts_with("ERROR"));
        assert!(render("2026-03-15", "{{ d.nope }}").starts_with("ERROR"));
    }

    #[test]
    fn datetimes_timezones_and_unix() {
        let syd = cal("Australia/Sydney");
        // Sydney is UTC+11 in March (daylight time).
        assert_eq!(
            render_with(
                syd,
                "2026-03-15",
                "{{ d.start }} | {{ d.start.utc }} | {{ d.start.iso }}"
            ),
            "2026-03-15 00:00:00 | 2026-03-14 13:00:00 | 2026-03-15T00:00:00+11:00"
        );
        assert_eq!(
            render_with(
                syd,
                "2026-03-15",
                "{{ d.unix }} {{ d.start.unix }} {{ d.start.utc.unix }} {{ d.unix_ms }}"
            ),
            "1773493200 1773493200 1773493200 1773493200000"
        );
        assert_eq!(render("2026-03-15", "{{ d.unix }}"), "1773532800");
        assert_eq!(
            render_with(syd, "2026-03-15", "{{ d.end.iso }}"),
            "2026-03-15T23:59:59.999999+11:00"
        );
        assert_eq!(
            render_with(
                syd,
                "2026-03-15",
                "{{ d.start.tz('Europe/London') }} {{ d.start.tz('Europe/London').offset }}"
            ),
            "2026-03-14 13:00:00 +00:00"
        );
        assert!(render_with(syd, "2026-03-15", "{{ d.start.tz('Mars/Base') }}").contains("isn't a timezone"));
        // 2026-04-05: Sydney leaves daylight time at 03:00, so the day has 25 hours.
        assert_eq!(
            render_with(
                syd,
                "2026-04-05",
                "{{ d.end.unix - d.start.unix + 1 }} {{ d.next_day.start.iso }}"
            ),
            "90000 2026-04-06T00:00:00+10:00"
        );
        // Datetime strings: in the run zone unless they carry an offset.
        assert_eq!(
            render_with(syd, "2026-03-15", "{{ datetime('2026-03-15 10:30:00').utc }}"),
            "2026-03-14 23:30:00"
        );
        assert_eq!(
            render_with(syd, "2026-03-15", "{{ datetime('2026-03-15T10:30:00Z') }}"),
            "2026-03-15 21:30:00"
        );
        assert_eq!(
            render_with(syd, "2026-03-15", "{{ datetime('2026-03-15 10:30:00.25').time }}"),
            "10:30:00.250000"
        );
        assert_eq!(
            render_with(
                syd,
                "2026-03-15",
                "{{ ('2026-03-15 10:30' | as_datetime).add(hours=2, days=1) }}"
            ),
            "2026-03-16 12:30:00"
        );
        assert_eq!(
            render_with(
                syd,
                "2026-03-15",
                "{{ datetime('2026-03-15 10:30').format('%H%M') }} {{ datetime('2026-03-15 10:30').date.prev_day }}"
            ),
            "1030 2026-03-14"
        );
        assert!(render_with(syd, "2026-03-15", "{{ datetime('later') }}").contains("isn't a date and time"));
    }

    #[test]
    fn period_presets() {
        let p = |x: &str| format!("{{% set p = {x} %}}{{{{ p.start.date }}}}..{{{{ p.end.date }}}}");
        // 2026-03-18 is a Wednesday.
        let cases = [
            ("period('today')", "2026-03-18..2026-03-18"),
            ("period('yesterday')", "2026-03-17..2026-03-17"),
            ("period('this_week')", "2026-03-16..2026-03-22"),
            ("period('last_week')", "2026-03-09..2026-03-15"),
            ("period('this_month')", "2026-03-01..2026-03-31"),
            ("period('last_month')", "2026-02-01..2026-02-28"),
            ("period('mtd')", "2026-03-01..2026-03-18"),
            ("period('this_quarter')", "2026-01-01..2026-03-31"),
            ("period('last_quarter')", "2025-10-01..2025-12-31"),
            ("period('qtd')", "2026-01-01..2026-03-18"),
            ("period('this_year')", "2026-01-01..2026-12-31"),
            ("period('last_year')", "2025-01-01..2025-12-31"),
            ("period('ytd')", "2026-01-01..2026-03-18"),
            ("period('last_n_days', n=7)", "2026-03-11..2026-03-17"),
            ("period('last_n_months', n=3)", "2025-12-01..2026-02-28"),
            // Year boundary, and an explicit as_of.
            (
                "period('last_month', as_of='2026-01-10')",
                "2025-12-01..2025-12-31",
            ),
            ("period('ytd', as_of=date(2026, 1, 1))", "2026-01-01..2026-01-01"),
            (
                "period('last_week', as_of='2026-01-01')",
                "2025-12-22..2025-12-28",
            ),
        ];
        for (x, want) in cases {
            assert_eq!(render("2026-03-18", &p(x)), want, "{x}");
        }
        let e = render("2026-03-18", "{{ period('fortnight') }}");
        assert!(e.contains("unknown period; use one of today, yesterday"), "{e}");
        assert!(render("2026-03-18", "{{ period('last_n_days') }}").contains("needs a positive `n`"));
        assert!(render("2026-03-18", "{{ period('last_n_days', n=0) }}").contains("needs a positive `n`"));
    }

    #[test]
    fn datetime_add_across_daylight_saving_changes() {
        let ny = cal("America/New_York");
        // 2026-03-08 02:30 doesn't exist in New York: the clocks skip from 02:00 to 03:00.
        assert_eq!(
            render_with(
                ny,
                "2026-03-07",
                "{{ datetime('2026-03-07 02:30').add(days=1).iso }}"
            ),
            "2026-03-08T03:30:00-04:00"
        );
        // 2026-11-01 01:30 happens twice; an hour after the first is the second.
        assert_eq!(
            render_with(
                ny,
                "2026-11-01",
                "{% set t = datetime('2026-11-01 01:30') %}{{ t.iso }} {{ t.add(hours=1).iso }} {{ t.add(hours=1).unix - t.unix }}"
            ),
            "2026-11-01T01:30:00-04:00 2026-11-01T01:30:00-05:00 3600"
        );
        // A day later from the second 01:30 keeps the clock time.
        assert_eq!(
            render_with(
                ny,
                "2026-11-01",
                "{{ datetime('2026-11-01 01:30').add(hours=1).add(days=1).iso }}"
            ),
            "2026-11-02T01:30:00-05:00"
        );
    }

    #[test]
    fn dst_gap_starts_at_first_valid_instant() {
        // Santiago springs forward at midnight: 2026-09-06 begins at 01:00.
        let c = cal("America/Santiago");
        assert_eq!(
            render_with(c, "2026-09-06", "{{ d.start }}"),
            "2026-09-06 01:00:00"
        );
    }

    #[test]
    fn now_renders_in_the_run_zone() {
        let at = Utc.with_ymd_and_hms(2026, 3, 14, 20, 0, 0).unwrap();
        let v = DateTime::now(at, cal("Australia/Sydney"));
        assert_eq!(v.to_string(), "2026-03-15 07:00:00");
    }
}
