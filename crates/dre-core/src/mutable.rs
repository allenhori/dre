//! Python-style mutable collections for templates: `list()` and `dict()`.
//!
//! MiniJinja's own values are immutable, so `{% set items = [] %}` followed by `items.append(x)`
//! can't work. Instead `list()` and `dict()` hand out these objects, which behave like Python's
//! `list` and `dict`: they are shared by reference (`{% set b = a %}` is the same list), iterate
//! in insertion order, and print like their immutable counterparts. They live for one render:
//! nothing is kept between templates, Bindings or Sets.
//!
//! A mutating method on an immutable `[]` or `{}` literal (or a `var()` value) is refused with a
//! hint to use `list()` / `dict()`; see [`unknown_method`].

use std::sync::{Arc, Mutex, MutexGuard};

use minijinja::value::{Enumerator, Kwargs, Object, ObjectRepr, Value, ValueKind, from_args};
use minijinja::{Error, ErrorKind, State};

/// Methods that change a list or dict in place, which an immutable value can't offer.
const MUTATING: &[&str] = &[
    "append",
    "extend",
    "insert",
    "pop",
    "remove",
    "clear",
    "sort",
    "reverse",
    "update",
    "setdefault",
];

pub fn register(env: &mut minijinja::Environment<'_>) {
    env.add_function("list", |items: Option<Value>| -> Result<Value, Error> {
        let items = match items {
            Some(v) => collect(&v)?,
            None => Vec::new(),
        };
        Ok(Value::from_object(MutableList(Mutex::new(items))))
    });
    env.add_function(
        "dict",
        |source: Option<Value>, kwargs: Kwargs| -> Result<Value, Error> {
            let mut pairs = Vec::new();
            if let Some(src) = source {
                merge(&mut pairs, &src)?;
            }
            merge_kwargs(&mut pairs, &kwargs)?;
            Ok(Value::from_object(MutableDict(Mutex::new(pairs))))
        },
    );
    env.set_unknown_method_callback(unknown_method);
}

/// Python-style methods on the built-in values (`pycompat`), except that the mutating ones get a
/// message that says what to write instead.
fn unknown_method(
    state: &State<'_, '_>,
    value: &Value,
    method: &str,
    args: &[Value],
) -> Result<Value, Error> {
    if MUTATING.contains(&method) && matches!(value.kind(), ValueKind::Seq | ValueKind::Map) {
        let (what, make) = if value.kind() == ValueKind::Seq {
            ("a list", "list()")
        } else {
            ("a mapping", "dict()")
        };
        return Err(Error::new(
            ErrorKind::InvalidOperation,
            format!(
                "`{method}()` needs a list or dict made with `list()` / `dict()`, but this is {what} that \
                 can't change (a `[]` or `{{}}` literal, or a `var()` value). \
                 Write `{{% set items = {make} %}}`, or `{make}` with the values to start from"
            ),
        ));
    }
    minijinja_contrib::pycompat::unknown_method_callback(state, value, method, args)
}

fn collect(v: &Value) -> Result<Vec<Value>, Error> {
    v.try_iter().map(Iterator::collect)
}

