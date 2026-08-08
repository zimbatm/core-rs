package main

import (
	"encoding/binary"
	"encoding/hex"
	"fmt"
	"path/filepath"
	"slices"

	"github.com/FastFilter/xorfilter"
)

type filterCase struct {
	N             int    `json:"n"`
	Seed          uint64 `json:"seed"`
	SectionBlake3 string `json:"section_blake3"`
	SectionHex    string `json:"section_hex,omitempty"` // only for n <= 100
}

type filtersFile struct {
	Cases []filterCase `json:"cases"`
}

// buildFilterSection replicates packstore's unexported buildFilterSection
// serialization (packstore/footer.go): 0x01 type byte, u64 BE seed, u32 BE
// SegmentLength / SegmentLengthMask / SegmentCount / SegmentCountLength, u32
// BE fingerprint count, then big-endian u16 fingerprints. inputs are
// deduplicated and sorted before the build, as packstore does.
func buildFilterSection(inputs []uint64) ([]byte, error) {
	const (
		filterHeaderSize       = 29
		filterTypeBinaryFuse16 = 1
	)
	tails := slices.Clone(inputs)
	slices.Sort(tails)
	tails = slices.Compact(tails)
	f, err := xorfilter.NewBinaryFuse[uint16](tails)
	if err != nil {
		return nil, fmt.Errorf("building fuse filter: %w", err)
	}
	out := make([]byte, filterHeaderSize+2*len(f.Fingerprints))
	out[0] = filterTypeBinaryFuse16
	binary.BigEndian.PutUint64(out[1:9], f.Seed)
	binary.BigEndian.PutUint32(out[9:13], f.SegmentLength)
	binary.BigEndian.PutUint32(out[13:17], f.SegmentLengthMask)
	binary.BigEndian.PutUint32(out[17:21], f.SegmentCount)
	binary.BigEndian.PutUint32(out[21:25], f.SegmentCountLength)
	binary.BigEndian.PutUint32(out[25:29], uint32(len(f.Fingerprints)))
	for i, fp := range f.Fingerprints {
		binary.BigEndian.PutUint16(out[filterHeaderSize+2*i:], fp)
	}
	return out, nil
}

// genFilters writes filters.json: binary-fuse filter sections over
// u64s(seed, n), for n in {1, 2, 3, 10, 100, 1000, 10000, 123456}.
func genFilters(outDir string) error {
	const seed = 42
	ns := []int{1, 2, 3, 10, 100, 1000, 10000, 123456}

	out := filtersFile{Cases: make([]filterCase, 0, len(ns))}
	for _, n := range ns {
		section, err := buildFilterSection(u64s(seed, n))
		if err != nil {
			return fmt.Errorf("n=%d: %w", n, err)
		}
		fc := filterCase{N: n, Seed: seed, SectionBlake3: blake3Hex(section)}
		if n <= 100 {
			fc.SectionHex = hex.EncodeToString(section)
		}
		out.Cases = append(out.Cases, fc)
	}
	return writeJSON(filepath.Join(outDir, "filters.json"), out)
}
