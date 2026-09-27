// Command dre-databricks is DRE's Databricks adapter: one program, installed under two plugin
// names, that serves as
//
//   - dre-source-databricks: the source for Databricks SQL warehouses, and
//   - dre-destination-databricks_volumes: the destination for Unity Catalog Volumes, and
//   - dre-destination-databricks_workspace: the destination for workspace files (/Workspace/...).
//
// It picks its role from the name it was started as. Every role shares one sign-in (PAT or
// OAuth) and one OAuth session per workspace in ~/.dre/oauth_sessions.json.
//
// It is written in Go so the source can use Databricks' official Go connector
// (databricks-sql-go): SQL warehouses only hold sessions for Databricks' own clients, and one
// session per Binding is what keeps temp views and SETs alive between a report's queries. It
// speaks DRE's plugin protocol (docs/protocol.md) on stdin/stdout and logs to stderr.
//
// Source profile fields: host, http_path, auth_type (pat, the default, or oauth), token for pat,
// client_id / client_secret / scopes / redirect_port for oauth, optional catalog, schema and
// retry_timeout (seconds to keep retrying while a stopped warehouse starts; default 900).
// Destination profile fields: host and the same sign-in fields.
package main

import (
	"bufio"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime/debug"
	"sort"
	"strings"

	"github.com/apache/arrow/go/v12/arrow"
	sdklog "github.com/databricks/databricks-sdk-go/logger"
	dbsqllog "github.com/databricks/databricks-sql-go/logger"
)

// version is set at build time with -ldflags "-X main.version=<version>".
var version = "unreleased"

const (
	protocolMin = 0
	protocolMax = 0
)

// role is which plugin this program is serving as.
type role struct {
	kind         string // "source" or "destination"
	name         string
	capabilities []string
}

var (
	sourceRole      = role{kind: "source", name: "databricks", capabilities: []string{"sessions", "check", "load"}}
	destinationRole = role{kind: "destination", name: "databricks_volumes", capabilities: []string{}}
	workspaceRole   = role{kind: "destination", name: "databricks_workspace", capabilities: []string{}}
)

// roleOf picks the role from the executable's name: dre-destination-databricks_workspace and
// dre-destination-databricks_volumes serve those destinations, anything else
// (dre-source-databricks) the source.
func roleOf(exe string) role {
	base := strings.ToLower(filepath.Base(exe))
	switch {
	case strings.HasPrefix(base, "dre-destination-databricks_workspace"):
		return workspaceRole
	case strings.HasPrefix(base, "dre-destination-"):
		return destinationRole
	}
	return sourceRole
}

func main() {
	// The connector and SDK log their own copy of every error, and warnings DRE has no use for;
	// DRE reports errors itself. Set DATABRICKS_LOG_LEVEL (e.g. debug) to see the connector's log.
	quietLibraries()
	os.Exit(serve(os.Stdin, os.Stdout, roleOf(os.Args[0]), newDatabricks))
}

// quietLibraries turns off the connector's and SDK's own logs unless DATABRICKS_LOG_LEVEL is set.
func quietLibraries() {
	if os.Getenv("DATABRICKS_LOG_LEVEL") == "" {
		_ = dbsqllog.SetLogLevel("disabled")
		sdklog.DefaultLogger = &sdklog.SimpleLogger{Level: sdklog.LevelError + 1}
	}
}

// backend is one database session. The Databricks one wraps databricks-sql-go; tests use a fake.
type backend interface {
	// run executes one statement and hands its result set to fn, or nil when the statement
	// returned none. The result is only valid inside fn.
	run(sql string, fn func(result) error) error
	// explain returns the plan text of EXPLAIN <sql>.
	explain(sql string) (string, error)
	close()
}

// result is one statement's result set, batch by batch.
type result interface {
	Schema() (*arrow.Schema, error)
	HasNext() bool
	Next() (arrow.Record, error)
}

type opener func(conn map[string]any) (backend, error)

type server struct {
	role    role
	in      *bufio.Reader
	out     *bufio.Writer
	open    opener
	db      backend
	greeted bool
}

// exitCode ends serve with a process exit code.
type exitCode int

