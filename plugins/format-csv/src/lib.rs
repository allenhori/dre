//! DRE format plugins `csv` and `delimited`: one table per file.
//!
//! Options (defaults): `delimiter` (`,`), `quote` (`"`), `quoting` (`minimal`|`all`|`strings`|`none`),
//! `header` (true), `line_ending` (`"\r\n"`), `encoding` (`utf-8`), `null` (`""`),
//! `byte_order_mark` (false).

use std::fs::File;
use std::io::{BufWriter, Write};

use arrow::array::{Array, RecordBatch};
use arrow::datatypes::DataType;
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use serde_json::{Map, Value};

#[derive(Clone, Copy, PartialEq)]
enum Quoting {
    Minimal,
    All,
    /// Quote every value of a column that isn't a number or boolean; nulls stay bare.
    Strings,
    None,
}

struct Options {
    delimiter: String,
    quote: String,
    quoting: Quoting,
    header: bool,
    line_ending: String,
    encoding: &'static encoding_rs::Encoding,
    null: String,
    bom: bool,
}

impl Options {
    fn parse(o: &Map<String, Value>) -> Result<Options> {
        let s = |k: &str, d: &str| o.get(k).and_then(Value::as_str).unwrap_or(d).to_string();
        let b = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
        let quoting = match s("quoting", "minimal").as_str() {
            "minimal" => Quoting::Minimal,
            "all" => Quoting::All,
            "strings" => Quoting::Strings,
            "none" => Quoting::None,
            q => return Err(format!("unknown `quoting` `{q}`").into()),
        };
        let label = s("encoding", "utf-8");
        let encoding = encoding_rs::Encoding::for_label(label.as_bytes())
            .filter(|e| e.output_encoding() == *e)
            .ok_or_else(|| format!("unsupported `encoding` `{label}`"))?;
        Ok(Options {
            delimiter: s("delimiter", ","),
            quote: s("quote", "\""),
            quoting,
            header: b("header", true),
            line_ending: s("line_ending", "\r\n"),
            encoding,
            null: s("null", ""),
            bom: b("byte_order_mark", false),
        })
    }

    /// `text` is whether the value comes from a column `quoting: strings` quotes.
    fn field(&self, v: &str, text: bool, row: u64, col: &str) -> Result<String> {
        let needs =
            v.contains(&self.delimiter) || v.contains(&self.quote) || v.contains('\n') || v.contains('\r');
        let quote = match self.quoting {
            Quoting::All => true,
            Quoting::Minimal => needs,
            Quoting::Strings => needs || text,
            Quoting::None if needs => {
                return Err(format!(
                    "row {row}, column `{col}`: the value contains the delimiter, quote or a line break, which `quoting: none` can't represent"
                )
                .into());
            }
            Quoting::None => false,
        };
        Ok(if quote {
            let q = &self.quote;
            format!("{q}{}{q}", v.replace(q.as_str(), &format!("{q}{q}")))
        } else {
            v.to_string()
        })
    }
}

struct Delimited;

impl Format for Delimited {
    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        if req.result_sets.len() != 1 {
            return Err(format!(
                "`{}` writes exactly one result set per file, got {}",
                req.format,
                req.result_sets.len()
            )
            .into());
        }
        let o = Options::parse(&req.options)?;
        let mut rs = sets.next_set()?.expect("one result set");
        let file = File::create(&req.path).map_err(|e| format!("can't create {}: {e}", req.path))?;
        let mut w = BufWriter::new(file);
        let mut out = Encoder {
            o: &o,
            w: &mut w,
            line: 0,
        };
        if o.bom && o.encoding == encoding_rs::UTF_8 {
            out.w.write_all(b"\xEF\xBB\xBF")?;
        }
        let names: Vec<String> = rs.schema.fields().iter().map(|f| f.name().clone()).collect();
        if o.header {
            let cells: Vec<String> = names
                .iter()
                .map(|n| o.field(n, true, 0, n))
                .collect::<Result<_>>()?;
            out.line(&cells.join(&o.delimiter))?;
        }
        let fmt = FormatOptions::default()
            .with_null(&o.null)
            .with_timestamp_format(Some("%Y-%m-%d %H:%M:%S%.f"))
            .with_timestamp_tz_format(Some("%Y-%m-%d %H:%M:%S%.f%:z"));
        let mut row = 0u64;
        while let Some(batch) = rs.next_batch()? {
            write_batch(&batch, &names, &o, &fmt, &mut out, &mut row)?;
        }
        w.flush()?;
        Ok(vec![req.path.clone()])
    }
}

fn write_batch(
    batch: &RecordBatch,
    names: &[String],
    o: &Options,
    fmt: &FormatOptions<'_>,
    out: &mut Encoder<'_>,
    row: &mut u64,
) -> Result<()> {
    let formatters: Vec<ArrayFormatter<'_>> = batch
        .columns()
        .iter()
        .map(|c| ArrayFormatter::try_new(c.as_ref(), fmt))
        .collect::<std::result::Result<_, _>>()?;
    let text: Vec<bool> = batch.columns().iter().map(|c| is_text(c.data_type())).collect();
    for i in 0..batch.num_rows() {
        *row += 1;
        let mut cells = Vec::with_capacity(names.len());
        for (c, f) in formatters.iter().enumerate() {
            let col = batch.column(c);
            if col.is_null(i) {
                cells.push(o.null.clone());
                continue;
            }
            let v = f.value(i).to_string();
            let v = if matches!(col.data_type(), DataType::Boolean) {
                v.to_lowercase()
            } else {
                v
            };
            cells.push(o.field(&v, text[c], *row, &names[c])?);
        }
        out.line(&cells.join(&o.delimiter))?;
    }
    Ok(())
}

/// Everything but numbers and booleans: strings, dates, times, binary, ...
fn is_text(t: &DataType) -> bool {
    !(t.is_numeric() || matches!(t, DataType::Boolean | DataType::Null))
}

struct Encoder<'a> {
    o: &'a Options,
    w: &'a mut BufWriter<File>,
    line: u64,
}

impl Encoder<'_> {
    fn line(&mut self, text: &str) -> Result<()> {
        self.line += 1;
        let text = format!("{text}{}", self.o.line_ending);
        if self.o.encoding == encoding_rs::UTF_8 {
            self.w.write_all(text.as_bytes())?;
            return Ok(());
        }
        let (bytes, _, unmappable) = self.o.encoding.encode(&text);
        if unmappable {
            return Err(format!(
                "line {} has characters that {} can't represent",
                self.line,
                self.o.encoding.name()
            )
            .into());
        }
        self.w.write_all(&bytes)?;
        Ok(())
    }
}

/// Serve the protocol as the format called `name` (`csv` or `delimited`).
pub fn serve(name: &'static str) -> ! {
    serve_format(About::new(name, env!("CARGO_PKG_VERSION")), Delimited)
}
