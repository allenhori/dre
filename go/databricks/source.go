package main

import (
	"context"
	"database/sql"
	"database/sql/driver"
	"fmt"
	"math"
	"net"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	dbsql "github.com/databricks/databricks-sql-go"
	dbsqlrows "github.com/databricks/databricks-sql-go/rows"

	"github.com/apache/arrow/go/v12/arrow"
	"github.com/apache/arrow/go/v12/arrow/array"
	"github.com/apache/arrow/go/v12/arrow/memory"
)

// databricks holds one connection from database/sql, which is one Databricks session: temp
// views and SETs made by one statement are there for the next.
type databricks struct {
	db   *sql.DB
	conn *sql.Conn
}

func newDatabricks(conn map[string]any) (backend, error) {
	host, err := required(conn, "host")
	if err != nil {
		return nil, err
	}
	path, err := required(conn, "http_path")
	if err != nil {
		return nil, err
	}
	if err := checkAuthType(conn); err != nil {
		return nil, err
	}
	if err := reachable(baseURL(host)); err != nil {
		return nil, err
	}
	auth, err := authFromConn(conn, baseURL(host))
	if err != nil {
		return nil, err
	}
	// Sign in before connecting. A sign-in that fails (no one at the terminal for the browser, a
	// revoked token) won't succeed by retrying, and inside the connector it would be retried for
	// as long as a starting warehouse is waited for.
	if _, err := auth.bearer(); err != nil {
		return nil, err
	}
	retry := 900
	if v, ok := number(conn["retry_timeout"]); ok {
		retry = v
	}
	opts := []dbsql.ConnOption{
		dbsql.WithServerHostname(hostname(host)),
		dbsql.WithPort(443),
		dbsql.WithHTTPPath(path),
		// DRE signs in itself (and shares the session with the Volumes destination); the
		// connector asks for the current token on every request.
		dbsql.WithExternalToken(auth.bearer),
		// Identify as DRE, appended to the connector's own client name.
		dbsql.WithUserAgentEntry("dre"),
		dbsql.WithSessionParams(map[string]string{"timezone": "UTC"}),
		dbsql.WithArrowNativeDecimal(true),
		// Retries cover a stopped warehouse starting up (HTTP 429/503).
		dbsql.WithRetries(max(retry/30, 1), time.Second, 30*time.Second),
	}
	catalog, schema := optional(conn, "catalog"), optional(conn, "schema")
	if catalog != "" || schema != "" {
		opts = append(opts, dbsql.WithInitialNamespace(catalog, schema))
	}
	c, err := dbsql.NewConnector(opts...)
	if err != nil {
		return nil, err
	}
	db := sql.OpenDB(c)
	db.SetMaxOpenConns(1)
	stop := waiting(fmt.Sprintf("the SQL warehouse %s", path))
	cn, err := db.Conn(context.Background())
	stop()
	if err != nil {
		db.Close()
		return nil, connectErr(err, auth)
	}
	return &databricks{db: db, conn: cn}, nil
}

func (d *databricks) run(query string, fn func(result) error) error {
	ctx := context.Background()
	err := d.conn.Raw(func(dc any) error {
		q, ok := dc.(driver.QueryerContext)
		if !ok {
			return fmt.Errorf("the Databricks connection can't run queries")
		}
		rows, err := q.QueryContext(ctx, query, nil)
		if err != nil {
			return err
		}
		defer rows.Close()
		// DDL, SET and the like: no columns, no result set.
		if len(rows.Columns()) == 0 {
			return fn(nil)
		}
		r, ok := rows.(dbsqlrows.Rows)
		if !ok {
			return fmt.Errorf("the Databricks connector didn't return Arrow results")
		}
		it, err := r.GetArrowBatches(ctx)
		if err != nil {
			return err
		}
		defer it.Close()
		return fn(it)
	})
	return cleanErr(err)
}

// explain returns EXPLAIN's plan text; Databricks sends it one line per row.
func (d *databricks) explain(query string) (string, error) {
	rows, err := d.conn.QueryContext(context.Background(), "EXPLAIN "+query)
	if err != nil {
		return "", cleanErr(err)
	}
	defer rows.Close()
	var lines []string
	for rows.Next() {
		var l sql.NullString
		if err := rows.Scan(&l); err != nil {
			return "", cleanErr(err)
		}
		lines = append(lines, l.String)
	}
	return strings.Join(lines, "\n"), cleanErr(rows.Err())
}

func (d *databricks) close() {
	d.conn.Close()
	d.db.Close()
}

// outputSchema is the schema sent to core: the columns as Databricks typed them, without the
// connector's Spark metadata, and NULL-typed columns (SELECT NULL) as text.
func outputSchema(s *arrow.Schema) *arrow.Schema {
	fields := make([]arrow.Field, len(s.Fields()))
	for i, f := range s.Fields() {
		t := f.Type
		if t.ID() == arrow.NULL {
			t = arrow.BinaryTypes.String
		}
		fields[i] = arrow.Field{Name: f.Name, Type: t, Nullable: true}
	}
	return arrow.NewSchema(fields, nil)
}

