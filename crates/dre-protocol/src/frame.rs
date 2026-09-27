//! Frames: a 4-byte big-endian length, then that many bytes. The first body byte is the frame
//! type: `J` (0x4A) for a UTF-8 JSON control message, `A` (0x41) for an Arrow IPC stream holding
//! a schema and zero or more record batches.

use std::io::{self, Read, Write};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::StreamReader;
use arrow::ipc::writer::StreamWriter;

pub const JSON_TAG: u8 = b'J';
pub const ARROW_TAG: u8 = b'A';
/// Largest frame body accepted: 1 GiB. Larger result sets are split across batches.
pub const MAX_FRAME: u32 = 1 << 30;

pub enum Frame {
    Json(serde_json::Value),
    Arrow(Vec<u8>),
}

/// Never the raw bytes of an Arrow frame: they end up in error messages.
impl std::fmt::Debug for Frame {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Frame::Json(v) => write!(f, "Json({v})"),
            Frame::Arrow(b) => write!(f, "Arrow frame ({} bytes)", b.len()),
        }
    }
}

#[derive(Debug)]
pub enum FrameError {
    /// Clean end of stream before a frame started.
    Eof,
    Io(io::Error),
    Malformed(String),
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Eof => f.write_str("end of stream"),
            FrameError::Io(e) => write!(f, "I/O error: {e}"),
            FrameError::Malformed(m) => write!(f, "malformed frame: {m}"),
        }
    }
}

impl std::error::Error for FrameError {}

pub fn read_frame(r: &mut impl Read) -> Result<Frame, FrameError> {
    let mut len = [0u8; 4];
    match r.read(&mut len[..1]) {
        Ok(0) => return Err(FrameError::Eof),
        Ok(_) => {}
        Err(e) => return Err(FrameError::Io(e)),
    }
    r.read_exact(&mut len[1..])
        .map_err(|_| FrameError::Malformed("stream ended inside a frame header".into()))?;
    let len = u32::from_be_bytes(len);
    if len == 0 || len > MAX_FRAME {
        return Err(FrameError::Malformed(format!(
            "frame length {len} is out of range"
        )));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)
        .map_err(|_| FrameError::Malformed(format!("stream ended inside a {len}-byte frame")))?;
    match body[0] {
        JSON_TAG => serde_json::from_slice(&body[1..])
            .map(Frame::Json)
            .map_err(|e| FrameError::Malformed(format!("invalid JSON: {e}"))),
        ARROW_TAG => {
            body.remove(0);
            Ok(Frame::Arrow(body))
        }
        t => Err(FrameError::Malformed(format!(
            "unknown frame type byte 0x{t:02x}"
        ))),
    }
}

pub fn write_json(w: &mut impl Write, v: &impl serde::Serialize) -> io::Result<()> {
    let mut body = vec![JSON_TAG];
    serde_json::to_writer(&mut body, v)?;
    write_body(w, &body)
}

pub fn write_arrow(w: &mut impl Write, ipc: &[u8]) -> io::Result<()> {
    let mut body = Vec::with_capacity(ipc.len() + 1);
    body.push(ARROW_TAG);
    body.extend_from_slice(ipc);
    write_body(w, &body)
}

fn write_body(w: &mut impl Write, body: &[u8]) -> io::Result<()> {
    let len = u32::try_from(body.len())
        .ok()
        .filter(|l| *l <= MAX_FRAME)
        .ok_or_else(|| io::Error::other("frame too large"))?;
    w.write_all(&len.to_be_bytes())?;
    w.write_all(body)?;
    w.flush()
}

/// Encode one batch as a self-contained Arrow IPC stream (schema + batch).
pub fn encode_batch(batch: &RecordBatch) -> Result<Vec<u8>, arrow::error::ArrowError> {
    let mut buf = Vec::new();
    {
        let mut w = StreamWriter::try_new(&mut buf, &batch.schema())?;
        w.write(batch)?;
        w.finish()?;
    }
    Ok(buf)
}

/// Decode an Arrow frame into its schema and batches.
pub fn decode_batches(ipc: &[u8]) -> Result<(SchemaRef, Vec<RecordBatch>), arrow::error::ArrowError> {
    let reader = StreamReader::try_new(io::Cursor::new(ipc), None)?;
    let schema = reader.schema();
    let batches = reader.collect::<Result<Vec<_>, _>>()?;
    Ok((schema, batches))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::array::Int64Array;
    use std::sync::Arc;

    #[test]
    fn json_and_arrow_frames_round_trip() {
        let batch = RecordBatch::try_from_iter([("n", Arc::new(Int64Array::from(vec![1, 2])) as _)]).unwrap();
        let mut buf = Vec::new();
        write_json(&mut buf, &serde_json::json!({"type": "ok"})).unwrap();
        write_arrow(&mut buf, &encode_batch(&batch).unwrap()).unwrap();
        let mut r = io::Cursor::new(buf);
        assert!(matches!(read_frame(&mut r).unwrap(), Frame::Json(v) if v["type"] == "ok"));
        let Frame::Arrow(ipc) = read_frame(&mut r).unwrap() else {
            panic!()
        };
        let (_, batches) = decode_batches(&ipc).unwrap();
        assert_eq!(batches, vec![batch]);
        assert!(matches!(read_frame(&mut r), Err(FrameError::Eof)));
    }

    #[test]
    fn rejects_malformed_frames() {
        for bad in [
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 2, b'X', 1],
            vec![0, 0, 0, 9, b'J'],
            vec![0, 0],
        ] {
            assert!(matches!(
                read_frame(&mut io::Cursor::new(bad)),
                Err(FrameError::Malformed(_))
            ));
        }
    }
}
