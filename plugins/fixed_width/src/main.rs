//! DRE format plugin `fixed_width`: each column at a fixed width, no delimiter.
//!
//! Options: `columns` (required), each `{name, width | picture, type, header, align, pad, truncate,
//! decimals, decimal_point, sign, date_format, null_fill}`, where `name` is a result-set column;
//! `header` (false): a first record of column labels; `line_ending` (`"\r\n"`); `encoding`
//! (`utf-8`); `line_breaks` (`error`|`replace`): a value with a line break would split its record,
//! so it's an error unless `replace` turns each break into a space. Tabs and other characters are
//! written as they are.
//!
//! Every field is left-aligned and space-filled unless `align` and `pad` say otherwise, whatever
//! the column's type. A column given a number option (`decimals`, `decimal_point`, `sign`,
//! `type: number` or a 9 picture) is laid out as a number: rounded to `decimals` and never cut,
//! so one that doesn't fit is an error. `picture` takes a COBOL PIC clause (`X(20)`, `9(7)V99`,
//! `S9(5)V99`) and sets the width and the number layout from it; a 9 picture is right-aligned and
//! zero-filled, as in COBOL. Trailer records are format-pack territory.

use std::fmt::Write as _;
use std::fs::File;
use std::io::{BufWriter, Write};

use arrow::array::Array;
use arrow::datatypes::DataType;
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dre_protocol::options::{OptionField, OptionType};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use serde_json::{Map, Value};

const KEYS: &[&str] = &[
    "name",
    "type",
    "width",
    "picture",
    "header",
    "align",
    "pad",
    "truncate",
    "decimals",
    "decimal_point",
    "sign",
    "date_format",
    "null_fill",
];

#[derive(Clone, Copy, PartialEq)]
enum Sign {
    /// `-` before a negative number, nothing before a positive one.
    Leading,
    /// `+` or `-` before every number.
    Always,
    /// `+` or `-` after every number.
    Trailing,
    /// No extra character: the last digit carries the sign (COBOL zoned decimal).
    Overpunch,
    /// Unsigned: a negative number is an error.
    None,
}

/// How a number is laid out, from `decimals`, `decimal_point` and `sign`, or from a picture.
#[derive(Clone, Copy)]
struct Num {
    decimals: Option<usize>,
    /// `None` is an implied point: the digits are written without it.
    point: Option<char>,
    sign: Sign,
}

struct Column {
    name: String,
    label: String,
    width: usize,
    /// Default: left, space-filled, unless a 9 picture says right and zero-filled.
    right: Option<bool>,
    pad: Option<char>,
    /// A 9 picture: right-aligned and zero-filled unless told otherwise, as in COBOL.
    zoned: bool,
    truncate: bool,
    /// Set when the options (`type: number`, a number option or a 9 picture) make this a number
    /// column whatever its type.
    num: Option<Num>,
    /// `type: text` or a `picture` of `X`s: text, whatever the column's type.
    text: bool,
    date_format: Option<String>,
    null_fill: Option<char>,
}

/// A COBOL PIC clause: `X(n)` for text, or `[S]9(n)[V|.]9(m)` for a number. Returns the width,
/// and for a number its layout.
fn picture(p: &str) -> std::result::Result<(usize, Option<Num>), String> {
    let bad = || format!("`picture` `{p}` isn't a PIC clause like X(20), 9(7)V99 or S9(5)V99");
    let s = p.trim().to_ascii_uppercase();
    let (signed, s) = match s.strip_prefix('S') {
        Some(rest) => (true, rest.to_string()),
        None => (false, s),
    };
    // Expand `9(3)` to `999`.
    let mut sym = Vec::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if chars.peek() == Some(&'(') {
            chars.next();
            let n: String = chars.by_ref().take_while(|c| *c != ')').collect();
            let n: usize = n.parse().ok().filter(|n| *n > 0).ok_or_else(bad)?;
            sym.extend(std::iter::repeat_n(c, n));
        } else {
            sym.push(c);
        }
    }
    if !sym.is_empty() && sym.iter().all(|c| *c == 'X') {
        return if signed { Err(bad()) } else { Ok((sym.len(), None)) };
    }
    let points: Vec<usize> = sym
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c, 'V' | '.'))
        .map(|(i, _)| i)
        .collect();
    let digits = sym.iter().filter(|c| **c == '9').count();
    if digits == 0 || points.len() > 1 || sym.iter().any(|c| !matches!(c, '9' | 'V' | '.')) {
        return Err(bad());
    }
    let (decimals, point) = match points.first() {
        Some(&i) => (sym.len() - i - 1, (sym[i] == '.').then_some('.')),
        None => (0, None),
    };
    let visible = usize::from(point.is_some());
    Ok((
        digits + visible,
        Some(Num {
            decimals: Some(decimals),
            point,
            sign: if signed { Sign::Overpunch } else { Sign::None },
        }),
    ))
}