// convertRecord puts rec's columns under schema (from outputSchema). The caller releases it.
func convertRecord(rec arrow.Record, schema *arrow.Schema) (arrow.Record, error) {
	if int(rec.NumCols()) != len(schema.Fields()) {
		return nil, fmt.Errorf("a batch has %d columns but the result has %d", rec.NumCols(), len(schema.Fields()))
	}
	cols := make([]arrow.Array, rec.NumCols())
	for i := range cols {
		c := rec.Column(i)
		want := schema.Field(i).Type
		switch {
		case c.DataType().ID() == arrow.NULL && want.ID() == arrow.STRING:
			b := array.NewStringBuilder(memory.DefaultAllocator)
			for n := int64(0); n < rec.NumRows(); n++ {
				b.AppendNull()
			}
			cols[i] = b.NewArray()
			b.Release()
		case arrow.TypeEqual(c.DataType(), want):
			c.Retain()
			cols[i] = c
		default:
			for _, done := range cols[:i] {
				done.Release()
			}
			return nil, fmt.Errorf("column `%s` changed type from %s to %s between batches", schema.Field(i).Name, want, c.DataType())
		}
	}
	out := array.NewRecord(schema, cols, rec.NumRows())
	for _, c := range cols {
		c.Release()
	}
	return out, nil
}

func emptyRecord(schema *arrow.Schema) arrow.Record {
	b := array.NewRecordBuilder(memory.DefaultAllocator, schema)
	defer b.Release()
	return b.NewRecord()
}

// cleanErr keeps the useful part of a server error: the message, not the JVM stack trace.
func cleanErr(err error) error {
	if err == nil {
		return nil
	}
	return fmt.Errorf("%s", cleanMessage(err.Error()))
}

func cleanMessage(msg string) string {
	var keep []string
	for _, l := range strings.Split(msg, "\n") {
		if strings.HasPrefix(strings.TrimSpace(l), "at ") {
			break
		}
		keep = append(keep, l)
	}
	return strings.TrimSpace(strings.Join(keep, "\n"))
}

func connectErr(err error, a *auth) error {
	msg := cleanMessage(err.Error())
	hint := ""
	switch {
	case strings.Contains(msg, "401") || strings.Contains(msg, "403") || strings.Contains(strings.ToLower(msg), "unauthorized"):
		if a.oauth == nil {
			hint = " (check the token and that it can use this warehouse)"
		} else {
			hint = " (check that the signed-in identity can use this warehouse)"
		}
	case strings.Contains(msg, "404"):
		hint = " (check `host` and `http_path`)"
	}
	return fmt.Errorf("can't open a Databricks session%s: %s", hint, msg)
}

// planError finds a planning error that EXPLAIN reports inside its plan text rather than failing.
func planError(plan string) string {
	for _, marker := range []string{
		"Error occurred during query planning",
		"AnalysisException",
		"ParseException",
		"[UNRESOLVED",
		"[TABLE_OR_VIEW_NOT_FOUND",
	} {
		if strings.Contains(plan, marker) {
			// Keep the error, not the plan tree that follows it.
			var keep []string
			for _, l := range strings.Split(strings.TrimPrefix(strings.TrimSpace(plan), "== Physical Plan =="), "\n") {
				t := strings.TrimSpace(l)
				if t == "" {
					continue
				}
				if strings.HasPrefix(t, "'") || strings.HasPrefix(t, "+-") || strings.HasPrefix(t, ":") || strings.HasPrefix(t, "==") {
					break
				}
				keep = append(keep, t)
			}
			return cleanMessage(strings.Join(keep, "\n"))
		}
	}
	return ""
}

func validViewName(name string) bool {
	if name == "" {
		return false
	}
	for _, r := range name {
		if !(r == '_' || r >= '0' && r <= '9' || r >= 'a' && r <= 'z' || r >= 'A' && r <= 'Z') {
			return false
		}
	}
	return true
}

func quote(ident string) string {
	return "`" + strings.ReplaceAll(ident, "`", "``") + "`"
}

