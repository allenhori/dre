//! Schema snapshot protection and drift comparison.
//!
//! Public names match by value and occurrence. Protected names never persist a raw, hashed, or
//! otherwise guessable identity. When a protected entry cannot be matched unambiguously, the
//! unmatched pool is conservatively protected and matched by occurrence.

use std::collections::{BTreeMap, VecDeque};

use arrow::datatypes::{DataType, Field};
use serde_json::{Value as Json, json};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
enum Key {
    Name(String, usize),
    Protected(usize),
}

struct Column {
    name: String,
    data_type: String,
    type_shape: Option<Json>,
    type_protected: bool,
}

struct ResultSet {
    name: String,
    columns: BTreeMap<Key, Column>,
}

type Entries = BTreeMap<Key, ResultSet>;

/// Redact secret-derived fields without touching structural JSON keys.
pub(crate) fn redact(snapshot: &mut Json) {
    let Some(result_sets) = snapshot.get_mut("result_sets").and_then(Json::as_array_mut) else {
        return;
    };

    mask_names(result_sets);
    for result_set in result_sets {
        let Some(columns) = result_set.get_mut("columns").and_then(Json::as_array_mut) else {
            continue;
        };
        mask_names(columns);
        for column in columns {
            mask_field(column, "type", "type_protected");
        }
    }
}

fn mask_names(entries: &mut [Json]) {
    for entry in entries {
        mask_field(entry, "name", "name_protected");
    }
}

fn mask_field(entry: &mut Json, field: &str, marker: &str) {
    let Some(std::borrow::Cow::Owned(protected)) =
        entry.get(field).and_then(Json::as_str).map(crate::secrets::mask)
    else {
        return;
    };
    entry[field] = Json::String(protected);
    entry[marker] = Json::Bool(true);
}

fn is_protected(entry: &Json, marker: &str) -> bool {
    entry.get(marker).and_then(Json::as_bool).unwrap_or(false)
}

#[derive(Default)]
struct Protection {
    name: bool,
    data_type: bool,
    columns: Option<Box<Self>>,
}

impl Protection {
    fn include(&mut self, entry: &Json) {
        self.name |= is_protected(entry, "name_protected");
        self.data_type |= is_protected(entry, "type_protected");
        if let Some(columns) = entry.get("columns").and_then(Json::as_array) {
            let nested = self.columns.get_or_insert_with(Default::default);
            for column in columns {
                nested.include(column);
            }
        }
    }

    fn any(&self) -> bool {
        self.name || self.data_type || self.columns.as_deref().is_some_and(Self::any)
    }

    fn apply(&self, entry: &mut Json) {
        if self.name {
            force_field(entry, "name", "name_protected");
        }
        if self.data_type {
            force_field(entry, "type", "type_protected");
        }
        if let (Some(protection), Some(columns)) = (
            self.columns.as_deref(),
            entry.get_mut("columns").and_then(Json::as_array_mut),
        ) {
            for column in columns {
                protection.apply(column);
            }
        }
    }
}

fn force_field(entry: &mut Json, field: &str, marker: &str) {
    if entry.get(field).is_some() {
        entry[field] = Json::String(crate::secrets::MASK.to_string());
        entry[marker] = Json::Bool(true);
    }
}