fn sign(s: &str) -> Option<Sign> {
    Some(match s {
        "leading" => Sign::Leading,
        "always" => Sign::Always,
        "trailing" => Sign::Trailing,
        "overpunch" => Sign::Overpunch,
        "none" => Sign::None,
        _ => return None,
    })
}

fn one_char(c: &Map<String, Value>, key: &str) -> Option<char> {
    c.get(key).and_then(Value::as_str).and_then(|s| s.chars().next())
}

/// Problems with the `n`th (1-based) entry of `columns:`.
fn column_errors(n: usize, c: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(m) = c.as_object() else {
        return vec![format!("column {n} must be a map with `name` and `width`")];
    };
    let label = match m.get("name").and_then(Value::as_str) {
        Some(s) => format!("column `{s}`"),
        None => {
            errs.push(format!("column {n} has no `name`"));
            format!("column {n}")
        }
    };
    for k in m.keys() {
        if !KEYS.contains(&k.as_str()) {
            errs.push(format!("{label} has unknown key `{k}`"));
        }
    }
    let pic = match m.get("picture") {
        None => None,
        Some(p) => match p.as_str().map(picture) {
            Some(Ok(p)) => Some(p),
            Some(Err(e)) => {
                errs.push(format!("{label}: {e}"));
                None
            }
            None => {
                errs.push(format!("{label} `picture` must be a string like 9(7)V99"));
                None
            }
        },
    };
    if m.contains_key("picture") {
        for k in ["width", "decimals", "decimal_point"] {
            if m.contains_key(k) {
                errs.push(format!(
                    "{label} has both `picture` and `{k}`; the picture sets it"
                ));
            }
        }
    } else {
        match m.get("width").and_then(Value::as_u64) {
            Some(w) if w > 0 => {}
            _ => errs.push(format!(
                "{label} needs a positive whole-number `width`, or a `picture`"
            )),
        }
    }
    if let Some(a) = m.get("align")
        && !matches!(a.as_str(), Some("left" | "right"))
    {
        errs.push(format!("{label} `align` must be `left` or `right`"));
    }
    for k in ["pad", "null_fill"] {
        if let Some(p) = m.get(k)
            && p.as_str().is_none_or(|s| s.chars().count() != 1)
        {
            errs.push(format!("{label} `{k}` must be a single character"));
        }
    }
    if m.get("truncate").is_some_and(|v| !v.is_boolean()) {
        errs.push(format!("{label} `truncate` must be true or false"));
    }
    if m.get("header").is_some_and(|v| !v.is_string()) {
        errs.push(format!(
            "{label} `header` must be text: the column's label in the header record"
        ));
    }
    let number_keys = ["sign", "decimals", "decimal_point"]
        .iter()
        .any(|k| m.contains_key(*k));
    match m.get("type").map(Value::as_str) {
        None | Some(Some("number")) => {}
        Some(Some("text")) if number_keys || matches!(pic, Some((_, Some(_)))) => {
            errs.push(format!("{label} has `type: text` and a number option"));
        }
        Some(Some("text")) => {}
        _ => errs.push(format!("{label} `type` must be `text` or `number`")),
    }
    if m.get("type").and_then(Value::as_str) == Some("number") && matches!(pic, Some((_, None))) {
        errs.push(format!("{label} has `type: number` and a text `picture` (X)"));
    }
    if m.get("decimals")
        .is_some_and(|d| d.as_u64().is_none_or(|d| d > 38))
    {
        errs.push(format!("{label} `decimals` must be a whole number from 0 to 38"));
    }
    match m.get("decimal_point").map(Value::as_str) {
        None | Some(Some("." | "," | "implied")) => {}
        _ => errs.push(format!(
            "{label} `decimal_point` must be \".\", \",\" or `implied`"
        )),
    }
    if m.get("decimal_point").and_then(Value::as_str) == Some("implied") && !m.contains_key("decimals") {
        errs.push(format!(
            "{label} has `decimal_point: implied` without `decimals`: say how many digits are decimals"
        ));
    }
    if let Some(s) = m.get("sign")
        && s.as_str().and_then(sign).is_none()
    {
        errs.push(format!(
            "{label} `sign` must be `leading`, `always`, `trailing`, `overpunch` or `none`"
        ));
    }
    if matches!(pic, Some((_, None))) && number_keys {
        errs.push(format!("{label} has a text `picture` (X) and a number option"));
    }
    if let Some(f) = m.get("date_format") {
        match f.as_str() {
            Some(f) if !bad_strftime(f) => {}
            _ => errs.push(format!(
                "{label} `date_format` must be a strftime pattern like \"%Y%m%d\""
            )),
        }
    }
    errs
}