func serve(stdin io.Reader, stdout io.Writer, r role, open opener) (code int) {
	s := &server{role: r, in: bufio.NewReader(stdin), out: bufio.NewWriter(stdout), open: open}
	defer func() {
		if p := recover(); p != nil {
			if c, ok := p.(exitCode); ok {
				code = int(c)
				return
			}
			fmt.Fprintf(os.Stderr, "plugin panicked: %v\n%s", p, debug.Stack())
			s.send(map[string]any{"type": "error", "message": fmt.Sprintf("plugin panicked: %v", p)})
			code = 101
		}
	}()
	for {
		f, err := readFrame(s.in)
		if err == errEOF {
			s.closeDB()
			return 0
		}
		if err != nil {
			fmt.Fprintln(os.Stderr, err)
			s.send(errorReply(err))
			return 2
		}
		if f.tag == tagArrow {
			s.send(errorMsg("unexpected Arrow frame"))
			continue
		}
		var req map[string]json.RawMessage
		if err := json.Unmarshal(f.body, &req); err != nil {
			s.send(errorMsg("unsupported request `?`"))
			continue
		}
		t := str(req["type"])
		if t == "hello" {
			s.hello(req)
			continue
		}
		if !s.greeted {
			s.send(errorMsg("the first request must be `hello`"))
			continue
		}
		if t == "close" {
			s.closeDB()
			s.send(map[string]any{"type": "ok"})
			return 0
		}
		if err := s.handle(t, req); err != nil {
			s.send(errorReply(err))
		}
	}
}

func (s *server) send(v any) {
	if err := writeJSON(s.out, v); err != nil {
		// Core has gone; nothing left to talk to.
		panic(exitCode(1))
	}
}

func (s *server) sendRecord(rec arrow.Record) error {
	b, err := encodeRecord(rec)
	if err != nil {
		return err
	}
	if err := writeFrame(s.out, tagArrow, b); err != nil {
		panic(exitCode(1))
	}
	return nil
}

func (s *server) closeDB() {
	if s.db != nil {
		s.db.close()
		s.db = nil
	}
}

func (s *server) hello(req map[string]json.RawMessage) {
	var lo, hi int
	if json.Unmarshal(req["min_version"], &lo) != nil || json.Unmarshal(req["max_version"], &hi) != nil {
		s.send(errorMsg("unsupported request `hello`"))
		return
	}
	lo, hi = max(lo, protocolMin), min(hi, protocolMax)
	if lo > hi {
		s.send(map[string]any{"type": "version_mismatch", "min_version": protocolMin, "max_version": protocolMax})
		panic(exitCode(1))
	}
	s.greeted = true
	s.send(map[string]any{
		"type": "hello", "protocol_version": hi, "kind": s.role.kind, "name": s.role.name,
		"version": version, "capabilities": s.role.capabilities,
	})
}

