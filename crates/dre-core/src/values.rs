//! Arrow values as Jinja values, for `run_query()` rows.

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{
    DataType, Float32Type, Float64Type, Int8Type, Int16Type, Int32Type, Int64Type, UInt8Type, UInt16Type,
    UInt32Type, UInt64Type,
};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use minijinja::Value;

/// Every row of `batch`, as a list of values per row.
pub fn batch_rows(batch: &RecordBatch) -> Vec<Vec<Value>> {
    let opts = FormatOptions::default();
    let cols: Vec<Box<dyn Fn(usize) -> Value + '_>> = batch
        .columns()
        .iter()
        .map(|c| column(c.as_ref(), &opts))
        .collect();
    (0..batch.num_rows())
        .map(|i| cols.iter().map(|f| f(i)).collect())
        .collect()
}

fn column<'a>(a: &'a dyn Array, opts: &'a FormatOptions<'a>) -> Box<dyn Fn(usize) -> Value + 'a> {
    macro_rules! prim {
        ($t:ty) => {{
            let arr = a.as_primitive::<$t>();
            Box::new(move |i| {
                if arr.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(arr.value(i))
                }
            })
        }};
    }
    match a.data_type() {
        DataType::Int8 => prim!(Int8Type),
        DataType::Int16 => prim!(Int16Type),
        DataType::Int32 => prim!(Int32Type),
        DataType::Int64 => prim!(Int64Type),
        DataType::UInt8 => prim!(UInt8Type),
        DataType::UInt16 => prim!(UInt16Type),
        DataType::UInt32 => prim!(UInt32Type),
        DataType::UInt64 => prim!(UInt64Type),
        DataType::Float32 => prim!(Float32Type),
        DataType::Float64 => prim!(Float64Type),
        DataType::Boolean => {
            let arr = a.as_boolean();
            Box::new(move |i| {
                if arr.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(arr.value(i))
                }
            })
        }
        _ => match ArrayFormatter::try_new(a, opts) {
            Ok(f) => Box::new(move |i| {
                if a.is_null(i) {
                    Value::from(())
                } else {
                    Value::from(f.value(i).to_string())
                }
            }),
            Err(_) => Box::new(|_| Value::from(())),
        },
    }
}
