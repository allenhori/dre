use std::path::Path;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Date32Array, Decimal128Array, Int64Array, RecordBatch, StringArray, TimestampMicrosecondArray,
};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-parquet"))
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn types_and_values_survive_a_round_trip() {
    let b1 = RecordBatch::try_from_iter([
        ("id", Arc::new(Int64Array::from(vec![Some(1), None])) as ArrayRef),
        (
            "name",
            Arc::new(StringArray::from(vec![Some("Acme Corp"), Some("Client A")])) as ArrayRef,
        ),
        (
            "amount",
            Arc::new(
                Decimal128Array::from(vec![10050, -325])
                    .with_precision_and_scale(12, 2)
                    .unwrap(),
            ) as ArrayRef,
        ),
        ("day", Arc::new(Date32Array::from(vec![20478, 0])) as ArrayRef),
        (
            "at",
            Arc::new(TimestampMicrosecondArray::from(vec![1, 2]).with_timezone("UTC")) as ArrayRef,
        ),
    ])
    .unwrap();
    let b2 = b1.slice(1, 1);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.parquet");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let meta = ResultSetMeta {
        name: "x".into(),
        query: "x".into(),
        result_index: 1,
        anchor: None,
        header: None,
        columns: Default::default(),
    };
    p.write_begin(
        path.to_str().unwrap(),
        "parquet",
        Default::default(),
        vec![meta],
        None,
    )
    .unwrap();
    p.write_result_set(&b1.schema(), vec![b1.clone(), b2.clone()])
        .unwrap();
    p.write_finish().unwrap();

    let reader = ParquetRecordBatchReaderBuilder::try_new(std::fs::File::open(&path).unwrap())
        .unwrap()
        .build()
        .unwrap();
    let back: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(back[0].schema(), b1.schema());
    let all = arrow::compute::concat_batches(&b1.schema(), &back).unwrap();
    let expected = arrow::compute::concat_batches(&b1.schema(), &[b1, b2]).unwrap();
    assert_eq!(all, expected);
}