fn bad_strftime(f: &str) -> bool {
    chrono::format::StrftimeItems::new(f).any(|i| matches!(i, chrono::format::Item::Error))
}

fn columns(v: Option<&Value>) -> Result<Vec<Column>> {
    let list = v
        .and_then(Value::as_array)
        .ok_or("fixed_width output needs a `columns:` list")?;
    list.iter()
        .enumerate()
        .map(|(i, c)| {
            if let Some(e) = column_errors(i + 1, c).into_iter().next() {
                return Err(e.into());
            }
            let m = c.as_object().expect("checked");
            let name = m["name"].as_str().expect("checked").to_string();
            let pic = m
                .get("picture")
                .and_then(Value::as_str)
                .map(|p| picture(p).expect("checked"));
            let width = match pic {
                Some((w, _)) => w,
                None => m["width"].as_u64().expect("checked") as usize,
            };
            let explicit_sign = m.get("sign").and_then(Value::as_str).and_then(sign);
            let forced = m.get("type").and_then(Value::as_str);
            let num = match pic {
                Some((_, Some(mut n))) => {
                    if let Some(s) = explicit_sign {
                        n.sign = s;
                    }
                    Some(n)
                }
                Some((_, None)) => None,
                None if forced == Some("number")
                    || ["decimals", "decimal_point", "sign"]
                        .iter()
                        .any(|k| m.contains_key(*k)) =>
                {
                    Some(Num {
                        decimals: m.get("decimals").and_then(Value::as_u64).map(|d| d as usize),
                        point: match m.get("decimal_point").and_then(Value::as_str) {
                            Some("implied") => None,
                            Some(",") => Some(','),
                            _ => Some('.'),
                        },
                        sign: explicit_sign.unwrap_or(Sign::Leading),
                    })
                }
                None => None,
            };
            // A separate sign takes a character of its own beside a picture's digits.
            let width = match (pic, num) {
                (Some(_), Some(n)) if matches!(n.sign, Sign::Leading | Sign::Always | Sign::Trailing) => {
                    width + 1
                }
                _ => width,
            };
            Ok(Column {
                label: m
                    .get("header")
                    .and_then(Value::as_str)
                    .unwrap_or(&name)
                    .to_string(),
                name,
                width,
                right: m.get("align").and_then(Value::as_str).map(|a| a == "right"),
                pad: one_char(m, "pad"),
                truncate: m.get("truncate").and_then(Value::as_bool).unwrap_or(false),
                num,
                text: matches!(pic, Some((_, None))) || forced == Some("text"),
                zoned: matches!(pic, Some((_, Some(_)))),
                date_format: m.get("date_format").and_then(Value::as_str).map(str::to_string),
                null_fill: one_char(m, "null_fill"),
            })
        })
        .collect()
}

fn is_temporal(t: &DataType) -> bool {
    matches!(
        t,
        DataType::Date32
            | DataType::Date64
            | DataType::Timestamp(..)
            | DataType::Time32(_)
            | DataType::Time64(_)
    )
}

fn is_text(t: &DataType) -> bool {
    matches!(t, DataType::Utf8 | DataType::LargeUtf8 | DataType::Utf8View)
}

