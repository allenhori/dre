//! The shared statement splitter is its own seam: validation and the run path both use it.

use dre_core::sqlsplit::{StatementKind, classify, split};

fn texts(sql: &str) -> Vec<String> {
    split(sql).into_iter().map(|s| s.text).collect()
}

#[test]
fn splits_on_semicolons_and_drops_empty_statements() {
    assert_eq!(texts("select 1; select 2;\n;  \n"), vec!["select 1", "select 2"]);
}

#[test]
fn keeps_a_trailing_statement_without_semicolon() {
    assert_eq!(texts("select 1;\nselect 2"), vec!["select 1", "select 2"]);
}

#[test]
fn ignores_semicolons_inside_quotes_and_identifiers() {
    let sql = "select 'a;b', 'it''s;' as \"x;y\", `c;d` from t; select 2";
    assert_eq!(
        texts(sql),
        vec!["select 'a;b', 'it''s;' as \"x;y\", `c;d` from t", "select 2"]
    );
}

#[test]
fn ignores_semicolons_inside_comments() {
    let sql = "select 1 -- not; here\n; /* nor; /* nested; */ here; */ select 2";
    assert_eq!(
        texts(sql),
        vec!["select 1 -- not; here", "/* nor; /* nested; */ here; */ select 2"]
    );
}

#[test]
fn ignores_semicolons_inside_dollar_quoted_blocks() {
    let sql = "create function f() returns int as $$ begin; return 1; end; $$ language plpgsql;\n\
               do $body$ begin perform 1; end $body$; select 3";
    let parts = texts(sql);
    assert_eq!(parts.len(), 3);
    assert!(parts[0].ends_with("language plpgsql"));
    assert!(parts[1].starts_with("do $body$"));
    assert_eq!(parts[2], "select 3");
}

#[test]
fn a_comment_only_file_has_no_statements() {
    assert!(split("-- just a comment\n/* and another */\n").is_empty());
}

#[test]
fn records_the_starting_line_of_each_statement() {
    let lines: Vec<usize> = split("\n\nselect 1;\n\n  select\n 2;")
        .iter()
        .map(|s| s.line)
        .collect();
    assert_eq!(lines, vec![3, 5]);
}

#[test]
fn classifies_reads_and_temp_object_creates_as_allowed_in_unmanaged_reports() {
    for ok in [
        "select 1",
        "  -- leading comment\n SELECT * from t",
        "with x as (select 1) select * from x",
        "(select 1) union (select 2)",
        "create temp table t as select 1",
        "CREATE TEMPORARY VIEW v AS SELECT 1",
        "create or replace temp view v as select 1",
        "create or replace temporary table t (a int)",
    ] {
        assert!(classify(ok).is_read_only_safe(), "should be allowed: {ok}");
    }
    assert_eq!(
        classify("create temp table t as select 1"),
        StatementKind::TempCreate
    );
    assert_eq!(classify("select 1"), StatementKind::Read);
}

#[test]
fn classifies_everything_else_as_not_allowed() {
    for bad in [
        "delete from t",
        "insert into t values (1)",
        "drop table t",
        "create table t (a int)",
        "create or replace view v as select 1",
        "update t set a = 1",
        "merge into t using s on true when matched then delete",
        "values (1)",
    ] {
        assert!(!classify(bad).is_read_only_safe(), "should be refused: {bad}");
    }
}
