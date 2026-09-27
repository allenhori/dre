package main

// Framing (docs/protocol.md): a 4-byte big-endian length, then a body whose first byte is the
// frame type: 'J' for one JSON control message, 'A' for one Arrow IPC stream.

import (
	"bufio"
	"bytes"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"

	"github.com/apache/arrow/go/v12/arrow"
	"github.com/apache/arrow/go/v12/arrow/ipc"
	"github.com/apache/arrow/go/v12/arrow/memory"
)

const (
	tagJSON  = 'J'
	tagArrow = 'A'
	maxFrame = 1 << 30
)

// errEOF means core closed stdin between frames: a clean end.
var errEOF = errors.New("core closed the connection")

type frame struct {
	tag  byte
	body []byte // without the tag byte
}

func readFrame(r io.Reader) (frame, error) {
	var head [4]byte
	n, err := io.ReadFull(r, head[:])
	if n == 0 && (err == io.EOF || err == io.ErrUnexpectedEOF) {
		return frame{}, errEOF
	}
	if err != nil {
		return frame{}, fmt.Errorf("truncated frame header: %w", err)
	}
	size := binary.BigEndian.Uint32(head[:])
	if size == 0 || size > maxFrame {
		return frame{}, fmt.Errorf("bad frame length %d", size)
	}
	body := make([]byte, size)
	if _, err := io.ReadFull(r, body); err != nil {
		return frame{}, fmt.Errorf("truncated frame body: %w", err)
	}
	switch body[0] {
	case tagJSON:
		if !json.Valid(body[1:]) {
			return frame{}, errors.New("a JSON frame holds invalid JSON")
		}
		return frame{tag: tagJSON, body: body[1:]}, nil
	case tagArrow:
		return frame{tag: tagArrow, body: body[1:]}, nil
	default:
		return frame{}, fmt.Errorf("unknown frame type byte 0x%02x", body[0])
	}
}

func writeFrame(w *bufio.Writer, tag byte, body []byte) error {
	if len(body)+1 > maxFrame {
		return fmt.Errorf("frame of %d bytes is too large", len(body)+1)
	}
	var head [5]byte
	binary.BigEndian.PutUint32(head[:4], uint32(len(body)+1))
	head[4] = tag
	if _, err := w.Write(head[:]); err != nil {
		return err
	}
	if _, err := w.Write(body); err != nil {
		return err
	}
	return w.Flush()
}

func writeJSON(w *bufio.Writer, v any) error {
	b, err := json.Marshal(v)
	if err != nil {
		return err
	}
	return writeFrame(w, tagJSON, b)
}

// encodeRecord writes one record as a self-contained IPC stream (schema, batch, end marker).
func encodeRecord(rec arrow.Record) ([]byte, error) {
	var buf bytes.Buffer
	w := ipc.NewWriter(&buf, ipc.WithSchema(rec.Schema()), ipc.WithAllocator(memory.DefaultAllocator))
	if err := w.Write(rec); err != nil {
		return nil, err
	}
	if err := w.Close(); err != nil {
		return nil, err
	}
	return buf.Bytes(), nil
}

// decodeRecords reads every record of one IPC stream. The caller releases them.
func decodeRecords(b []byte) (*arrow.Schema, []arrow.Record, error) {
	r, err := ipc.NewReader(bytes.NewReader(b), ipc.WithAllocator(memory.DefaultAllocator))
	if err != nil {
		return nil, nil, fmt.Errorf("bad Arrow data: %w", err)
	}
	defer r.Release()
	var recs []arrow.Record
	for r.Next() {
		rec := r.Record()
		rec.Retain()
		recs = append(recs, rec)
	}
	if err := r.Err(); err != nil {
		for _, rec := range recs {
			rec.Release()
		}
		return nil, nil, fmt.Errorf("bad Arrow data: %w", err)
	}
	return r.Schema(), recs, nil
}
