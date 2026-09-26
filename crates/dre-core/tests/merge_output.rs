//! How output layers merge their `destination`.

use dre_core::project::merge_output;
use serde_yaml_ng::{Mapping, Value};

fn map(s: &str) -> Mapping {
    serde_yaml_ng::from_str(s).unwrap()
}

fn dest(m: &Mapping) -> Value {
    m.get("destination").cloned().unwrap_or(Value::Null)
}

#[test]
fn a_path_only_override_keeps_the_inherited_profile() {
    let mut base = map("destination: {profile: s3, path: a.csv}");
    assert_eq!(merge_output(&mut base, &map("destination: {path: b.csv}")), None);
    assert_eq!(dest(&base), Value::Mapping(map("{profile: s3, path: b.csv}")));
}

#[test]
fn a_different_profile_replaces_the_destination_instead_of_merging_options() {
    let mut base = map("destination: {profile: mail, to: a@example.com, subject: Hi}");
    merge_output(&mut base, &map("destination: {profile: slack, channel: C1}"));
    assert_eq!(dest(&base), Value::Mapping(map("{profile: slack, channel: C1}")));
}

#[test]
fn a_list_replaces_whatever_was_inherited() {
    let mut base = map("destination: {profile: s3, path: a.csv}");
    merge_output(
        &mut base,
        &map("destination: [{profile: s3, path: b.csv}, {profile: mail, to: x@example.com}]"),
    );
    assert_eq!(dest(&base).as_sequence().unwrap().len(), 2);
}

#[test]
fn a_path_only_override_of_a_one_entry_list_merges_into_that_entry() {
    let mut base = map("destination: [{profile: s3, path: a.csv}]");
    assert_eq!(merge_output(&mut base, &map("destination: {path: b.csv}")), None);
    assert_eq!(dest(&base), Value::Mapping(map("{profile: s3, path: b.csv}")));
}

#[test]
fn a_path_only_override_of_several_destinations_is_refused() {
    let mut base = map("destination: [{profile: s3, path: a.csv}, {profile: mail, to: x@example.com}]");
    let before = base.clone();
    let err = merge_output(&mut base, &map("destination: {path: b.csv}")).unwrap();
    assert!(err.contains("2 destinations"), "{err}");
    assert!(err.contains("full list"), "{err}");
    assert_eq!(base, before);
}