/// A column fixed to its result-set type: whether it's a number, its alignment and pad.
struct Layout {
    num: Option<Num>,
    right: bool,
    pad: char,
}

impl Column {
    fn layout(&self, t: &DataType) -> Result<Layout> {
        let num = if self.text {
            None
        } else if let Some(n) = self.num {
            if !(t.is_numeric() || is_text(t)) {
                return Err(format!(
                    "column `{}` is {t}, so it can't take number options (`decimals`, `decimal_point`, `sign`, a 9 `picture`)",
                    self.name
                )
                .into());
            }
            Some(n)
        } else {
            None
        };
        if self.date_format.is_some() && !is_temporal(t) {
            return Err(format!(
                "column `{}` is {t}, not a date, time or timestamp, so `date_format` doesn't apply",
                self.name
            )
            .into());
        }
        let right = self.right.unwrap_or(self.zoned);
        let pad = self.pad.unwrap_or(if self.zoned { '0' } else { ' ' });
        Ok(Layout { num, right, pad })
    }

    fn text_cell(&self, l: &Layout, v: &str, row: u64) -> Result<String> {
        let len = v.chars().count();
        if len > self.width {
            if !self.truncate {
                return Err(format!(
                    "row {row}, column `{}`: `{v}` is {len} characters, wider than its width of {} (set `truncate: true` to cut it)",
                    self.name, self.width
                )
                .into());
            }
            return Ok(v.chars().take(self.width).collect());
        }
        let fill: String = std::iter::repeat_n(l.pad, self.width - len).collect();
        Ok(if !l.right {
            format!("{v}{fill}")
        } else if l.pad == '0' && v.starts_with('-') {
            // Zero fill goes after the sign: -0042, not 00-42.
            format!("-{fill}{}", &v[1..])
        } else {
            format!("{fill}{v}")
        })
    }

    fn number_cell(&self, l: &Layout, n: Num, v: &str, row: u64) -> Result<String> {
        let err = |why: String| format!("row {row}, column `{}`: {why}", self.name);
        let d = parse_decimal(v).ok_or_else(|| err(format!("`{v}` isn't a number")))?;
        let (int, frac) = match n.decimals {
            Some(places) => round(&d.int, &d.frac, places),
            None => (d.int.clone(), d.frac.clone()),
        };
        let negative = d.negative && (int.bytes().chain(frac.bytes())).any(|b| b != b'0');
        let int = int.trim_start_matches('0');
        let mut body = match n.point {
            Some(p) if !frac.is_empty() => format!("{}{p}{frac}", if int.is_empty() { "0" } else { int }),
            Some(_) => {
                if int.is_empty() {
                    "0".to_string()
                } else {
                    int.to_string()
                }
            }
            None if int.is_empty() && frac.is_empty() => "0".to_string(),
            None => format!("{int}{frac}"),
        };
        let (mut prefix, mut suffix) = ("", "");
        match n.sign {
            Sign::Leading if negative => prefix = "-",
            Sign::Leading => {}
            Sign::Always => prefix = if negative { "-" } else { "+" },
            Sign::Trailing => suffix = if negative { "-" } else { "+" },
            Sign::Overpunch => {
                let last = body.pop().expect("a number has a digit");
                let d = last.to_digit(10).expect("the last character is a digit") as u8;
                body.push(match (negative, d) {
                    (false, 0) => '{',
                    (true, 0) => '}',
                    (false, d) => (b'A' + d - 1) as char,
                    (true, d) => (b'J' + d - 1) as char,
                });
            }
            Sign::None if negative => {
                return Err(err(format!(
                    "`{v}` is negative, but the column is unsigned (give it a `sign`, or an S picture)"
                ))
                .into());
            }
            Sign::None => {}
        }
        let len = prefix.len() + body.chars().count() + suffix.len();
        if len > self.width {
            return Err(err(format!(
                "`{v}` needs {len} characters, wider than its width of {}; numbers are never cut",
                self.width
            ))
            .into());
        }
        let fill: String = std::iter::repeat_n(l.pad, self.width - len).collect();
        Ok(if !l.right {
            format!("{prefix}{body}{suffix}{fill}")
        } else if l.pad == '0' {
            // Zero fill goes after a leading sign: -0042, not 00-42.
            format!("{prefix}{fill}{body}{suffix}")
        } else {
            format!("{fill}{prefix}{body}{suffix}")
        })
    }
}

