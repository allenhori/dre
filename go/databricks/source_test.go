package main

import (
	"errors"
	"strings"
	"testing"

	"github.com/apache/arrow/go/v12/arrow"
	"github.com/apache/arrow/go/v12/arrow/array"
	"github.com/apache/arrow/go/v12/arrow/memory"
)

func TestTempViewSQLEscapesForSparkAndCastsEveryColumn(t *testing.T) {
	schema := arrow.NewSchema([]arrow.Field{
		{Name: "code", Type: arrow.BinaryTypes.String, Nullable: true},
		{Name: "n", Type: arrow.PrimitiveTypes.Int64, Nullable: true},
		{Name: "d", Type: arrow.FixedWidthTypes.Date32, Nullable: true},
	}, nil)
	b := array.NewRecordBuilder(memory.DefaultAllocator, schema)
	defer b.Release()
	b.Field(0).(*array.StringBuilder).AppendValues([]string{`O'Brien \ Co`, ""}, []bool{true, false})
	b.Field(1).(*array.Int64Builder).AppendValues([]int64{1, 0}, []bool{true, false})
	b.Field(2).(*array.Date32Builder).AppendValues([]arrow.Date32{20454, 0}, []bool{true, false})
	rec := b.NewRecord()
	defer rec.Release()
	sql, n, err := tempViewSQL("dre_lookup_x", schema, []arrow.Record{rec})
	want := "CREATE OR REPLACE TEMPORARY VIEW dre_lookup_x AS SELECT CAST(`code` AS STRING) AS `code`, " +
		"CAST(`n` AS BIGINT) AS `n`, CAST(`d` AS DATE) AS `d` FROM VALUES\n  " +
		`('O\'Brien \\ Co', 1, DATE'2026-01-01'),` + "\n  (NULL, NULL, NULL)\nAS t(`code`, `n`, `d`)"
	if err != nil || n != 2 || sql != want {
		t.Fatalf("%v %d\n%s\nwant\n%s", err, n, sql, want)
	}
	sql, _, _ = tempViewSQL("v", schema, nil)
	if !strings.HasSuffix(sql, "VALUES (NULL, NULL, NULL) AS t(`code`, `n`, `d`) WHERE 1 = 0") {
		t.Fatal(sql)
	}
	bad := arrow.NewSchema([]arrow.Field{{Name: "b", Type: arrow.BinaryTypes.Binary}}, nil)
	if _, _, err := tempViewSQL("v", bad, nil); err == nil {
		t.Fatal("binary accepted")
	}
}

func TestErrorsKeepTheMessageNotTheStackTrace(t *testing.T) {
	err := cleanErr(errors.New("[TABLE_OR_VIEW_NOT_FOUND] The table `x` cannot be found.\n\tat org.apache.spark.Foo(Foo.scala:1)\n\tat more"))
	if err.Error() != "[TABLE_OR_VIEW_NOT_FOUND] The table `x` cannot be found." {
		t.Fatal(err)
	}
	if planError("== Physical Plan ==\n*(1) Project") != "" {
		t.Fatal("a good plan flagged")
	}
}

func TestHostsAndFields(t *testing.T) {
	for in, want := range map[string]string{
		"dbc-1.cloud.databricks.com":          "dbc-1.cloud.databricks.com",
		"https://dbc-1.cloud.databricks.com/": "dbc-1.cloud.databricks.com",
	} {
		if hostname(in) != want {
			t.Fatalf("%s → %s", in, hostname(in))
		}
	}
	if baseURL("dbc-1.cloud.databricks.com/") != "https://dbc-1.cloud.databricks.com" {
		t.Fatal(baseURL("dbc-1.cloud.databricks.com/"))
	}
	if _, err := required(map[string]any{}, "host"); err == nil || err.Error() != "the profile output needs a `host` field" {
		t.Fatal(err)
	}
	if n, ok := number("900"); !ok || n != 900 {
		t.Fatal(n)
	}
}
