//! DRE format plugin `parquet`: one result set per file, Arrow types preserved as-is.

use std::fs::File;

use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use parquet::arrow::ArrowWriter;
use parquet::basic::Compression;
use parquet::file::properties::WriterProperties;

struct Parquet;

impl Format for Parquet {
    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        if req.result_sets.len() != 1 {
            return Err(format!(
                "parquet writes exactly one result set per file, got {}",
                req.result_sets.len()
            )
            .into());
        }
        let mut rs = sets.next_set()?.expect("one result set");
        let file = File::create(&req.path).map_err(|e| format!("can't create {}: {e}", req.path))?;
        let props = WriterProperties::builder()
            .set_compression(Compression::SNAPPY)
            .build();
        let mut w = ArrowWriter::try_new(file, rs.schema.clone(), Some(props))?;
        while let Some(b) = rs.next_batch()? {
            w.write(&b)?;
        }
        w.close()?;
        Ok(vec![req.path.clone()])
    }
}

fn main() {
    serve_format(About::new("parquet", env!("CARGO_PKG_VERSION")), Parquet)
}
