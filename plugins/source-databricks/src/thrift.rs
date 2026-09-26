//! A minimal Thrift binary-protocol codec with a dynamic value model: enough to speak the
//! HiveServer2 service (TCLIService) over HTTP. Written from the public Thrift binary protocol
//! specification: big-endian integers, `(type, id)` field headers, a stop byte.

use std::collections::BTreeMap;

pub const T_STOP: u8 = 0;
pub const T_BOOL: u8 = 2;
pub const T_BYTE: u8 = 3;
pub const T_DOUBLE: u8 = 4;
pub const T_I16: u8 = 6;
pub const T_I32: u8 = 8;
pub const T_I64: u8 = 10;
pub const T_STRING: u8 = 11;
pub const T_STRUCT: u8 = 12;
pub const T_MAP: u8 = 13;
pub const T_SET: u8 = 14;
pub const T_LIST: u8 = 15;

const VERSION_1: u32 = 0x8001_0000;
pub const CALL: u8 = 1;
pub const REPLY: u8 = 2;
pub const EXCEPTION: u8 = 3;

#[derive(Debug, Clone, PartialEq)]
pub enum V {
    Bool(bool),
    Byte(i8),
    Double(f64),
    I16(i16),
    I32(i32),
    I64(i64),
    /// Thrift `string` and `binary` share a wire type.
    Bin(Vec<u8>),
    Struct(Struct),
    Map(u8, u8, Vec<(V, V)>),
    List(u8, Vec<V>),
}

/// A struct: field id → value.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Struct(pub BTreeMap<i16, V>);

impl Struct {
    pub fn new() -> Struct {
        Struct::default()
    }
    pub fn with(mut self, id: i16, v: V) -> Struct {
        self.0.insert(id, v);
        self
    }
    pub fn get(&self, id: i16) -> Option<&V> {
        self.0.get(&id)
    }
    pub fn st(&self, id: i16) -> Option<&Struct> {
        match self.get(id)? {
            V::Struct(s) => Some(s),
            _ => None,
        }
    }
    pub fn i32(&self, id: i16) -> Option<i32> {
        match self.get(id)? {
            V::I32(v) => Some(*v),
            _ => None,
        }
    }
    pub fn i64(&self, id: i16) -> Option<i64> {
        match self.get(id)? {
            V::I64(v) => Some(*v),
            _ => None,
        }
    }
    pub fn bool(&self, id: i16) -> Option<bool> {
        match self.get(id)? {
            V::Bool(v) => Some(*v),
            _ => None,
        }
    }
    pub fn double(&self, id: i16) -> Option<f64> {
        match self.get(id)? {
            V::Double(v) => Some(*v),
            _ => None,
        }
    }
    pub fn str(&self, id: i16) -> Option<String> {
        match self.get(id)? {
            V::Bin(b) => Some(String::from_utf8_lossy(b).to_string()),
            _ => None,
        }
    }
    pub fn bin(&self, id: i16) -> Option<&[u8]> {
        match self.get(id)? {
            V::Bin(b) => Some(b),
            _ => None,
        }
    }
    pub fn list(&self, id: i16) -> Option<&[V]> {
        match self.get(id)? {
            V::List(_, l) => Some(l),
            _ => None,
        }
    }
}

pub fn s(v: &str) -> V {
    V::Bin(v.as_bytes().to_vec())
}

fn type_of(v: &V) -> u8 {
    match v {
        V::Bool(_) => T_BOOL,
        V::Byte(_) => T_BYTE,
        V::Double(_) => T_DOUBLE,
        V::I16(_) => T_I16,
        V::I32(_) => T_I32,
        V::I64(_) => T_I64,
        V::Bin(_) => T_STRING,
        V::Struct(_) => T_STRUCT,
        V::Map(..) => T_MAP,
        V::List(..) => T_LIST,
    }
}

fn write_value(out: &mut Vec<u8>, v: &V) {
    match v {
        V::Bool(b) => out.push(u8::from(*b)),
        V::Byte(b) => out.push(*b as u8),
        V::Double(d) => out.extend_from_slice(&d.to_bits().to_be_bytes()),
        V::I16(i) => out.extend_from_slice(&i.to_be_bytes()),
        V::I32(i) => out.extend_from_slice(&i.to_be_bytes()),
        V::I64(i) => out.extend_from_slice(&i.to_be_bytes()),
        V::Bin(b) => {
            out.extend_from_slice(&(b.len() as i32).to_be_bytes());
            out.extend_from_slice(b);
        }
        V::Struct(s) => write_struct(out, s),
        V::Map(kt, vt, entries) => {
            out.push(*kt);
            out.push(*vt);
            out.extend_from_slice(&(entries.len() as i32).to_be_bytes());
            for (k, v) in entries {
                write_value(out, k);
                write_value(out, v);
            }
        }
        V::List(et, items) => {
            out.push(*et);
            out.extend_from_slice(&(items.len() as i32).to_be_bytes());
            for i in items {
                write_value(out, i);
            }
        }
    }
}