// tempViewSQL is a temporary view holding recs, as one VALUES statement. A SQL warehouse
// connection has no bulk path, so this is the only way in; every column is cast explicitly so
// an all-NULL column still gets its type.
func tempViewSQL(view string, schema *arrow.Schema, recs []arrow.Record) (string, int64, error) {
	var casts, names []string
	for _, f := range schema.Fields() {
		var t string
		switch f.Type.ID() {
		case arrow.STRING:
			t = "STRING"
		case arrow.INT64:
			t = "BIGINT"
		case arrow.FLOAT64:
			t = "DOUBLE"
		case arrow.BOOL:
			t = "BOOLEAN"
		case arrow.DATE32:
			t = "DATE"
		default:
			return "", 0, fmt.Errorf("can't load a column of type %s", f.Type)
		}
		casts = append(casts, fmt.Sprintf("CAST(%s AS %s) AS %s", quote(f.Name), t, quote(f.Name)))
		names = append(names, quote(f.Name))
	}
	var rows []string
	var n int64
	for _, rec := range recs {
		for r := 0; r < int(rec.NumRows()); r++ {
			vals := make([]string, rec.NumCols())
			for c := range vals {
				vals[c] = literal(rec.Column(c), r)
			}
			rows = append(rows, "("+strings.Join(vals, ", ")+")")
			n++
		}
	}
	var from string
	if len(rows) == 0 {
		nulls := strings.TrimSuffix(strings.Repeat("NULL, ", len(names)), ", ")
		from = fmt.Sprintf("VALUES (%s) AS t(%s) WHERE 1 = 0", nulls, strings.Join(names, ", "))
	} else {
		from = fmt.Sprintf("VALUES\n  %s\nAS t(%s)", strings.Join(rows, ",\n  "), strings.Join(names, ", "))
	}
	return fmt.Sprintf("CREATE OR REPLACE TEMPORARY VIEW %s AS SELECT %s FROM %s", view, strings.Join(casts, ", "), from), n, nil
}

// literal is one cell as a Spark SQL literal.
func literal(col arrow.Array, i int) string {
	if col.IsNull(i) {
		return "NULL"
	}
	switch c := col.(type) {
	case *array.String:
		v := strings.ReplaceAll(c.Value(i), `\`, `\\`)
		return "'" + strings.ReplaceAll(v, "'", `\'`) + "'"
	case *array.Int64:
		return strconv.FormatInt(c.Value(i), 10)
	case *array.Float64:
		v := c.Value(i)
		if math.IsNaN(v) || math.IsInf(v, 0) {
			return fmt.Sprintf("CAST('%s' AS DOUBLE)", strconv.FormatFloat(v, 'g', -1, 64))
		}
		return strconv.FormatFloat(v, 'g', -1, 64)
	case *array.Boolean:
		return strconv.FormatBool(c.Value(i))
	case *array.Date32:
		return "DATE'" + c.Value(i).ToTime().Format("2006-01-02") + "'"
	default:
		return "NULL"
	}
}

func required(conn map[string]any, key string) (string, error) {
	if v := optional(conn, key); v != "" {
		return v, nil
	}
	return "", fmt.Errorf("the profile output needs a `%s` field", key)
}

func optional(conn map[string]any, key string) string {
	s, _ := conn[key].(string)
	return s
}

func number(v any) (int, bool) {
	switch n := v.(type) {
	case float64:
		return int(n), true
	case string:
		i, err := strconv.Atoi(n)
		return i, err == nil
	}
	return 0, false
}

// baseURL is https://<host> for a bare host; a URL with a scheme is kept.
// reachable fails at once when the workspace can't be reached at all: a host name that doesn't
// resolve or a refused connection won't fix itself by retrying, unlike a warehouse that is
// starting. Behind an HTTP(S) proxy the proxy decides, so nothing is checked here.
func reachable(base string) error {
	u, err := url.Parse(base)
	if err != nil || u.Hostname() == "" {
		return fmt.Errorf("`host` %q isn't a workspace address", base)
	}
	req := &http.Request{URL: u}
	if p, err := http.ProxyFromEnvironment(req); err != nil || p != nil {
		return nil
	}
	port := u.Port()
	if port == "" {
		port = "443"
		if u.Scheme == "http" {
			port = "80"
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
	defer cancel()
	if _, err := net.DefaultResolver.LookupHost(ctx, u.Hostname()); err != nil {
		return fmt.Errorf("can't find the Databricks workspace %s: the host name doesn't resolve (check `host`): %v", u.Hostname(), err)
	}
	c, err := (&net.Dialer{}).DialContext(ctx, "tcp", net.JoinHostPort(u.Hostname(), port))
	if err != nil {
		return fmt.Errorf("can't reach the Databricks workspace %s:%s: %v", u.Hostname(), port, err)
	}
	c.Close()
	return nil
}

// waiting prints an info line every 30 s until the returned stop is called, so a person knows
// DRE is waiting (typically for a stopped warehouse to start) rather than stuck.
func waiting(what string) (stop func()) {
	done := make(chan struct{})
	start := time.Now()
	go func() {
		t := time.NewTicker(waitInterval)
		defer t.Stop()
		for {
			select {
			case <-done:
				return
			case <-t.C:
				fmt.Fprintf(os.Stderr, "info: waiting for %s to answer (%d s; a stopped warehouse takes a few minutes to start)\n",
					what, int(time.Since(start).Seconds()))
			}
		}
	}()
	return func() { close(done) }
}

var waitInterval = 30 * time.Second

func baseURL(host string) string {
	host = strings.TrimRight(host, "/")
	if strings.HasPrefix(host, "http://") || strings.HasPrefix(host, "https://") {
		return host
	}
	return "https://" + host
}

func hostname(host string) string {
	h := strings.TrimRight(host, "/")
	h = strings.TrimPrefix(h, "https://")
	return strings.TrimPrefix(h, "http://")
}