struct Decimal {
    negative: bool,
    int: String,
    frac: String,
}

/// Reads a plain or scientific decimal (`-12.50`, `.5`, `1e-7`) into its digits.
fn parse_decimal(v: &str) -> Option<Decimal> {
    let v = v.trim();
    if v.contains(['e', 'E']) {
        let f: f64 = v.parse().ok().filter(|f: &f64| f.is_finite())?;
        return parse_decimal(&format!("{f}"));
    }
    let (negative, v) = match v.as_bytes().first()? {
        b'-' => (true, &v[1..]),
        b'+' => (false, &v[1..]),
        _ => (false, v),
    };
    let (int, frac) = v.split_once('.').unwrap_or((v, ""));
    let digits = |s: &str| s.bytes().all(|b| b.is_ascii_digit());
    if (int.is_empty() && frac.is_empty()) || !digits(int) || !digits(frac) {
        return None;
    }
    Some(Decimal {
        negative,
        int: int.to_string(),
        frac: frac.to_string(),
    })
}

/// Rounds `int.frac` to `places` decimals, half away from zero.
fn round(int: &str, frac: &str, places: usize) -> (String, String) {
    if frac.len() <= places {
        return (int.to_string(), format!("{frac:0<places$}"));
    }
    let mut digits: Vec<u8> = format!("{int}{}", &frac[..places]).into_bytes();
    if frac.as_bytes()[places] >= b'5' {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, b'1');
                break;
            }
            i -= 1;
            if digits[i] == b'9' {
                digits[i] = b'0';
            } else {
                digits[i] += 1;
                break;
            }
        }
    }
    let s = String::from_utf8(digits).expect("ascii digits");
    let (i, f) = s.split_at(s.len() - places);
    (i.to_string(), f.to_string())
}

struct FixedWidth;

impl Format for FixedWidth {
    fn options(&self) -> Vec<OptionField> {
        use OptionType::*;
        vec![
            OptionField::new(
                "columns",
                List,
                "the record layout, in order: {name, width or picture, type, header, align, pad, truncate, decimals, decimal_point, sign, date_format, null_fill}",
            )
            .required(),
            OptionField::new("header", Boolean, "write a first record of column labels").default(false),
            OptionField::new("line_ending", String, "ends each record")
                .choices(&["\n", "\r\n"])
                .default("\r\n"),
            OptionField::new("encoding", String, "text encoding, e.g. utf-8, latin1").default("utf-8"),
            OptionField::new(
                "line_breaks",
                String,
                "a value with a line break: fail, or write a space",
            )
            .choices(&["error", "replace"])
            .default("error"),
        ]
    }

