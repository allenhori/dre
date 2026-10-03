package main

// The protocol loop against a fake backend: framing, the handshake, every source request, and
// the error and exit rules (docs/protocol.md). The real connector is covered by the Rust test
// crates/dre-protocol/tests/go_plugins.rs, against a warehouse.

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"strings"
	"testing"

	"github.com/apache/arrow/go/v12/arrow"
	"github.com/apache/arrow/go/v12/arrow/array"
	"github.com/apache/arrow/go/v12/arrow/memory"
)

type fakeResult struct {
	schema *arrow.Schema
	recs   []arrow.Record
	i      int
}

func (r *fakeResult) Schema() (*arrow.Schema, error) { return r.schema, nil }
func (r *fakeResult) HasNext() bool                  { return r.i < len(r.recs) }
func (r *fakeResult) Next() (arrow.Record, error) {
	if r.i >= len(r.recs) {
		return nil, io.EOF
	}
	rec := r.recs[r.i]
	r.i++
	rec.Retain()
	return rec, nil
}

type fakeDB struct {
	ran    []string
	closed bool
}

var idSchema = arrow.NewSchema([]arrow.Field{{Name: "id", Type: arrow.PrimitiveTypes.Int64, Nullable: true}}, nil)

func ids(from, n int64) arrow.Record {
	b := array.NewInt64Builder(memory.DefaultAllocator)
	defer b.Release()
	for i := from; i < from+n; i++ {
		b.Append(i)
	}
	col := b.NewArray()
	defer col.Release()
	return array.NewRecord(idSchema, []arrow.Array{col}, n)
}

func (f *fakeDB) run(sql string, fn func(result) error) error {
	f.ran = append(f.ran, sql)
	switch {
	case strings.HasPrefix(strings.ToLower(sql), "create"):
		return fn(nil)
	case sql == "select three batches":
		return fn(&fakeResult{schema: idSchema, recs: []arrow.Record{ids(0, 2), ids(2, 2), ids(4, 2)}})
	case sql == "select nothing":
		return fn(&fakeResult{schema: idSchema})
	case sql == "select null":
		s := arrow.NewSchema([]arrow.Field{{Name: "n", Type: arrow.Null, Nullable: true}}, nil)
		col := array.NewNull(2)
		defer col.Release()
		return fn(&fakeResult{schema: s, recs: []arrow.Record{array.NewRecord(s, []arrow.Array{col}, 2)}})
	}
	return errors.New("[PARSE_SYNTAX_ERROR] Syntax error\n\tat org.apache.spark.Foo(Foo.scala:1)")
}

func (f *fakeDB) explain(sql string) (string, error) {
	if strings.Contains(sql, "nope") {
		return "Error occurred during query planning: \n[UNRESOLVED_COLUMN.WITHOUT_SUGGESTION] A column `nope` cannot be resolved.\n'Project ['nope]\n+- OneRowRelation", nil
	}
	return "== Physical Plan ==\n*(1) Project [1 AS 1#0]", nil
}

func (f *fakeDB) close() { f.closed = true }

// conversation runs serve over in-memory pipes.
type conversation struct {
	role role
	t    *testing.T
	in   *io.PipeWriter
	out  *bufio.Reader
	db   *fakeDB
	code chan int
}

func start(t *testing.T) *conversation { return startAs(t, sourceRole) }

func startAs(t *testing.T, r role) *conversation {
	inR, inW := io.Pipe()
	outR, outW := io.Pipe()
	c := &conversation{role: r, t: t, in: inW, out: bufio.NewReader(outR), db: &fakeDB{}, code: make(chan int, 1)}
	go func() {
		c.code <- serve(inR, outW, c.role, func(map[string]any) (backend, error) { return c.db, nil })
		outW.Close()
	}()
	return c
}

func (c *conversation) sendRaw(b []byte) {
	if _, err := c.in.Write(b); err != nil {
		c.t.Fatal(err)
	}
}