/// Match public names exactly. If either side has unmatched protected data, protect the remaining
/// pool and align it by occurrence. This avoids persisting a reversible or guessable identity.
fn align_entries(previous: &mut [Json], current: &mut [Json]) -> Vec<(usize, usize)> {
    let mut current_by_name: BTreeMap<String, VecDeque<usize>> = BTreeMap::new();
    for (index, entry) in current.iter().enumerate() {
        if !is_protected(entry, "name_protected")
            && let Some(name) = entry.get("name").and_then(Json::as_str)
        {
            current_by_name
                .entry(name.to_string())
                .or_default()
                .push_back(index);
        }
    }

    let mut previous_matched = vec![false; previous.len()];
    let mut current_matched = vec![false; current.len()];
    let mut pairs = Vec::new();
    for (previous_index, entry) in previous.iter().enumerate() {
        if is_protected(entry, "name_protected") {
            continue;
        }
        let Some(name) = entry.get("name").and_then(Json::as_str) else {
            continue;
        };
        let Some(current_index) = current_by_name.get_mut(name).and_then(VecDeque::pop_front) else {
            continue;
        };
        previous_matched[previous_index] = true;
        current_matched[current_index] = true;
        pairs.push((previous_index, current_index));
    }

    let previous_unmatched: Vec<usize> = previous_matched
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| (!matched).then_some(index))
        .collect();
    let current_unmatched: Vec<usize> = current_matched
        .iter()
        .enumerate()
        .filter_map(|(index, matched)| (!matched).then_some(index))
        .collect();
    let mut protection = Protection::default();
    for &index in &previous_unmatched {
        protection.include(&previous[index]);
    }
    for &index in &current_unmatched {
        protection.include(&current[index]);
    }
    if protection.any() {
        for &index in &previous_unmatched {
            protection.apply(&mut previous[index]);
        }
        for &index in &current_unmatched {
            protection.apply(&mut current[index]);
        }
        pairs.extend(previous_unmatched.into_iter().zip(current_unmatched));
    }
    pairs
}

pub(crate) fn align_protection(previous: &mut Json, current: &mut Json) {
    let (Some(previous_sets), Some(current_sets)) = (
        previous.get_mut("result_sets").and_then(Json::as_array_mut),
        current.get_mut("result_sets").and_then(Json::as_array_mut),
    ) else {
        return;
    };
    let result_set_pairs = align_entries(previous_sets, current_sets);
    for (previous_index, current_index) in result_set_pairs {
        let (Some(previous_columns), Some(current_columns)) = (
            previous_sets[previous_index]
                .get_mut("columns")
                .and_then(Json::as_array_mut),
            current_sets[current_index]
                .get_mut("columns")
                .and_then(Json::as_array_mut),
        ) else {
            continue;
        };
        let column_pairs = align_entries(previous_columns, current_columns);
        for (previous_column, current_column) in column_pairs {
            if is_protected(&previous_columns[previous_column], "type_protected")
                || is_protected(&current_columns[current_column], "type_protected")
            {
                force_field(&mut previous_columns[previous_column], "type", "type_protected");
                force_field(&mut current_columns[current_column], "type", "type_protected");
            }
        }
    }
}

fn entry_key(entry: &Json, protected_count: &mut usize, name_counts: &mut BTreeMap<String, usize>) -> Key {
    if is_protected(entry, "name_protected") {
        let key = Key::Protected(*protected_count);
        *protected_count += 1;
        return key;
    }
    let name = entry.get("name").and_then(Json::as_str).unwrap_or("").to_string();
    let occurrence = name_counts.entry(name.clone()).or_default();
    let key = Key::Name(name, *occurrence);
    *occurrence += 1;
    key
}

fn entries(snapshot: &Json) -> Entries {
    let mut result = BTreeMap::new();
    let mut result_names = BTreeMap::new();
    let mut protected_results = 0;
    for result_set in snapshot["result_sets"].as_array().into_iter().flatten() {
        let key = entry_key(result_set, &mut protected_results, &mut result_names);
        let mut column_names = BTreeMap::new();
        let mut protected_columns = 0;
        let mut columns = BTreeMap::new();
        for column in result_set["columns"].as_array().into_iter().flatten() {
            let column_key = entry_key(column, &mut protected_columns, &mut column_names);
            columns.insert(
                column_key,
                Column {
                    name: column["name"].as_str().unwrap_or("").to_string(),
                    data_type: column["type"].as_str().unwrap_or("").to_string(),
                    type_shape: column.get("type_shape").cloned(),
                    type_protected: is_protected(column, "type_protected"),
                },
            );
        }
        result.insert(
            key,
            ResultSet {
                name: result_set["name"].as_str().unwrap_or("").to_string(),
                columns,
            },
        );
    }
    result
}