    fn validate(&self, o: &Map<String, Value>) -> Vec<String> {
        let mut errs = Vec::new();
        if let Some(e) = o.get("encoding").and_then(Value::as_str)
            && encoding_rs::Encoding::for_label(e.as_bytes()).is_none_or(|x| x.output_encoding() != x)
        {
            errs.push(format!("unknown or unsupported `encoding` \"{e}\""));
        }
        match o.get("columns").and_then(Value::as_array) {
            Some(cols) if cols.is_empty() => errs.push("`columns` must not be empty".to_string()),
            Some(cols) => {
                for (i, c) in cols.iter().enumerate() {
                    errs.extend(column_errors(i + 1, c));
                }
            }
            None => {}
        }
        errs
    }

    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        if req.result_sets.len() != 1 {
            return Err(format!(
                "fixed_width writes exactly one result set per file, got {}",
                req.result_sets.len()
            )
            .into());
        }
        let cols = columns(req.options.get("columns"))?;
        let header = req
            .options
            .get("header")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let line_ending = req
            .options
            .get("line_ending")
            .and_then(Value::as_str)
            .unwrap_or("\r\n")
            .to_string();
        let label = req
            .options
            .get("encoding")
            .and_then(Value::as_str)
            .unwrap_or("utf-8");
        let enc = encoding_rs::Encoding::for_label(label.as_bytes())
            .filter(|e| e.output_encoding() == *e)
            .ok_or_else(|| format!("unsupported `encoding` `{label}`"))?;
        let replace_breaks = match req.options.get("line_breaks").and_then(Value::as_str) {
            None | Some("error") => false,
            Some("replace") => true,
            Some(o) => return Err(format!("`line_breaks` must be `error` or `replace`, not `{o}`").into()),
        };
        let mut rs = sets.next_set()?.expect("one result set");
        let idx: Vec<usize> = cols
            .iter()
            .map(|c| {
                rs.schema.index_of(&c.name).map_err(|_| {
                    let have: Vec<String> = rs.schema.fields().iter().map(|f| f.name().clone()).collect();
                    format!(
                        "column `{}` isn't in the result set (it has: {})",
                        c.name,
                        have.join(", ")
                    )
                    .into()
                })
            })
            .collect::<Result<_>>()?;
        let layouts: Vec<Layout> = cols
            .iter()
            .zip(&idx)
            .map(|(c, &i)| c.layout(rs.schema.field(i).data_type()))
            .collect::<Result<_>>()?;
        let opts: Vec<FormatOptions<'_>> = cols
            .iter()
            .map(|c| {
                let o = FormatOptions::default()
                    .with_timestamp_format(Some("%Y-%m-%d %H:%M:%S"))
                    .with_timestamp_tz_format(Some("%Y-%m-%d %H:%M:%S%:z"));
                match c.date_format.as_deref() {
                    Some(f) => o
                        .with_date_format(Some(f))
                        .with_datetime_format(Some(f))
                        .with_timestamp_format(Some(f))
                        .with_timestamp_tz_format(Some(f))
                        .with_time_format(Some(f)),
                    None => o,
                }
            })
            .collect();
        let file = File::create(&req.path).map_err(|e| format!("can't create {}: {e}", req.path))?;
        let mut w = BufWriter::new(file);
        let mut emit = |line: &str, what: &str| -> Result<()> {
            if enc == encoding_rs::UTF_8 {
                w.write_all(line.as_bytes())?;
            } else {
                let (bytes, _, bad) = enc.encode(line);
                if bad {
                    return Err(format!("{what} has characters that {} can't represent", enc.name()).into());
                }
                w.write_all(&bytes)?;
            }
            Ok(())
        };
        if header {
            let mut line = String::new();
            for c in &cols {
                let label: String = c.label.chars().take(c.width).collect();
                line.push_str(&format!("{label:<width$}", width = c.width));
            }
            line.push_str(&line_ending);
            emit(&line, "the header")?;
        }
        let mut row = 0u64;
        while let Some(batch) = rs.next_batch()? {
            let fmts: Vec<ArrayFormatter<'_>> = idx
                .iter()
                .zip(&opts)
                .map(|(&i, o)| ArrayFormatter::try_new(batch.column(i).as_ref(), o))
                .collect::<std::result::Result<_, _>>()?;
            for r in 0..batch.num_rows() {
                row += 1;
                let mut line = String::new();
                for (k, c) in cols.iter().enumerate() {
                    let l = &layouts[k];
                    if batch.column(idx[k]).is_null(r) {
                        let fill = c.null_fill.unwrap_or(l.pad);
                        line.extend(std::iter::repeat_n(fill, c.width));
                        continue;
                    }
                    let mut v = String::new();
                    write!(v, "{}", fmts[k].value(r))
                        .map_err(|_| format!("row {row}, column `{}`: can't format the value", c.name))?;
                    if let Some(n) = l.num {
                        line.push_str(&c.number_cell(l, n, &v, row)?);
                        continue;
                    }
                    let v = if !v.contains(['\r', '\n']) {
                        v
                    } else if replace_breaks {
                        v.replace("\r\n", " ").replace(['\r', '\n'], " ")
                    } else {
                        return Err(format!(
                            "row {row}, column `{}`: the value contains a line break, which would split the record (set `line_breaks: replace` to write a space instead)",
                            c.name
                        )
                        .into());
                    };
                    line.push_str(&c.text_cell(l, &v, row)?);
                }
                line.push_str(&line_ending);
                emit(&line, &format!("row {row}"))?;
            }
        }
        w.flush()?;
        Ok(vec![req.path.clone()])
    }
}

fn main() {
    serve_format(About::new("fixed_width", env!("CARGO_PKG_VERSION")), FixedWidth)
}