pub fn write_struct(out: &mut Vec<u8>, s: &Struct) {
    for (id, v) in &s.0 {
        out.push(type_of(v));
        out.extend_from_slice(&id.to_be_bytes());
        write_value(out, v);
    }
    out.push(T_STOP);
}

/// A complete message: header, then the arguments struct.
pub fn message(name: &str, kind: u8, seq: i32, body: &Struct) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&(VERSION_1 | u32::from(kind)).to_be_bytes());
    write_value(&mut out, &s(name));
    out.extend_from_slice(&seq.to_be_bytes());
    write_struct(&mut out, body);
    out
}

pub struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

type R<T> = Result<T, String>;

impl<'a> Reader<'a> {
    pub fn new(b: &'a [u8]) -> Reader<'a> {
        Reader { b, pos: 0 }
    }

    fn take(&mut self, n: usize) -> R<&'a [u8]> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|e| *e <= self.b.len())
            .ok_or("truncated Thrift message")?;
        let s = &self.b[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> R<u8> {
        Ok(self.take(1)?[0])
    }
    fn i16(&mut self) -> R<i16> {
        Ok(i16::from_be_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn i32(&mut self) -> R<i32> {
        Ok(i32::from_be_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> R<i64> {
        Ok(i64::from_be_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn len(&mut self) -> R<usize> {
        let n = self.i32()?;
        usize::try_from(n).map_err(|_| format!("negative length {n}"))
    }

    fn value(&mut self, t: u8) -> R<V> {
        Ok(match t {
            T_BOOL => V::Bool(self.u8()? != 0),
            T_BYTE => V::Byte(self.u8()? as i8),
            T_DOUBLE => V::Double(f64::from_bits(u64::from_be_bytes(
                self.take(8)?.try_into().unwrap(),
            ))),
            T_I16 => V::I16(self.i16()?),
            T_I32 => V::I32(self.i32()?),
            T_I64 => V::I64(self.i64()?),
            T_STRING => {
                let n = self.len()?;
                V::Bin(self.take(n)?.to_vec())
            }
            T_STRUCT => V::Struct(self.read_struct()?),
            T_MAP => {
                let (kt, vt) = (self.u8()?, self.u8()?);
                let n = self.len()?;
                let mut e = Vec::with_capacity(n.min(1 << 16));
                for _ in 0..n {
                    let k = self.value(kt)?;
                    let v = self.value(vt)?;
                    e.push((k, v));
                }
                V::Map(kt, vt, e)
            }
            T_LIST | T_SET => {
                let et = self.u8()?;
                let n = self.len()?;
                let mut items = Vec::with_capacity(n.min(1 << 20));
                for _ in 0..n {
                    items.push(self.value(et)?);
                }
                V::List(et, items)
            }
            t => return Err(format!("unknown Thrift type {t}")),
        })
    }

    pub fn read_struct(&mut self) -> R<Struct> {
        let mut s = Struct::new();
        loop {
            let t = self.u8()?;
            if t == T_STOP {
                return Ok(s);
            }
            let id = self.i16()?;
            let v = self.value(t)?;
            s.0.insert(id, v);
        }
    }

    /// Read a message header and body: `(name, kind, seq, body)`.
    pub fn read_message(&mut self) -> R<(String, u8, i32, Struct)> {
        let v = self.i32()? as u32;
        if v & 0xffff_0000 != VERSION_1 {
            return Err("not a strict Thrift binary message".into());
        }
        let kind = (v & 0xff) as u8;
        let name = match self.value(T_STRING)? {
            V::Bin(b) => String::from_utf8_lossy(&b).to_string(),
            _ => unreachable!(),
        };
        let seq = self.i32()?;
        let body = self.read_struct()?;
        Ok((name, kind, seq, body))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_round_trip() {
        let body = Struct::new()
            .with(
                1,
                V::Struct(Struct::new().with(1, V::I32(7)).with(2, s("select 1"))),
            )
            .with(2, V::List(T_I64, vec![V::I64(-1), V::I64(2)]))
            .with(3, V::Map(T_STRING, T_STRING, vec![(s("k"), s("v"))]))
            .with(4, V::Bool(true))
            .with(5, V::Double(1.5));
        let bytes = message("ExecuteStatement", CALL, 3, &body);
        let (name, kind, seq, back) = Reader::new(&bytes).read_message().unwrap();
        assert_eq!((name.as_str(), kind, seq), ("ExecuteStatement", CALL, 3));
        assert_eq!(back, body);
    }

    #[test]
    fn truncated_input_is_an_error_not_a_panic() {
        let bytes = message("X", CALL, 1, &Struct::new().with(1, s("hello")));
        for n in 0..bytes.len() {
            assert!(Reader::new(&bytes[..n]).read_message().is_err());
        }
    }
}