func (s *server) handle(t string, req map[string]json.RawMessage) error {
	if s.role.kind == "destination" {
		return s.handleDestination(t, req)
	}
	switch t {
	case "describe":
		s.send(map[string]any{"type": "describe", "connection_fields": connectionFields()})
		return nil
	case "open":
		var conn map[string]any
		if err := json.Unmarshal(req["connection"], &conn); err != nil || conn == nil {
			return fmt.Errorf("unsupported request `open`")
		}
		s.closeDB()
		db, err := s.open(conn)
		if err != nil {
			return err
		}
		s.db = db
		s.send(map[string]any{"type": "ok"})
		return nil
	case "execute":
		var r struct {
			SQL      *string `json:"sql"`
			RowLimit *int64  `json:"row_limit"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.SQL == nil {
			return fmt.Errorf("unsupported request `execute`")
		}
		return s.execute(*r.SQL, r.RowLimit)
	case "check":
		var r struct {
			SQL *string `json:"sql"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.SQL == nil {
			return fmt.Errorf("unsupported request `check`")
		}
		if s.db == nil {
			return fmt.Errorf("no open session")
		}
		plan, err := s.db.explain(*r.SQL)
		if err != nil {
			return err
		}
		if e := planError(plan); e != "" {
			return fmt.Errorf("%s", e)
		}
		s.send(map[string]any{"type": "ok"})
		return nil
	case "load":
		var name string
		if json.Unmarshal(req["name"], &name) != nil {
			return fmt.Errorf("unsupported request `load`")
		}
		return s.load(name)
	case "write", "deliver", "result_set_end", "finish":
		return fmt.Errorf("a source plugin doesn't handle %s requests", t)
	default:
		return fmt.Errorf("unsupported request `%s`", t)
	}
}

func (s *server) handleDestination(t string, req map[string]json.RawMessage) error {
	switch t {
	case "describe":
		s.send(map[string]any{"type": "describe", "connection_fields": volumesFields()})
		return nil
	case "deliver":
		var r struct {
			LocalPath  *string `json:"local_path"`
			RemotePath *string `json:"remote_path"`
			Files      []struct {
				LocalPath  string  `json:"local_path"`
				RemotePath *string `json:"remote_path"`
			} `json:"files"`
			Connection map[string]any `json:"connection"`
			Options    map[string]any `json:"options"`
		}
		if json.Unmarshal(mustObject(req), &r) != nil || r.Connection == nil {
			return fmt.Errorf("unsupported request `deliver`")
		}
		// This destination takes no options, so a misspelt key is an error, not silently dropped.
		if len(r.Options) > 0 {
			keys := make([]string, 0, len(r.Options))
			for k := range r.Options {
				keys = append(keys, k)
			}
			sort.Strings(keys)
			return fmt.Errorf("this destination takes no options, but the destination entry has `%s`; check the key's spelling", keys[0])
		}
		var local, remote string
		switch {
		case r.LocalPath != nil && len(r.Files) == 0:
			local = *r.LocalPath
			if r.RemotePath != nil {
				remote = *r.RemotePath
			}
		case r.LocalPath == nil && len(r.Files) == 1:
			local = r.Files[0].LocalPath
			if r.Files[0].RemotePath != nil {
				remote = *r.Files[0].RemotePath
			}
		case r.LocalPath == nil && len(r.Files) > 1:
			return fmt.Errorf("this destination takes one file per delivery")
		default:
			return fmt.Errorf("`deliver` needs exactly one of `local_path` or `files`")
		}
		deliver := deliverToVolume
		if s.role.name == workspaceRole.name {
			deliver = deliverToWorkspace
		}
		loc, err := deliver(local, remote, r.Connection)
		if err != nil {
			return err
		}
		s.send(map[string]any{"type": "delivered", "location": loc})
		return nil
	case "open", "execute", "check", "load", "write", "result_set_end", "finish":
		return fmt.Errorf("a destination plugin doesn't handle %s requests", t)
	default:
		return fmt.Errorf("unsupported request `%s`", t)
	}
}

// execute runs one statement and streams its result, stopping at row_limit.
func (s *server) execute(sql string, rowLimit *int64) error {
	if s.db == nil {
		return fmt.Errorf("no open session")
	}
	return cleanErr(s.db.run(sql, func(res result) error {
		if res == nil {
			s.send(map[string]any{"type": "no_result"})
			return nil
		}
		return s.stream(res, rowLimit)
	}))
}

// stream sends one result set: `result`, Arrow frames (at least one), `result_end`.
func (s *server) stream(res result, rowLimit *int64) error {
	raw, err := res.Schema()
	if err != nil {
		return cleanErr(err)
	}
	schema := outputSchema(raw)
	cols := make([]string, len(schema.Fields()))
	for i, f := range schema.Fields() {
		cols[i] = f.Name
	}
	s.send(map[string]any{"type": "result", "columns": cols})
	var rows int64
	sent := false
	for res.HasNext() {
		if rowLimit != nil && rows >= *rowLimit {
			break
		}
		rec, err := res.Next()
		if err == io.EOF {
			break
		}
		if err != nil {
			// An error in place of result_end: the stream failed part-way.
			return cleanErr(err)
		}
		out, err := convertRecord(rec, schema)
		rec.Release()
		if err != nil {
			return err
		}
		n := out.NumRows()
		if rowLimit != nil && rows+n > *rowLimit {
			sliced := out.NewSlice(0, *rowLimit-rows)
			out.Release()
			out, n = sliced, *rowLimit-rows
		}
		if n > 0 || !sent {
			if err := s.sendRecord(out); err != nil {
				out.Release()
				return err
			}
			rows += n
			sent = true
		}
		out.Release()
	}
	if !sent {
		empty := emptyRecord(schema)
		err := s.sendRecord(empty)
		empty.Release()
		if err != nil {
			return err
		}
	}
	s.send(map[string]any{"type": "result_end", "rows": rows})
	return nil
}

// load reads the rows core streams after `load` and puts them in a temporary view.
func (s *server) load(name string) error {
	first, err := readFrame(s.in)
	if err != nil {
		return err
	}
	if first.tag != tagArrow {
		return fmt.Errorf("expected the rows to load, got %s", first.body)
	}
	schema, recs, err := decodeRecords(first.body)
	if err != nil {
		return err
	}
	defer func() {
		for _, r := range recs {
			r.Release()
		}
	}()
	// Read to result_set_end before anything can fail, so the stream stays in step.
	for {
		f, err := readFrame(s.in)
		if err != nil {
			return err
		}
		if f.tag == tagArrow {
			_, more, err := decodeRecords(f.body)
			if err != nil {
				return err
			}
			recs = append(recs, more...)
			continue
		}
		var m map[string]json.RawMessage
		if json.Unmarshal(f.body, &m) != nil || str(m["type"]) != "result_set_end" {
			return fmt.Errorf("expected result data, got %s", f.body)
		}
		break
	}
	if !validViewName(name) {
		return fmt.Errorf("`%s` isn't a valid view name", name)
	}
	if s.db == nil {
		return fmt.Errorf("no open session")
	}
	view := "dre_lookup_" + name
	sql, rows, err := tempViewSQL(view, schema, recs)
	if err != nil {
		return err
	}
	if err := s.db.run(sql, func(result) error { return nil }); err != nil {
		return cleanErr(err)
	}
	s.send(map[string]any{
		"type": "loaded", "relation": view, "rows": rows,
		"warning": fmt.Sprintf("Databricks has no bulk load over a SQL warehouse connection, so %d rows were sent as one SQL statement into a temporary view. Data this size probably belongs in a table in Databricks", rows),
	})
	return nil
}

func errorMsg(m string) map[string]any { return map[string]any{"type": "error", "message": m} }

func errorReply(err error) map[string]any { return errorMsg(err.Error()) }

func str(raw json.RawMessage) string {
	var s string
	_ = json.Unmarshal(raw, &s)
	return s
}

func mustObject(m map[string]json.RawMessage) []byte {
	b, _ := json.Marshal(m)
	return b
}

// connectionField mirrors the protocol's `describe` entry.
type connectionField struct {
	Name         string `json:"name"`
	Description  string `json:"description"`
	Required     bool   `json:"required"`
	Secret       bool   `json:"secret"`
	Default      any    `json:"default,omitempty"`
	SameAsSource string `json:"same_as_source,omitempty"`
}

func connectionFields() []connectionField {
	return []connectionField{
		{Name: "host", Description: "workspace host, e.g. adb-123.4.azuredatabricks.net", Required: true},
		{Name: "http_path", Description: "the SQL warehouse's HTTP path, e.g. /sql/1.0/warehouses/abc", Required: true},
		{Name: "auth_type", Description: "pat (a token) or oauth (browser sign-in; with client_id and client_secret, a service principal)", Default: "pat", SameAsSource: "databricks"},
		{Name: "token", Description: "personal access token, for auth_type pat", Secret: true, SameAsSource: "databricks"},
		{Name: "client_id", Description: "OAuth client; a service principal's application ID (browser sign-in defaults to databricks-cli)", SameAsSource: "databricks"},
		{Name: "client_secret", Description: "service principal OAuth secret", Secret: true, SameAsSource: "databricks"},
		{Name: "catalog", Description: "default catalog"},
		{Name: "schema", Description: "default schema"},
	}
}