func (c *conversation) send(v any) {
	b, _ := json.Marshal(v)
	c.sendRaw(frameBytes(tagJSON, b))
}

func (c *conversation) sendRecord(rec arrow.Record) {
	b, err := encodeRecord(rec)
	if err != nil {
		c.t.Fatal(err)
	}
	c.sendRaw(frameBytes(tagArrow, b))
}

func frameBytes(tag byte, body []byte) []byte {
	out := make([]byte, 5, 5+len(body))
	binary.BigEndian.PutUint32(out, uint32(len(body)+1))
	out[4] = tag
	return append(out, body...)
}

func (c *conversation) reply() map[string]any {
	c.t.Helper()
	f, err := readFrame(c.out)
	if err != nil {
		c.t.Fatalf("reading a reply: %v", err)
	}
	if f.tag != tagJSON {
		c.t.Fatalf("expected a JSON reply, got an Arrow frame")
	}
	var m map[string]any
	json.Unmarshal(f.body, &m)
	return m
}

func (c *conversation) records() []arrow.Record {
	c.t.Helper()
	f, err := readFrame(c.out)
	if err != nil || f.tag != tagArrow {
		c.t.Fatalf("expected an Arrow frame (%v)", err)
	}
	_, recs, err := decodeRecords(f.body)
	if err != nil {
		c.t.Fatal(err)
	}
	return recs
}

func (c *conversation) hello() {
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 3, "core_version": "test"})
	r := c.reply()
	if r["type"] != "hello" || r["protocol_version"] != 0.0 || r["name"] != "databricks" || r["kind"] != "source" {
		c.t.Fatalf("hello: %v", r)
	}
}

func expectError(t *testing.T, r map[string]any, contains string) {
	t.Helper()
	if r["type"] != "error" || !strings.Contains(r["message"].(string), contains) {
		t.Fatalf("expected an error containing %q, got %v", contains, r)
	}
}

func TestHandshakeDescribeAndRequestErrors(t *testing.T) {
	c := start(t)
	c.send(map[string]any{"type": "describe"})
	expectError(t, c.reply(), "the first request must be `hello`")
	c.hello()
	c.send(map[string]any{"type": "describe"})
	d := c.reply()
	fields := d["connection_fields"].([]any)
	var names []string
	for _, f := range fields {
		names = append(names, f.(map[string]any)["name"].(string))
	}
	if strings.Join(names, ",") != "host,http_path,auth_type,token,client_id,client_secret,catalog,schema" {
		t.Fatalf("fields: %v", names)
	}
	if d["identifier_quote"] != "`" {
		t.Fatalf("identifier_quote: %v", d["identifier_quote"])
	}
	c.send(map[string]any{"type": "nonsense"})
	expectError(t, c.reply(), "unsupported request `nonsense`")
	c.send(map[string]any{"type": "write", "path": "x", "format": "csv", "options": map[string]any{}, "result_sets": []any{}})
	expectError(t, c.reply(), "a source plugin doesn't handle write requests")
	c.sendRecord(ids(0, 1))
	expectError(t, c.reply(), "unexpected Arrow frame")
	c.send(map[string]any{"type": "execute", "sql": "select 1"})
	expectError(t, c.reply(), "no open session")
	c.send(map[string]any{"type": "close"})
	if r := c.reply(); r["type"] != "ok" {
		t.Fatalf("close: %v", r)
	}
	if code := <-c.code; code != 0 {
		t.Fatalf("exit %d", code)
	}
}

