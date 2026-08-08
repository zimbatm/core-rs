package main

import (
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"fmt"
	"os"

	"github.com/zeebo/blake3"
)

// splitmixNext advances the splitmix64 generator exactly as VECTORS.md defines.
func splitmixNext(state *uint64) uint64 {
	*state += 0x9E3779B97F4A7C15
	z := *state
	z = (z ^ (z >> 30)) * 0xBF58476D1CE4E5B9
	z = (z ^ (z >> 27)) * 0x94D049BB133111EB
	return z ^ (z >> 31)
}

// smData is data(seed, n): splitmix64 outputs appended as 8 little-endian
// bytes, truncated to n bytes.
func smData(seed uint64, n int) []byte {
	out := make([]byte, 0, n+8)
	state := seed
	for len(out) < n {
		out = binary.LittleEndian.AppendUint64(out, splitmixNext(&state))
	}
	return out[:n]
}

// u64s is u64s(seed, n): the first n splitmix64 outputs.
func u64s(seed uint64, n int) []uint64 {
	out := make([]uint64, n)
	state := seed
	for i := range out {
		out[i] = splitmixNext(&state)
	}
	return out
}

// Payload is the JSON description of a deterministic byte payload:
// {"kind":"splitmix","seed":S,"len":N}, {"kind":"const","byte":B,"len":N}, or
// {"kind":"concat","parts":[...]}.
type Payload struct {
	Kind  string    `json:"kind"`
	Seed  *uint64   `json:"seed,omitempty"`
	Byte  *int      `json:"byte,omitempty"`
	Len   *int      `json:"len,omitempty"`
	Parts []Payload `json:"parts,omitempty"`
}

// SM describes data(seed, n).
func SM(seed uint64, n int) Payload {
	return Payload{Kind: "splitmix", Seed: &seed, Len: &n}
}

// Const describes n copies of byte b.
func Const(b byte, n int) Payload {
	bi := int(b)
	return Payload{Kind: "const", Byte: &bi, Len: &n}
}

// Concat describes the concatenation of parts.
func Concat(parts ...Payload) Payload {
	return Payload{Kind: "concat", Parts: parts}
}

// Materialize produces the payload's bytes.
func (p Payload) Materialize() []byte {
	switch p.Kind {
	case "splitmix":
		return smData(*p.Seed, *p.Len)
	case "const":
		out := make([]byte, *p.Len)
		for i := range out {
			out[i] = byte(*p.Byte)
		}
		return out
	case "concat":
		var out []byte
		for _, part := range p.Parts {
			out = append(out, part.Materialize()...)
		}
		return out
	default:
		panic(fmt.Sprintf("unknown payload kind %q", p.Kind))
	}
}

// writeJSON marshals v with two-space indentation plus a trailing newline.
// Determinism: v must be built from structs (and ordered slices) only, never
// bare maps, so field order is fixed by declaration order.
func writeJSON(path string, v any) error {
	b, err := json.MarshalIndent(v, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(b, '\n'), 0o644)
}

// blake3Hex returns the lowercase hex BLAKE3-256 digest of b.
func blake3Hex(b []byte) string {
	sum := blake3.Sum256(b)
	return hex.EncodeToString(sum[:])
}