fn bad(msg: impl Into<String>) -> Error {
    Error::new(ErrorKind::InvalidOperation, msg.into())
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A poisoned lock only means an earlier render panicked; the data is still a plain Vec.
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Python index rules: negative counts from the end.
fn index(i: i64, len: usize) -> Option<usize> {
    let i = if i < 0 { i + len as i64 } else { i };
    (0..len as i64).contains(&i).then_some(i as usize)
}

#[derive(Debug)]
struct MutableList(Mutex<Vec<Value>>);

impl Object for MutableList {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Seq
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        let items = lock(&self.0);
        items
            .get(index(i64::try_from(key.clone()).ok()?, items.len())?)
            .cloned()
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        // A snapshot, so changing the list inside a loop over it can't skip or repeat items.
        Enumerator::Iter(Box::new(lock(&self.0).clone().into_iter()))
    }

    fn enumerator_len(self: &Arc<Self>) -> Option<usize> {
        Some(lock(&self.0).len())
    }

    fn call_method(
        self: &Arc<Self>,
        _: &State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        if method == "extend" {
            let (v,): (Value,) = from_args(args).map_err(|_| bad("`extend()` takes one list"))?;
            // Read before locking: `a.extend(a)` iterates `a` while it is being changed.
            let more = collect(&v)?;
            lock(&self.0).extend(more);
            return Ok(Value::from(()));
        }
        let mut items = lock(&self.0);
        match method {
            "append" => {
                let (v,): (Value,) = from_args(args).map_err(|_| bad("`append()` takes one value"))?;
                items.push(v);
                Ok(Value::from(()))
            }
            "insert" => {
                let (i, v): (i64, Value) =
                    from_args(args).map_err(|_| bad("`insert()` takes an index and a value"))?;
                // Python clamps instead of failing: past the end appends, before the start prepends.
                let at = if i < 0 {
                    (i + items.len() as i64).max(0)
                } else {
                    i.min(items.len() as i64)
                };
                items.insert(at as usize, v);
                Ok(Value::from(()))
            }
            "pop" => {
                let (i,): (Option<i64>,) =
                    from_args(args).map_err(|_| bad("`pop()` takes an optional index"))?;
                let at = match i {
                    Some(i) => index(i, items.len()),
                    None => items.len().checked_sub(1),
                };
                at.map(|at| items.remove(at))
                    .ok_or_else(|| bad("`pop()` from an empty list or an index out of range"))
            }
            "remove" => {
                let (v,): (Value,) = from_args(args).map_err(|_| bad("`remove()` takes one value"))?;
                let at = items
                    .iter()
                    .position(|x| *x == v)
                    .ok_or_else(|| bad(format!("`remove()`: {v} is not in the list")))?;
                items.remove(at);
                Ok(Value::from(()))
            }
            "clear" => {
                from_args::<()>(args).map_err(|_| bad("`clear()` takes no arguments"))?;
                items.clear();
                Ok(Value::from(()))
            }
            "reverse" => {
                from_args::<()>(args).map_err(|_| bad("`reverse()` takes no arguments"))?;
                items.reverse();
                Ok(Value::from(()))
            }
            "sort" => {
                let (kwargs,): (Kwargs,) =
                    from_args(args).map_err(|_| bad("`sort()` takes only `reverse=`"))?;
                let reverse: Option<bool> = kwargs.get("reverse")?;
                kwargs.assert_all_used()?;
                items.sort();
                if reverse == Some(true) {
                    items.reverse();
                }
                Ok(Value::from(()))
            }
            "index" => {
                let (v,): (Value,) = from_args(args).map_err(|_| bad("`index()` takes one value"))?;
                items
                    .iter()
                    .position(|x| *x == v)
                    .map(|i| Value::from(i as i64))
                    .ok_or_else(|| bad(format!("`index()`: {v} is not in the list")))
            }
            "count" => {
                let (v,): (Value,) = from_args(args).map_err(|_| bad("`count()` takes one value"))?;
                Ok(Value::from(items.iter().filter(|x| **x == v).count() as i64))
            }
            "copy" => {
                from_args::<()>(args).map_err(|_| bad("`copy()` takes no arguments"))?;
                Ok(Value::from_object(MutableList(Mutex::new(items.clone()))))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }
}

#[derive(Debug)]
struct MutableDict(Mutex<Vec<(Value, Value)>>);

/// Set `key` to `value`, keeping the position of an existing key (as Python does).
fn put(pairs: &mut Vec<(Value, Value)>, key: Value, value: Value) {
    match pairs.iter_mut().find(|(k, _)| *k == key) {
        Some((_, v)) => *v = value,
        None => pairs.push((key, value)),
    }
}

fn merge(pairs: &mut Vec<(Value, Value)>, src: &Value) -> Result<(), Error> {
    if src.kind() != ValueKind::Map {
        return Err(bad(format!("expected a mapping, got {}", src.kind())));
    }
    for k in src.try_iter()? {
        let v = src.get_item(&k)?;
        put(pairs, k, v);
    }
    Ok(())
}

fn merge_kwargs(pairs: &mut Vec<(Value, Value)>, kwargs: &Kwargs) -> Result<(), Error> {
    for k in kwargs.args() {
        let v: Value = kwargs.get(k)?;
        put(pairs, Value::from(k), v);
    }
    Ok(())
}

impl Object for MutableDict {
    fn repr(self: &Arc<Self>) -> ObjectRepr {
        ObjectRepr::Map
    }

    fn get_value(self: &Arc<Self>, key: &Value) -> Option<Value> {
        lock(&self.0)
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
    }

    fn enumerate(self: &Arc<Self>) -> Enumerator {
        Enumerator::Iter(Box::new(
            lock(&self.0)
                .iter()
                .map(|(k, _)| k.clone())
                .collect::<Vec<_>>()
                .into_iter(),
        ))
    }

    fn enumerator_len(self: &Arc<Self>) -> Option<usize> {
        Some(lock(&self.0).len())
    }

    fn call_method(
        self: &Arc<Self>,
        _: &State<'_, '_>,
        method: &str,
        args: &[Value],
    ) -> Result<Value, Error> {
        if method == "update" {
            let (src, kwargs): (Option<Value>, Kwargs) =
                from_args(args).map_err(|_| bad("`update()` takes a mapping and/or `key=value` pairs"))?;
            // Read before locking: `d.update(d)` iterates `d` while it is being changed.
            let mut incoming = Vec::new();
            if let Some(src) = src {
                merge(&mut incoming, &src)?;
            }
            merge_kwargs(&mut incoming, &kwargs)?;
            let mut pairs = lock(&self.0);
            for (k, v) in incoming {
                put(&mut pairs, k, v);
            }
            return Ok(Value::from(()));
        }
        let mut pairs = lock(&self.0);
        match method {
            "get" => {
                let (k, default): (Value, Option<Value>) =
                    from_args(args).map_err(|_| bad("`get()` takes a key and an optional default"))?;
                Ok(pairs
                    .iter()
                    .find(|(x, _)| *x == k)
                    .map(|(_, v)| v.clone())
                    .or(default)
                    .unwrap_or_else(|| Value::from(())))
            }
            "setdefault" => {
                let (k, default): (Value, Option<Value>) =
                    from_args(args).map_err(|_| bad("`setdefault()` takes a key and an optional default"))?;
                if let Some((_, v)) = pairs.iter().find(|(x, _)| *x == k) {
                    return Ok(v.clone());
                }
                let v = default.unwrap_or_else(|| Value::from(()));
                pairs.push((k, v.clone()));
                Ok(v)
            }
            "pop" => {
                let (k, default): (Value, Option<Value>) =
                    from_args(args).map_err(|_| bad("`pop()` takes a key and an optional default"))?;
                match pairs.iter().position(|(x, _)| *x == k) {
                    Some(at) => Ok(pairs.remove(at).1),
                    None => default.ok_or_else(|| bad(format!("`pop()`: no key {k}"))),
                }
            }
            "clear" => {
                from_args::<()>(args).map_err(|_| bad("`clear()` takes no arguments"))?;
                pairs.clear();
                Ok(Value::from(()))
            }
            "keys" => Ok(Value::from(
                pairs.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>(),
            )),
            "values" => Ok(Value::from(
                pairs.iter().map(|(_, v)| v.clone()).collect::<Vec<_>>(),
            )),
            "items" => Ok(Value::from(
                pairs
                    .iter()
                    .map(|(k, v)| Value::from(vec![k.clone(), v.clone()]))
                    .collect::<Vec<_>>(),
            )),
            "copy" => {
                from_args::<()>(args).map_err(|_| bad("`copy()` takes no arguments"))?;
                Ok(Value::from_object(MutableDict(Mutex::new(pairs.clone()))))
            }
            _ => Err(Error::from(ErrorKind::UnknownMethod)),
        }
    }
}