func TestExecuteStreamsResultsWithinTheRowLimit(t *testing.T) {
	c := start(t)
	c.hello()
	c.send(map[string]any{"type": "open", "connection": map[string]any{}, "read_only": false})
	if r := c.reply(); r["type"] != "ok" {
		t.Fatalf("open: %v", r)
	}
	// No result set.
	c.send(map[string]any{"type": "execute", "sql": "create temp view v"})
	if r := c.reply(); r["type"] != "no_result" {
		t.Fatalf("%v", r)
	}
	// Three batches, all sent.
	c.send(map[string]any{"type": "execute", "sql": "select three batches"})
	if r := c.reply(); r["type"] != "result" || r["columns"].([]any)[0] != "id" {
		t.Fatalf("%v", r)
	}
	for range 3 {
		c.records()
	}
	if r := c.reply(); r["type"] != "result_end" || r["rows"] != 6.0 {
		t.Fatalf("%v", r)
	}
	// row_limit 3: a batch and a slice, then stop.
	c.send(map[string]any{"type": "execute", "sql": "select three batches", "row_limit": 3})
	c.reply()
	a, b := c.records(), c.records()
	if a[0].NumRows() != 2 || b[0].NumRows() != 1 || b[0].Column(0).(*array.Int64).Value(0) != 2 {
		t.Fatalf("limited batches: %d, %d", a[0].NumRows(), b[0].NumRows())
	}
	if r := c.reply(); r["rows"] != 3.0 {
		t.Fatalf("%v", r)
	}
	// Zero rows: still one frame carrying the columns.
	c.send(map[string]any{"type": "execute", "sql": "select nothing"})
	c.reply()
	if recs := c.records(); len(recs) != 1 || recs[0].NumRows() != 0 || recs[0].Schema().Field(0).Name != "id" {
		t.Fatalf("empty result: %v", recs)
	}
	if r := c.reply(); r["rows"] != 0.0 {
		t.Fatalf("%v", r)
	}
	// A NULL-typed column arrives as text.
	c.send(map[string]any{"type": "execute", "sql": "select null"})
	c.reply()
	if recs := c.records(); recs[0].Schema().Field(0).Type.ID() != arrow.STRING || recs[0].Column(0).NullN() != 2 {
		t.Fatalf("null column: %v", recs[0].Schema())
	}
	c.reply()
	// Errors lose the JVM stack trace, and the plugin keeps serving.
	c.send(map[string]any{"type": "execute", "sql": "bad sql"})
	r := c.reply()
	expectError(t, r, "[PARSE_SYNTAX_ERROR]")
	if strings.Contains(r["message"].(string), "scala") {
		t.Fatalf("stack trace kept: %v", r)
	}
	// check: ok, then a planning error reported without the plan tree.
	c.send(map[string]any{"type": "check", "sql": "select 1"})
	if r := c.reply(); r["type"] != "ok" {
		t.Fatalf("%v", r)
	}
	c.send(map[string]any{"type": "check", "sql": "select nope"})
	r = c.reply()
	expectError(t, r, "UNRESOLVED_COLUMN")
	if strings.Contains(r["message"].(string), "Project") {
		t.Fatalf("plan tree kept: %v", r)
	}
	// End of input closes the session and exits 0.
	c.in.Close()
	if code := <-c.code; code != 0 || !c.db.closed {
		t.Fatalf("exit %d, closed %v", code, c.db.closed)
	}
}

func TestLoadBuildsOneTempView(t *testing.T) {
	c := start(t)
	c.hello()
	c.send(map[string]any{"type": "open", "connection": map[string]any{}, "read_only": false})
	c.reply()
	c.send(map[string]any{"type": "load", "name": "countries"})
	c.sendRecord(ids(0, 2))
	c.sendRecord(ids(2, 1))
	c.send(map[string]any{"type": "result_set_end"})
	r := c.reply()
	if r["type"] != "loaded" || r["relation"] != "dre_lookup_countries" || r["rows"] != 3.0 || r["warning"] == nil {
		t.Fatalf("%v", r)
	}
	sql := c.db.ran[len(c.db.ran)-1]
	if !strings.HasPrefix(sql, "CREATE OR REPLACE TEMPORARY VIEW dre_lookup_countries AS SELECT CAST(`id` AS BIGINT) AS `id` FROM VALUES\n  (0),\n  (1),\n  (2)") {
		t.Fatalf("%s", sql)
	}
	// A bad name is refused after the rows are read, so the stream stays in step.
	c.send(map[string]any{"type": "load", "name": "no-dashes"})
	c.sendRecord(ids(0, 1))
	c.send(map[string]any{"type": "result_set_end"})
	expectError(t, c.reply(), "isn't a valid view name")
	c.send(map[string]any{"type": "describe"})
	if r := c.reply(); r["type"] != "describe" {
		t.Fatalf("out of step: %v", r)
	}
}

