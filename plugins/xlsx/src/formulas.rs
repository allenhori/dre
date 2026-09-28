//! Row formulas and totals rows: `formula` and `total` in a `columns:` map.
//!
//! A row formula replaces a placeholder column's value with a formula over cells on the same row
//! (`={qty}*{price}` → `=B2*C2`); the placeholder's value becomes the formula's cached result. A
//! `total` adds a totals row under the data (plain sheets only): `sum`, `average`, `count`,
//! `min`, `max`, or a formula over whole columns (`=SUM({amount:*})`), with the aggregate worked
//! out in DRE as the cached result so readers that don't recalculate still see it.
//!
//! As with `format`, a query entry's `formula` / `total` wins over the output-level one.

use std::collections::BTreeMap;

use arrow::datatypes::Schema;
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use dre_protocol::options::{FormulaPart, TOTAL_FUNCTIONS, parse_formula};
use dre_protocol::plugin::Result;

use crate::cells::Excel;
use crate::formats::Kind;

/// A totals row function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Agg {
    Sum,
    Average,
    Count,
    Min,
    Max,
}

impl Agg {
    fn of(name: &str) -> Option<Agg> {
        Some(match name {
            "sum" => Agg::Sum,
            "average" => Agg::Average,
            "count" => Agg::Count,
            "min" => Agg::Min,
            "max" => Agg::Max,
            _ => return None,
        })
    }

    fn excel(self, name: &str) -> &'static str {
        TOTAL_FUNCTIONS
            .iter()
            .find(|(k, _)| *k == name)
            .map(|(_, f)| *f)
            .unwrap_or("SUM")
    }

    fn fits(self, kind: Kind) -> bool {
        match self {
            Agg::Count => true,
            Agg::Sum | Agg::Average => kind == Kind::Number,
            Agg::Min | Agg::Max => kind != Kind::Other,
        }
    }
}

/// One column's totals row entry.
#[derive(Clone, Debug)]
pub enum Total {
    Function { agg: Agg, excel: &'static str },
    Formula(Vec<FormulaPart>),
}

/// The formulas of one result set, per column.
pub struct SetFormulas {
    pub row: Vec<Option<Vec<FormulaPart>>>,
    pub totals: Vec<Option<Total>>,
}

impl SetFormulas {
    /// Merge the query entry's settings over the output-level ones, and check every reference
    /// names a column of the set and every total fits its column's type (`schema` normalized).
    pub fn resolve(
        meta: &ResultSetMeta,
        output: &BTreeMap<String, ColumnOptions>,
        schema: &Schema,
    ) -> Result<SetFormulas> {
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let pick = |name: &str, get: fn(&ColumnOptions) -> Option<&String>| {
            meta.columns
                .get(name)
                .and_then(get)
                .or_else(|| output.get(name).and_then(get))
                .cloned()
        };
        let check_refs = |col: &str, what: &str, f: &str, parts: &[FormulaPart]| -> Result<()> {
            for p in parts {
                if let FormulaPart::Cell(n) | FormulaPart::Column(n) = p
                    && !names.contains(&n.as_str())
                {
                    return Err(format!(
                        "sheet `{}`: column `{col}`: {what} `{f}` refers to `{n}`, which query `{}` doesn't return (it has: {})",
                        meta.name,
                        meta.query,
                        names.join(", ")
                    )
                    .into());
                }
            }
            Ok(())
        };
        let mut row = Vec::with_capacity(names.len());
        let mut totals = Vec::with_capacity(names.len());
        for f in schema.fields() {
            let name = f.name();
            row.push(match pick(name, |c| c.formula.as_ref()) {
                Some(formula) => {
                    let parts = parse_formula(&formula)
                        .map_err(|e| format!("column `{name}`: formula `{formula}` {e}"))?;
                    check_refs(name, "formula", &formula, &parts)?;
                    Some(parts)
                }
                None => None,
            });
            totals.push(match pick(name, |c| c.total.as_ref()) {
                Some(t) if t.starts_with('=') => {
                    let parts = parse_formula(&t).map_err(|e| format!("column `{name}`: total `{t}` {e}"))?;
                    check_refs(name, "total", &t, &parts)?;
                    Some(Total::Formula(parts))
                }
                Some(t) => {
                    let agg = Agg::of(&t).ok_or_else(|| format!("column `{name}`: unknown total `{t}`"))?;
                    let kind = Kind::of(f.data_type());
                    if !agg.fits(kind) {
                        return Err(format!(
                            "sheet `{}`: column `{name}` is {}, which `total: {t}` can't total",
                            meta.name,
                            kind.describe()
                        )
                        .into());
                    }
                    Some(Total::Function {
                        agg,
                        excel: agg.excel(&t),
                    })
                }
                None => None,
            });
        }
        Ok(SetFormulas { row, totals })
    }

