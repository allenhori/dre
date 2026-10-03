//! How `dre validate --live` compares a declared source column's `data_type` with what the
//! database returns (an Arrow type, as the source plugin delivers it). The comparison is loose:
//! a declared SQL type matches a family of Arrow types (`bigint` ≈ `Int64`, `varchar(20)` ≈
//! `Utf8`), since plugins map database types to Arrow in their own ways. Documented in
//! `docs/sources.md`.

/// The families a declared type can belong to, each with the Arrow type names it accepts
/// (matched by prefix, so `Timestamp(Microsecond, None)` is a `Timestamp`).
const FAMILIES: &[(&[&str], &[&str])] = &[
    (
        &[
            "tinyint",
            "smallint",
            "int",
            "integer",
            "bigint",
            "hugeint",
            "int2",
            "int4",
            "int8",
            "int16",
            "int32",
            "int64",
            "byte",
            "short",
            "long",
            "utinyint",
            "usmallint",
            "uinteger",
            "ubigint",
            "serial",
            "bigserial",
            "smallserial",
        ],
        &[
            "Int8", "Int16", "Int32", "Int64", "UInt8", "UInt16", "UInt32", "UInt64", "Decimal",
        ],
    ),
    (
        &["float", "float4", "float8", "real", "double", "double precision"],
        &["Float16", "Float32", "Float64", "Decimal"],
    ),
    (
        &["decimal", "numeric", "number", "dec", "money"],
        &[
            "Decimal", "Float32", "Float64", "Int8", "Int16", "Int32", "Int64", "Utf8",
        ],
    ),
    (
        &[
            "varchar",
            "char",
            "character",
            "character varying",
            "text",
            "string",
            "nvarchar",
            "nchar",
            "bpchar",
            "uuid",
            "citext",
            "name",
            "enum",
        ],
        &["Utf8", "LargeUtf8", "Utf8View"],
    ),
    (&["boolean", "bool", "bit"], &["Boolean"]),
    (&["date"], &["Date32", "Date64"]),
    (
        &[
            "timestamp",
            "timestamptz",
            "timestamp_ntz",
            "timestamp_ltz",
            "timestamp_tz",
            "datetime",
            "timestamp with time zone",
            "timestamp without time zone",
        ],
        &["Timestamp", "Date64"],
    ),
    (
        &["time", "timetz", "time with time zone", "time without time zone"],
        &["Time32", "Time64"],
    ),
    (
        &["binary", "varbinary", "bytea", "blob", "bytes"],
        &["Binary", "LargeBinary", "BinaryView", "FixedSizeBinary"],
    ),
    (&["interval"], &["Interval", "Duration"]),
    (
        &["json", "jsonb", "variant", "object"],
        &["Utf8", "LargeUtf8", "Utf8View"],
    ),
    (&["array", "list"], &["List", "LargeList", "FixedSizeList"]),
    (&["struct", "record", "row"], &["Struct"]),
    (&["map"], &["Map"]),
];

/// Whether `declared` (a SQL type as written in YAML) matches `arrow` (an Arrow type name);
/// `None` when the declared type isn't one DRE knows.
pub fn matches(declared: &str, arrow: &str) -> Option<bool> {
    let base = base_type(declared);
    let (_, accepts) = FAMILIES
        .iter()
        .find(|(names, _)| names.contains(&base.as_str()))?;
    Some(accepts.iter().any(|a| arrow.starts_with(a)))
}

/// `VARCHAR(20)` → `varchar`; `ARRAY<INT>` → `array`; `int[]` → `array`; `Decimal(10, 2)` →
/// `decimal`.
fn base_type(declared: &str) -> String {
    let t = declared.trim().to_lowercase();
    if t.ends_with("[]") {
        return "array".into();
    }
    let cut = t.find(['(', '<']).unwrap_or(t.len());
    t[..cut].trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn declared_types_match_their_arrow_families() {
        assert_eq!(matches("bigint", "Int64"), Some(true));
        assert_eq!(matches("INTEGER", "Int32"), Some(true));
        assert_eq!(matches("varchar(20)", "Utf8"), Some(true));
        assert_eq!(matches("numeric(10,2)", "Decimal128(10, 2)"), Some(true));
        assert_eq!(
            matches(
                "timestamp with time zone",
                "Timestamp(Microsecond, Some(\"UTC\"))"
            ),
            Some(true)
        );
        assert_eq!(matches("int[]", "List(Int32)"), Some(true));
        assert_eq!(matches("date", "Date32"), Some(true));
    }

    #[test]
    fn mismatches_and_unknown_types() {
        assert_eq!(matches("bigint", "Utf8"), Some(false));
        assert_eq!(matches("date", "Int32"), Some(false));
        assert_eq!(matches("geography", "Binary"), None);
    }
}