func TestMalformedFramesAndVersionMismatchExit(t *testing.T) {
	c := start(t)
	c.sendRaw([]byte{0, 0, 0, 2, 'X', 0})
	expectError(t, c.reply(), "unknown frame type byte 0x58")
	if code := <-c.code; code != 2 {
		t.Fatalf("exit %d", code)
	}
	c = start(t)
	c.send(map[string]any{"type": "hello", "min_version": 5, "max_version": 6, "core_version": "x"})
	if r := c.reply(); r["type"] != "version_mismatch" || r["max_version"] != 0.0 {
		t.Fatalf("%v", r)
	}
	if code := <-c.code; code != 1 {
		t.Fatalf("exit %d", code)
	}
}

func TestFramesRoundTripAndRejectBadLengths(t *testing.T) {
	var buf bytes.Buffer
	w := bufio.NewWriter(&buf)
	writeJSON(w, map[string]any{"type": "ok"})
	b, _ := encodeRecord(ids(0, 3))
	writeFrame(w, tagArrow, b)
	f, err := readFrame(&buf)
	if err != nil || f.tag != tagJSON || string(f.body) != `{"type":"ok"}` {
		t.Fatalf("%v %s", err, f.body)
	}
	f, _ = readFrame(&buf)
	if _, recs, err := decodeRecords(f.body); err != nil || recs[0].NumRows() != 3 {
		t.Fatalf("%v", err)
	}
	if _, err := readFrame(&buf); err != errEOF {
		t.Fatalf("clean end: %v", err)
	}
	if _, err := readFrame(bytes.NewReader([]byte{0, 0, 0, 0})); err == nil {
		t.Fatal("zero length accepted")
	}
	if _, err := readFrame(bytes.NewReader([]byte{0, 0, 0, 9, 'J', '{'})); err == nil || err == errEOF {
		t.Fatalf("truncated body: %v", err)
	}
}

func TestHelloServesThePluginCoreAsksFor(t *testing.T) {
	c := start(t)
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t"})
	r := c.reply()
	if r["kind"] != "source" || r["name"] != "databricks" || fmt.Sprint(r["provides"]) != "[source/databricks destination/databricks]" {
		t.Fatalf("%v", r)
	}
	c = startAs(t, sourceRole)
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t", "plugin": "destination/databricks"})
	if r := c.reply(); r["kind"] != "destination" {
		t.Fatalf("%v", r)
	}
	c = startAs(t, sourceRole)
	c.send(map[string]any{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "t", "plugin": "destination/databricks_volumes"})
	expectError(t, c.reply(), "this executable provides source/databricks, destination/databricks, not destination/databricks_volumes")
	if code := <-c.code; code != 1 {
		t.Fatalf("exit code %d", code)
	}
}

func TestTheDestinationRoutesByPath(t *testing.T) {
	for _, bad := range []string{"", "/tmp/x.csv", "Volumez/c/s/v/x"} {
		if _, err := deliver("/x", bad, map[string]any{}); err == nil {
			t.Fatalf("%q was accepted", bad)
		}
	}
	if _, err := deliver("/x", "/Volumes/c/s", map[string]any{}); err == nil || !strings.Contains(err.Error(), "/Volumes/<catalog>") {
		t.Fatalf("%v", err)
	}
	if _, err := deliver("/x", "/Workspace/Nope/x", map[string]any{}); err == nil || !strings.Contains(err.Error(), "workspace file path") {
		t.Fatalf("%v", err)
	}
}