fn types_match(previous: &Column, current: &Column) -> bool {
    if previous.type_protected || current.type_protected {
        match (&previous.type_shape, &current.type_shape) {
            (Some(previous), Some(current)) => previous == current,
            // A protected snapshot written by the older format has no safe structural identity.
            // Treat this first comparison as migration; a successful run writes the new shape.
            _ => true,
        }
    } else {
        previous.data_type == current.data_type
    }
}

pub(crate) fn drift(previous: &Json, current: &Json) -> Vec<String> {
    let (previous, current) = (entries(previous), entries(current));
    let mut drift = Vec::new();
    for (identity, old_result) in &previous {
        let Some(new_result) = current.get(identity) else {
            drift.push(format!("result set `{}` is gone", old_result.name));
            continue;
        };
        for (column_identity, old_column) in &old_result.columns {
            match new_result.columns.get(column_identity) {
                None => drift.push(format!(
                    "`{}`: column `{}` removed",
                    old_result.name, old_column.name
                )),
                Some(new_column) if !types_match(old_column, new_column) => drift.push(format!(
                    "`{}`: column `{}` changed type from {} to {}",
                    old_result.name, old_column.name, old_column.data_type, new_column.data_type
                )),
                _ => {}
            }
        }
        for column_identity in new_result
            .columns
            .keys()
            .filter(|identity| !old_result.columns.contains_key(*identity))
        {
            let column = &new_result.columns[column_identity];
            drift.push(format!("`{}`: column `{}` added", old_result.name, column.name));
        }
    }
    for identity in current
        .keys()
        .filter(|identity| !previous.contains_key(*identity))
    {
        drift.push(format!("result set `{}` is new", current[identity].name));
    }
    drift
}

fn field_shape(field: &Field) -> Json {
    json!({
        "nullable": field.is_nullable(),
        "type": type_shape(field.data_type()),
    })
}

/// A comparison shape that retains type structure while omitting names, timezones, and metadata.
pub(crate) fn type_shape(data_type: &DataType) -> Json {
    match data_type {
        DataType::Timestamp(unit, _) => json!({ "timestamp": format!("{unit:?}") }),
        DataType::List(field) => json!({ "list": field_shape(field) }),
        DataType::ListView(field) => json!({ "list_view": field_shape(field) }),
        DataType::FixedSizeList(field, size) => {
            json!({ "fixed_size_list": { "size": size, "field": field_shape(field) } })
        }
        DataType::LargeList(field) => json!({ "large_list": field_shape(field) }),
        DataType::LargeListView(field) => json!({ "large_list_view": field_shape(field) }),
        DataType::Struct(fields) => {
            json!({ "struct": fields.iter().map(|field| field_shape(field)).collect::<Vec<_>>() })
        }
        DataType::Union(fields, mode) => json!({
            "union": {
                "mode": format!("{mode:?}"),
                "fields": fields
                    .iter()
                    .map(|(id, field)| json!({ "id": id, "field": field_shape(field) }))
                    .collect::<Vec<_>>(),
            }
        }),
        DataType::Dictionary(key, value) => json!({
            "dictionary": {
                "key": type_shape(key),
                "value": type_shape(value),
            }
        }),
        DataType::Map(field, sorted) => {
            json!({ "map": { "sorted": sorted, "field": field_shape(field) } })
        }
        DataType::RunEndEncoded(run_ends, values) => json!({
            "run_end_encoded": {
                "run_ends": field_shape(run_ends),
                "values": field_shape(values),
            }
        }),
        // All string-bearing and nested variants are handled above. The remaining displays contain
        // only Arrow-defined type names and numeric parameters.
        scalar => Json::String(scalar.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow::datatypes::Fields;

    #[test]
    fn type_shape_omits_nested_names_but_preserves_structure() {
        let nested =
            |name, data_type| DataType::Struct(Fields::from(vec![Field::new(name, data_type, true)]));

        let secret_name = type_shape(&nested("customer_secret", DataType::Int32));
        let public_name = type_shape(&nested("public_name", DataType::Int32));
        let changed_type = type_shape(&nested("customer_secret", DataType::Int64));

        assert_eq!(secret_name, public_name);
        assert_ne!(secret_name, changed_type);
        assert!(!secret_name.to_string().contains("customer_secret"));
    }
}