    pub fn has_totals(&self) -> bool {
        self.totals.iter().any(Option::is_some)
    }

    /// The first column with a total, by name, for messages.
    pub fn first_total(&self, names: &[String]) -> Option<String> {
        self.totals
            .iter()
            .position(Option::is_some)
            .map(|i| names[i].clone())
    }
}

/// Excel's column letters for a 0-based column.
pub fn col_letters(col: u32) -> String {
    let mut n = col + 1;
    let mut out = Vec::new();
    while n > 0 {
        let r = (n - 1) % 26;
        out.push(b'A' + r as u8);
        n = (n - 1) / 26;
    }
    out.reverse();
    String::from_utf8(out).unwrap()
}

/// A formula with its references turned into A1 cells: `{name}` on 0-based `row`, `{name:*}`
/// over the 0-based data rows `rows`. `col_of` places a column; `Err(name)` when it can't.
pub fn render(
    parts: &[FormulaPart],
    col_of: impl Fn(&str) -> Option<u32>,
    row: u32,
    rows: (u32, u32),
) -> std::result::Result<String, String> {
    let mut out = String::new();
    for p in parts {
        match p {
            FormulaPart::Text(t) => out.push_str(t),
            FormulaPart::Cell(n) => {
                let c = col_of(n).ok_or_else(|| n.clone())?;
                out.push_str(&format!("{}{}", col_letters(c), row + 1));
            }
            FormulaPart::Column(n) => {
                let c = col_letters(col_of(n).ok_or_else(|| n.clone())?);
                out.push_str(&format!("{c}{}:{c}{}", rows.0 + 1, rows.1 + 1));
            }
        }
    }
    Ok(out)
}

/// A formula's cached result from the value DRE would have written.
pub fn cached(v: &Option<Excel>) -> String {
    match v {
        None => String::new(),
        Some(Excel::Number(n) | Excel::Date(n) | Excel::DateTime(n) | Excel::Time(n)) => n.to_string(),
        Some(Excel::Bool(b)) => (if *b { "TRUE" } else { "FALSE" }).into(),
        Some(Excel::Text(t)) => t.clone(),
    }
}

/// Running aggregates of one column on one sheet, as Excel would compute them over the cells
/// written: text (including numbers written as text) counts for `count` only.
#[derive(Clone, Default)]
pub struct Acc {
    count: u64,
    nums: u64,
    sum: f64,
    min: f64,
    max: f64,
}

impl Acc {
    pub fn add(&mut self, v: &Option<Excel>) {
        let Some(v) = v else { return };
        self.count += 1;
        if let Excel::Number(n) | Excel::Date(n) | Excel::DateTime(n) | Excel::Time(n) = v {
            if self.nums == 0 {
                self.min = *n;
                self.max = *n;
            } else {
                self.min = self.min.min(*n);
                self.max = self.max.max(*n);
            }
            self.nums += 1;
            self.sum += n;
        }
    }

    /// The cached result of `agg` over what was added.
    pub fn result(&self, agg: Agg) -> String {
        match agg {
            Agg::Count => self.count.to_string(),
            Agg::Sum => self.sum.to_string(),
            Agg::Average if self.nums == 0 => "#DIV/0!".into(),
            Agg::Average => (self.sum / self.nums as f64).to_string(),
            Agg::Min => self.min.to_string(),
            Agg::Max => self.max.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dre_protocol::options::parse_formula;

    #[test]
    fn letters() {
        assert_eq!(col_letters(0), "A");
        assert_eq!(col_letters(25), "Z");
        assert_eq!(col_letters(26), "AA");
        assert_eq!(col_letters(16_383), "XFD");
    }

    #[test]
    fn rendering() {
        let col = |n: &str| match n {
            "qty" => Some(1),
            "price" => Some(2),
            _ => None,
        };
        let f = parse_formula("={qty}*{price}*$H$1").unwrap();
        assert_eq!(render(&f, col, 4, (1, 9)).unwrap(), "=B5*C5*$H$1");
        let t = parse_formula("=SUM({qty:*})").unwrap();
        assert_eq!(render(&t, col, 10, (1, 9)).unwrap(), "=SUM(B2:B10)");
        let bad = parse_formula("={nope}").unwrap();
        assert_eq!(render(&bad, col, 0, (0, 0)), Err("nope".into()));
    }
}
