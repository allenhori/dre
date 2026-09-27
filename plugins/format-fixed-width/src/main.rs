//! DRE format plugin `fixed_width`: each column at a fixed width, no header, no delimiter.
//!
//! Options: `columns` (required): `{name, width, align (left|right), pad, truncate}`, where
//! `name` is a result-set column; `line_ending` (`"\r\n"`); `encoding` (`utf-8`);
//! `line_breaks` (`error`|`replace`): a value with a line break would split its record, so it's
//! an error unless `replace` turns each break into a space. Tabs and other characters are
//! written as they are. The pad defaults to a space when left-aligned and `0` when
//! right-aligned. Header and trailer records are format-pack territory.

use std::fs::File;
use std::io::{BufWriter, Write};

use arrow::array::Array;
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use serde_json::Value;

struct Column {
    name: String,
    width: usize,
    right: bool,
    pad: char,
    truncate: bool,
}

fn columns(v: Option<&Value>) -> Result<Vec<Column>> {
    let list = v
        .and_then(Value::as_array)
        .ok_or("fixed_width output needs a `columns:` list")?;
    list.iter()
        .map(|c| {
            let name = c
                .get("name")
                .and_then(Value::as_str)
                .ok_or("every column needs a `name`")?
                .to_string();
            let width = c
                .get("width")
                .and_then(Value::as_u64)
                .filter(|w| *w > 0)
                .ok_or_else(|| format!("column `{name}` needs a positive `width`"))?;
            let right = match c.get("align").and_then(Value::as_str).unwrap_or("left") {
                "left" => false,
                "right" => true,
                a => return Err(format!("column `{name}`: `align` must be left or right, not `{a}`").into()),
            };
            let pad = match c.get("pad").and_then(Value::as_str) {
                Some(p) => p
                    .chars()
                    .next()
                    .ok_or_else(|| format!("column `{name}`: empty `pad`"))?,
                None if right => '0',
                None => ' ',
            };
            Ok(Column {
                name,
                width: width as usize,
                right,
                pad,
                truncate: c.get("truncate").and_then(Value::as_bool).unwrap_or(false),
            })
        })
        .collect()
}

impl Column {
    fn cell(&self, v: &str, row: u64) -> Result<String> {
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
        let fill: String = std::iter::repeat_n(self.pad, self.width - len).collect();
        Ok(if !self.right {
            format!("{v}{fill}")
        } else if self.pad == '0' && v.starts_with('-') {
            // Zero padding goes after the sign: -0042, not 00-42.
            format!("-{fill}{}", &v[1..])
        } else {
            format!("{fill}{v}")
        })
    }
}

struct FixedWidth;

impl Format for FixedWidth {
    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        if req.result_sets.len() != 1 {
            return Err(format!(
                "fixed_width writes exactly one result set per file, got {}",
                req.result_sets.len()
            )
            .into());
        }
        let cols = columns(req.options.get("columns"))?;
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
        let file = File::create(&req.path).map_err(|e| format!("can't create {}: {e}", req.path))?;
        let mut w = BufWriter::new(file);
        let opts = FormatOptions::default()
            .with_timestamp_format(Some("%Y-%m-%d %H:%M:%S"))
            .with_timestamp_tz_format(Some("%Y-%m-%d %H:%M:%S%:z"));
        let mut row = 0u64;
        while let Some(batch) = rs.next_batch()? {
            let fmts: Vec<ArrayFormatter<'_>> = idx
                .iter()
                .map(|&i| ArrayFormatter::try_new(batch.column(i).as_ref(), &opts))
                .collect::<std::result::Result<_, _>>()?;
            for r in 0..batch.num_rows() {
                row += 1;
                let mut line = String::new();
                for (k, c) in cols.iter().enumerate() {
                    let v = if batch.column(idx[k]).is_null(r) {
                        String::new()
                    } else {
                        fmts[k].value(r).to_string()
                    };
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
                    line.push_str(&c.cell(&v, row)?);
                }
                line.push_str(&line_ending);
                if enc == encoding_rs::UTF_8 {
                    w.write_all(line.as_bytes())?;
                } else {
                    let (bytes, _, bad) = enc.encode(&line);
                    if bad {
                        return Err(
                            format!("row {row} has characters that {} can't represent", enc.name()).into(),
                        );
                    }
                    w.write_all(&bytes)?;
                }
            }
        }
        w.flush()?;
        Ok(vec![req.path.clone()])
    }
}

fn main() {
    serve_format(About::new("fixed_width", env!("CARGO_PKG_VERSION")), FixedWidth)
}
