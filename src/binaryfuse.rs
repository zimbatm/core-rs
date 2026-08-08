//! Binary fuse filter with 16-bit fingerprints.
//!
//! A bit-for-bit port of `BinaryFuse[uint16]` from
//! `github.com/FastFilter/xorfilter` v0.5.1 (`binaryfusefilter.go` +
//! `xorfilter.go`): identical inputs produce an identical seed, geometry, and
//! fingerprint array on every platform. `packstore` relies on this — the
//! filter section of a sealed-segment footer is part of the byte-compatibility
//! contract (PORTING.md), so the construction must be deterministic
//! (`rngcounter` starts at 1) and must not depend on platform `libm`.
//!
//! This module also carries the filter *section* codec from Go
//! `packstore/footer.go` (`buildFilterSection` / `parseFilterSection`):
//! a type byte, big-endian seed + geometry header, and big-endian `u16`
//! fingerprints. `packstore` composes these into segment footers.
//!
//! Attribution: the construction and containment algorithms are ported from
//! FastFilter/xorfilter (Copyright Thomas Mueller Graf and Daniel Lemire,
//! Apache License 2.0). The private `go_log` function is ported from Go's
//! `math.Log` (BSD-style license, Copyright 2009 The Go Authors), which in
//! turn derives from FreeBSD's `/usr/src/lib/msun/src/e_log.c`:
//!
//! ```text
//! ====================================================
//! Copyright (C) 1993 by Sun Microsystems, Inc. All rights reserved.
//!
//! Developed at SunPro, a Sun Microsystems, Inc. business.
//! Permission to use, copy, modify, and distribute this
//! software is freely granted, provided that this notice
//! is preserved.
//! ====================================================
//! ```

/// Maximum number of construction iterations before [`BinaryFuse16::new`]
/// gives up (Go: `xorfilter.MaxIterations`). The probability of ever hitting
/// this is lower than the cosmic-ray probability.
pub const MAX_ITERATIONS: usize = 1024;

/// Serialized filter-section header length: type byte, u64 seed, four u32
/// geometry fields, u32 fingerprint count (Go: `packstore.filterHeaderSize`).
pub const SECTION_HEADER_SIZE: usize = 29;

/// Section type byte for a binary fuse filter with 16-bit fingerprints
/// (Go: `packstore.filterTypeBinaryFuse16`).
pub const SECTION_TYPE_BINARY_FUSE16: u8 = 1;

/// Errors from filter construction and section parsing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Error {
    /// Construction did not converge within [`MAX_ITERATIONS`]
    /// (Go: `errors.New("too many iterations")`).
    #[error("too many iterations")]
    TooManyIterations,
    /// The filter section bytes are invalid. The message carries the same
    /// diagnostic detail Go attaches to `packstore.ErrCorrupt`; callers that
    /// wrap this (packstore) should classify it as corrupt data.
    #[error("{0}")]
    Corrupt(String),
}

impl Error {
    /// Reports whether this error is a corrupt-data condition
    /// (Go: `errors.Is(err, ErrCorrupt)`).
    pub fn is_corrupt(&self) -> bool {
        matches!(self, Error::Corrupt(_))
    }
}

/// A binary fuse filter with 16-bit fingerprints
/// (Go: `xorfilter.BinaryFuse[uint16]`).
///
/// The fields mirror Go's exported fields; they are `pub` so `packstore` can
/// inspect them, but the invariants between them are only guaranteed for
/// filters produced by [`BinaryFuse16::new`] or [`BinaryFuse16::parse_section`].
/// [`BinaryFuse16::contains`] indexes `fingerprints` using the geometry
/// fields and, like Go, panics if a hand-assembled filter violates the
/// construction invariants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinaryFuse16 {
    pub seed: u64,
    pub segment_length: u32,
    pub segment_length_mask: u32,
    pub segment_count: u32,
    pub segment_count_length: u32,
    pub fingerprints: Vec<u16>,
}

// ---------------------------------------------------------------------------
// Hashing primitives (Go: xorfilter.go).

fn murmur64(mut h: u64) -> u64 {
    h ^= h >> 33;
    h = h.wrapping_mul(0xff51_afd7_ed55_8ccd);
    h ^= h >> 33;
    h = h.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    h ^= h >> 33;
    h
}

/// Returns a random number; modifies the seed (Go: `splitmix64`).
fn splitmix64(seed: &mut u64) -> u64 {
    *seed = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *seed;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

fn mixsplit(key: u64, seed: u64) -> u64 {
    murmur64(key.wrapping_add(seed))
}

fn fingerprint(hash: u64) -> u64 {
    hash ^ (hash >> 32)
}

/// Sorts and deduplicates in place (Go: `pruneDuplicates`).
fn prune_duplicates(keys: &mut Vec<u64>) {
    keys.sort_unstable();
    keys.dedup();
}

// ---------------------------------------------------------------------------
// Portable float math (Go: math.Log, math.Round, math.Frexp, math.Max).
//
// PORTING.md requires the filter geometry derivation to be bit-identical on
// every platform. Go's portable math.Log (the FDLIBM algorithm above) is
// ported verbatim rather than calling `f64::ln`, whose platform libm may
// differ in the last ulp and shift the derived integer geometry.

/// Go `math.Log` (portable FDLIBM version), bit-exact.
// The constants keep Go's exact decimal spellings (their f64 bit patterns are
// the hex comments, pinned by the go_math_bit_exact test); clippy would have
// them shortened.
#[allow(clippy::excessive_precision)]
fn go_log(x: f64) -> f64 {
    const LN2_HI: f64 = 6.931_471_803_691_238_164_90e-01; /* 3fe62e42 fee00000 */
    const LN2_LO: f64 = 1.908_214_929_270_587_700_02e-10; /* 3dea39ef 35793c76 */
    const L1: f64 = 6.666_666_666_666_735_130e-01; /* 3FE55555 55555593 */
    const L2: f64 = 3.999_999_999_940_941_908e-01; /* 3FD99999 9997FA04 */
    const L3: f64 = 2.857_142_874_366_239_149e-01; /* 3FD24924 94229359 */
    const L4: f64 = 2.222_219_843_214_978_396e-01; /* 3FCC71C5 1D8E78AF */
    const L5: f64 = 1.818_357_216_161_805_012e-01; /* 3FC74664 96CB03DE */
    const L6: f64 = 1.531_383_769_920_937_332e-01; /* 3FC39A09 D078C69F */
    const L7: f64 = 1.479_819_860_511_658_591e-01; /* 3FC2F112 DF3E5244 */
    const SQRT2: f64 = std::f64::consts::SQRT_2;

    // Special cases, in Go's order.
    if x.is_nan() || x == f64::INFINITY {
        return x;
    }
    if x < 0.0 {
        return f64::NAN;
    }
    if x == 0.0 {
        return f64::NEG_INFINITY;
    }

    // Reduce.
    let (mut f1, mut ki) = go_frexp(x);
    if f1 < SQRT2 / 2.0 {
        f1 *= 2.0;
        ki -= 1;
    }
    let f = f1 - 1.0;
    let k = ki as f64;

    // Compute.
    let s = f / (2.0 + f);
    let s2 = s * s;
    let s4 = s2 * s2;
    let t1 = s2 * (L1 + s4 * (L3 + s4 * (L5 + s4 * L7)));
    let t2 = s4 * (L2 + s4 * (L4 + s4 * L6));
    let r = t1 + t2;
    let hfsq = 0.5 * f * f;
    k * LN2_HI - ((hfsq - (s * (hfsq + r) + k * LN2_LO)) - f)
}

/// Go `math.Frexp` (with `normalize`), bit-exact.
fn go_frexp(f: f64) -> (f64, i32) {
    const SHIFT: u64 = 64 - 11 - 1;
    const MASK: u64 = 0x7ff;
    const BIAS: i32 = 1023;

    if f == 0.0 || f.is_infinite() || f.is_nan() {
        return (f, 0); // correctly returns -0
    }
    // Go math.normalize: scale denormals into the normal range.
    const SMALLEST_NORMAL: f64 = 2.225_073_858_507_201_4e-308; // 2**-1022
    let (f, mut exp) = if f.abs() < SMALLEST_NORMAL {
        (f * (1u64 << 52) as f64, -52i32)
    } else {
        (f, 0i32)
    };
    let mut x = f.to_bits();
    exp += ((x >> SHIFT) & MASK) as i32 - BIAS + 1;
    x &= !(MASK << SHIFT);
    x |= ((-1 + BIAS) as u64) << SHIFT;
    (f64::from_bits(x), exp)
}

/// Go `math.Round`: nearest integer, rounding half away from zero. Ported
/// from Go's bit-twiddled implementation; `f64::round` happens to share the
/// half-away-from-zero rule but this port removes any doubt.
fn go_round(x: f64) -> f64 {
    const SHIFT: u64 = 64 - 11 - 1;
    const MASK: u64 = 0x7ff;
    const BIAS: u64 = 1023;
    const SIGN_MASK: u64 = 1 << 63;
    const FRAC_MASK: u64 = (1 << SHIFT) - 1;
    const UVONE: u64 = 0x3FF0_0000_0000_0000;

    let mut bits = x.to_bits();
    let mut e = (bits >> SHIFT) & MASK;
    if e < BIAS {
        // Round abs(x) < 1 including denormals.
        bits &= SIGN_MASK; // +-0
        if e == BIAS - 1 {
            bits |= UVONE; // +-1
        }
    } else if e < BIAS + SHIFT {
        // Round any abs(x) >= 1 containing a fractional component [0,1).
        const HALF: u64 = 1 << (SHIFT - 1);
        e -= BIAS;
        bits = bits.wrapping_add(HALF >> e);
        bits &= !(FRAC_MASK >> e);
    }
    f64::from_bits(bits)
}

/// Go `math.Max` semantics (NaN and +Inf propagation, +0 over -0).
fn go_max(x: f64, y: f64) -> f64 {
    if x == f64::INFINITY || y == f64::INFINITY {
        return f64::INFINITY;
    }
    if x.is_nan() || y.is_nan() {
        return f64::NAN;
    }
    if x == 0.0 && x == y {
        return if x.is_sign_negative() { y } else { x };
    }
    if x > y { x } else { y }
}

// ---------------------------------------------------------------------------
// Parameter derivation (Go: binaryfusefilter.go).

/// Go `calculateSegmentLength`. These parameters are very sensitive:
/// replacing 'floor' by 'round' can substantially affect construction time.
/// (`f64::floor` is exact — a correctly-rounded IEEE 754 operation — on every
/// platform, so it needs no port.)
fn calculate_segment_length(arity: u32, size: u32) -> u32 {
    if size == 0 {
        return 4;
    }
    if arity == 3 {
        1u32 << ((go_log(f64::from(size)) / go_log(3.33) + 2.25).floor() as i32)
    } else if arity == 4 {
        1u32 << ((go_log(f64::from(size)) / go_log(2.91) - 0.5).floor() as i32)
    } else {
        65536
    }
}

/// Go `calculateSizeFactor`.
fn calculate_size_factor(arity: u32, size: u32) -> f64 {
    if arity == 3 {
        go_max(
            1.125,
            0.875 + 0.25 * go_log(1_000_000.0) / go_log(f64::from(size)),
        )
    } else if arity == 4 {
        go_max(
            1.075,
            0.77 + 0.305 * go_log(600_000.0) / go_log(f64::from(size)),
        )
    } else {
        2.0
    }
}

impl BinaryFuse16 {
    /// Go `initializeParameters`: derives the segment geometry for `size`
    /// keys and allocates the (zeroed) fingerprint array.
    fn initialize_parameters(size: u32) -> BinaryFuse16 {
        let arity = 3u32;
        let mut segment_length = calculate_segment_length(arity, size);
        if segment_length > 262144 {
            segment_length = 262144;
        }
        let segment_length_mask = segment_length - 1;
        let mut capacity = 0u32;
        if size > 1 {
            let size_factor = calculate_size_factor(arity, size);
            capacity = go_round(f64::from(size) * size_factor) as u32;
        }
        // Go's exact expression, kept verbatim rather than rewritten as
        // div_ceil (overflow is impossible for any realistic size; see
        // port-notes for the degenerate >3.8e9-key regime).
        #[allow(clippy::manual_div_ceil)]
        let mut total_segment_count = (capacity + segment_length - 1) / segment_length;
        if total_segment_count < arity {
            total_segment_count = arity;
        }
        let segment_count = total_segment_count - (arity - 1);
        let segment_count_length = segment_count * segment_length;
        let num_fingerprints = total_segment_count * segment_length;
        BinaryFuse16 {
            seed: 0,
            segment_length,
            segment_length_mask,
            segment_count,
            segment_count_length,
            fingerprints: vec![0u16; num_fingerprints as usize],
        }
    }

    /// Go `getHashFromHash`: the three fingerprint indexes for a hash. The
    /// segment base comes from the high word of the 64×64→128 widening
    /// multiply (Go: `bits.Mul64`).
    fn get_hash_from_hash(&self, hash: u64) -> (u32, u32, u32) {
        let hi = ((u128::from(hash) * u128::from(self.segment_count_length)) >> 64) as u64;
        let h0 = hi as u32;
        let mut h1 = h0 + self.segment_length;
        let mut h2 = h1 + self.segment_length;
        h1 ^= (hash >> 18) as u32 & self.segment_length_mask;
        h2 ^= hash as u32 & self.segment_length_mask;
        (h0, h1, h2)
    }

    /// Creates a binary fuse filter with the provided keys. For best results,
    /// the caller should avoid having too many duplicated keys.
    /// (Go: `NewBinaryFuse[uint16]`; the input is copied instead of mutated.)
    ///
    /// The construction is deterministic: the seed sequence starts from
    /// `rngcounter = 1`, so identical inputs yield identical filters.
    pub fn new(keys: &[u64]) -> Result<BinaryFuse16, Error> {
        let mut keys = keys.to_vec();
        Self::build(&mut keys)
    }

    /// Go `buildBinaryFuse`, ported verbatim (including the `iterations % 4`
    /// segment-resize dance, in-loop duplicate handling, and the error-path
    /// re-seeding).
    fn build(keys: &mut Vec<u64>) -> Result<BinaryFuse16, Error> {
        let mut size = keys.len() as u32;
        let mut filter = Self::initialize_parameters(size);
        let mut rngcounter: u64 = 1;
        filter.seed = splitmix64(&mut rngcounter);
        let capacity = filter.fingerprints.len() as u32;

        let mut alone = vec![0u32; capacity as usize];
        // The lowest 2 bits of t2count are the h index (0, 1, or 2), so only
        // 6 bits count — sufficient (wrapping, as in Go).
        let mut t2count = vec![0u8; capacity as usize];
        let mut reverse_h = vec![0u8; size as usize];
        let mut t2hash = vec![0u64; capacity as usize];
        let mut reverse_order = vec![0u64; size as usize + 1];
        reverse_order[size as usize] = 1; // sentinel for the placement scan

        let mut iterations = 0usize;
        loop {
            iterations += 1;
            if iterations > MAX_ITERATIONS {
                // The probability of this happening is lower than the
                // cosmic-ray probability.
                return Err(Error::TooManyIterations);
            }
            if size > 4 && size < 1_000_000 {
                // The segment length formula is empirical; for some sizes it
                // is too large and leads to many iterations. Once every four
                // iterations, use the previous segment length while keeping
                // the same capacity (Go: see TestBinaryFuseBoundarySizes).
                match iterations % 4 {
                    2 => {
                        // Switch to a smaller segment size.
                        filter.segment_length /= 2;
                        filter.segment_length_mask = filter.segment_length - 1;
                        filter.segment_count = filter.segment_count * 2 + 2;
                        filter.segment_count_length = filter.segment_count * filter.segment_length;
                    }
                    3 => {
                        // Restore the calculated segment size.
                        filter.segment_length *= 2;
                        filter.segment_length_mask = filter.segment_length - 1;
                        filter.segment_count = filter.segment_count / 2 - 1;
                        filter.segment_count_length = filter.segment_count * filter.segment_length;
                    }
                    _ => {}
                }
            }

            let mut block_bits = 1u32;
            while (1u32 << block_bits) < filter.segment_count {
                block_bits += 1;
            }
            let mut start_pos = vec![0u32; 1usize << block_bits];
            for (i, sp) in start_pos.iter_mut().enumerate() {
                // Important: i * size must not overflow (hence u64, as Go).
                *sp = ((i as u64 * u64::from(size)) >> block_bits) as u32;
            }
            for &key in keys.iter() {
                let hash = mixsplit(key, filter.seed);
                let mut segment_index = hash >> (64 - block_bits);
                while reverse_order[start_pos[segment_index as usize] as usize] != 0 {
                    segment_index += 1;
                    segment_index &= (1u64 << block_bits) - 1;
                }
                reverse_order[start_pos[segment_index as usize] as usize] = hash;
                start_pos[segment_index as usize] += 1;
            }

            let mut error = false;
            let mut duplicates = 0u32;
            for &hash in reverse_order.iter().take(size as usize) {
                let (index1, index2, index3) = filter.get_hash_from_hash(hash);
                let (i1, i2, i3) = (index1 as usize, index2 as usize, index3 as usize);
                t2count[i1] = t2count[i1].wrapping_add(4);
                // t2count[i1] ^= 0 // noop
                t2hash[i1] ^= hash;
                t2count[i2] = t2count[i2].wrapping_add(4);
                t2count[i2] ^= 1;
                t2hash[i2] ^= hash;
                t2count[i3] = t2count[i3].wrapping_add(4);
                t2count[i3] ^= 2;
                t2hash[i3] ^= hash;
                // If we have duplicated hash values, then it is likely that
                // the next comparison is true.
                if t2hash[i1] & t2hash[i2] & t2hash[i3] == 0 {
                    // Next we do the actual test.
                    if (t2hash[i1] == 0 && t2count[i1] == 8)
                        || (t2hash[i2] == 0 && t2count[i2] == 8)
                        || (t2hash[i3] == 0 && t2count[i3] == 8)
                    {
                        duplicates += 1;
                        t2count[i1] = t2count[i1].wrapping_sub(4);
                        t2hash[i1] ^= hash;
                        t2count[i2] = t2count[i2].wrapping_sub(4);
                        t2count[i2] ^= 1;
                        t2hash[i2] ^= hash;
                        t2count[i3] = t2count[i3].wrapping_sub(4);
                        t2count[i3] ^= 2;
                        t2hash[i3] ^= hash;
                    }
                }
                if t2count[i1] < 4 {
                    error = true;
                }
                if t2count[i2] < 4 {
                    error = true;
                }
                if t2count[i3] < 4 {
                    error = true;
                }
            }
            if error {
                reverse_order[..size as usize].fill(0);
                t2count.fill(0);
                t2hash.fill(0);
                filter.seed = splitmix64(&mut rngcounter);
                continue;
            }

            // End of key addition.

            let mut qsize = 0usize;
            // Add sets with one key to the queue.
            for i in 0..capacity {
                alone[qsize] = i;
                if (t2count[i as usize] >> 2) == 1 {
                    qsize += 1;
                }
            }
            let mut stacksize = 0u32;
            let seg_len = filter.segment_length;
            // Used to change seg_len to -2*seg_len via XOR.
            let seg_len_to_minus_seg_len_x2 = seg_len ^ (2u32.wrapping_mul(seg_len)).wrapping_neg();
            while qsize > 0 {
                qsize -= 1;
                let index = alone[qsize];
                if (t2count[index as usize] >> 2) == 1 {
                    let hash = t2hash[index as usize];
                    let found = t2count[index as usize] & 3;
                    reverse_h[stacksize as usize] = found;
                    reverse_order[stacksize as usize] = hash;
                    stacksize += 1;

                    // The other two indexes are derived from the hash with
                    // bit tricks to avoid branching (Go keeps a disabled
                    // getHashFromHash cross-check here).

                    let h01 = (hash >> 18) as u32 & filter.segment_length_mask;
                    let h02 = hash as u32 & filter.segment_length_mask;

                    // These variables are either 0 or all 1s.
                    let is0 = u32::from(found.wrapping_sub(1) >> 7).wrapping_neg(); // found==0 (relies on u8 wrap)
                    let is1 = u32::from(found & 1).wrapping_neg(); // found==1
                    let is2 = u32::from(found >> 1).wrapping_neg(); // found==2

                    // First, adjust the segment index. other_index1 is:
                    //  if found<2: index + seg_len
                    //  if found=2: index - seg_len*2
                    let mut other_index1 =
                        index.wrapping_add(seg_len ^ (seg_len_to_minus_seg_len_x2 & is2));
                    // other_index2 is:
                    //  if found>0: index - seg_len
                    //  if found=0: index + 2*seg_len
                    let mut other_index2 =
                        index.wrapping_sub(seg_len ^ (seg_len_to_minus_seg_len_x2 & is0));

                    // Now adjust the offset inside the segment. Three cases:
                    //   0: other_index1 ^= h01      other_index2 ^= h02
                    //   1: other_index1 ^= h01^h02  other_index2 ^= h01
                    //   2: other_index1 ^= h02      other_index2 ^= h01^h02
                    other_index1 ^= (h01 & !is2) ^ (h02 & !is0);
                    other_index2 ^= (h01 & !is0) ^ (h02 & !is1);

                    let f1 = (is0 & 1 | is1 & 2) as u8; // f1 = (found + 1) % 3
                    let f2 = (is0 & 2 | is2 & 1) as u8; // f2 = (found + 2) % 3

                    alone[qsize] = other_index1;
                    if (t2count[other_index1 as usize] >> 2) == 2 {
                        qsize += 1;
                    }
                    t2count[other_index1 as usize] = t2count[other_index1 as usize].wrapping_sub(4);
                    t2count[other_index1 as usize] ^= f1;
                    t2hash[other_index1 as usize] ^= hash;

                    alone[qsize] = other_index2;
                    if (t2count[other_index2 as usize] >> 2) == 2 {
                        qsize += 1;
                    }
                    t2count[other_index2 as usize] = t2count[other_index2 as usize].wrapping_sub(4);
                    t2count[other_index2 as usize] ^= f2;
                    t2hash[other_index2 as usize] ^= hash;
                }
            }

            if stacksize + duplicates == size {
                // Success.
                size = stacksize;
                break;
            } else if duplicates > 0 {
                // Duplicates were found, but we did not manage to remove them
                // all. Sorting the keys solves the issue (Go mutates the
                // caller's slice here; this port owns its copy).
                prune_duplicates(keys);
            }
            reverse_order[..size as usize].fill(0);
            t2count.fill(0);
            t2hash.fill(0);
            filter.seed = splitmix64(&mut rngcounter);
        }
        if size == 0 {
            return Ok(filter);
        }

        let mut h012 = [0u32; 5];
        for i in (0..size as usize).rev() {
            // The hash of the key we insert next.
            let hash = reverse_order[i];
            let xor2 = fingerprint(hash) as u16;
            let (index1, index2, index3) = filter.get_hash_from_hash(hash);
            let found = reverse_h[i] as usize;
            h012[0] = index1;
            h012[1] = index2;
            h012[2] = index3;
            h012[3] = h012[0];
            h012[4] = h012[1];
            filter.fingerprints[h012[found] as usize] = xor2
                ^ filter.fingerprints[h012[found + 1] as usize]
                ^ filter.fingerprints[h012[found + 2] as usize];
        }

        Ok(filter)
    }

    /// Returns `true` if `key` is part of the set, with a small false
    /// positive probability (Go: `Contains`).
    pub fn contains(&self, key: u64) -> bool {
        let hash = mixsplit(key, self.seed);
        let mut f = fingerprint(hash) as u16;
        let (h0, h1, h2) = self.get_hash_from_hash(hash);
        f ^= self.fingerprints[h0 as usize]
            ^ self.fingerprints[h1 as usize]
            ^ self.fingerprints[h2 as usize];
        f == 0
    }

    /// Serializes this filter as a packstore filter section
    /// (Go: `packstore.buildFilterSection`, serialization part): the type
    /// byte [`SECTION_TYPE_BINARY_FUSE16`], big-endian seed and geometry,
    /// the fingerprint count, then big-endian `u16` fingerprints.
    pub fn section_bytes(&self) -> Vec<u8> {
        let mut out = vec![0u8; SECTION_HEADER_SIZE + 2 * self.fingerprints.len()];
        out[0] = SECTION_TYPE_BINARY_FUSE16;
        out[1..9].copy_from_slice(&self.seed.to_be_bytes());
        out[9..13].copy_from_slice(&self.segment_length.to_be_bytes());
        out[13..17].copy_from_slice(&self.segment_length_mask.to_be_bytes());
        out[17..21].copy_from_slice(&self.segment_count.to_be_bytes());
        out[21..25].copy_from_slice(&self.segment_count_length.to_be_bytes());
        out[25..29].copy_from_slice(&(self.fingerprints.len() as u32).to_be_bytes());
        for (chunk, fp) in out[SECTION_HEADER_SIZE..]
            .chunks_exact_mut(2)
            .zip(&self.fingerprints)
        {
            chunk.copy_from_slice(&fp.to_be_bytes());
        }
        out
    }

    /// Deserializes a filter section, copying the fingerprints out of `b`
    /// (which may be a read-only mmap) into RAM
    /// (Go: `packstore.parseFilterSection`).
    ///
    /// The five geometry fields are validated against the binary-fuse
    /// construction invariants: [`BinaryFuse16::contains`] indexes
    /// `fingerprints` from them, so crafted values would otherwise panic the
    /// read path rather than fail parse with a corrupt-data error.
    ///
    /// One check is stricter than Go's `parseFilterSection`:
    /// `segment_count == 0` is rejected (Go accepts it, and its `Contains`
    /// then panics with an index out of range — the very outcome the
    /// validation exists to prevent). Real filters always have
    /// `segment_count >= 1`, so no Go-written section is affected.
    pub fn parse_section(b: &[u8]) -> Result<BinaryFuse16, Error> {
        if b.len() < SECTION_HEADER_SIZE {
            return Err(Error::Corrupt(format!(
                "filter section too short: {} bytes",
                b.len()
            )));
        }
        if b[0] != SECTION_TYPE_BINARY_FUSE16 {
            return Err(Error::Corrupt(format!("unknown filter type {}", b[0])));
        }
        let fp_count = read_u32_be(b, 25);
        let want = SECTION_HEADER_SIZE as u64 + 2 * u64::from(fp_count);
        if b.len() as u64 != want {
            return Err(Error::Corrupt(format!(
                "filter section is {} bytes, want {}",
                b.len(),
                want
            )));
        }
        let seg_len = read_u32_be(b, 9);
        let seg_len_mask = read_u32_be(b, 13);
        let seg_count = read_u32_be(b, 17);
        let seg_count_len = read_u32_be(b, 21);
        if seg_len == 0
            || seg_len & (seg_len - 1) != 0
            || seg_len_mask != seg_len - 1
            || seg_count == 0
            || u64::from(seg_count_len) != u64::from(seg_count) * u64::from(seg_len)
            || u64::from(fp_count) != u64::from(seg_count_len) + 2 * u64::from(seg_len)
        {
            return Err(Error::Corrupt("filter geometry invalid".to_string()));
        }
        Ok(BinaryFuse16 {
            seed: u64::from_be_bytes([b[1], b[2], b[3], b[4], b[5], b[6], b[7], b[8]]),
            segment_length: seg_len,
            segment_length_mask: seg_len_mask,
            segment_count: seg_count,
            segment_count_length: seg_count_len,
            fingerprints: b[SECTION_HEADER_SIZE..]
                .chunks_exact(2)
                .map(|c| u16::from_be_bytes([c[0], c[1]]))
                .collect(),
        })
    }
}

fn read_u32_be(b: &[u8], off: usize) -> u32 {
    u32::from_be_bytes([b[off], b[off + 1], b[off + 2], b[off + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `u64s(seed, n)` from VECTORS.md (also Go's splitmix64 stream).
    fn u64s(seed: u64, n: usize) -> Vec<u64> {
        let mut s = seed;
        (0..n).map(|_| splitmix64(&mut s)).collect()
    }

    fn sorted_dedup(mut v: Vec<u64>) -> Vec<u64> {
        v.sort_unstable();
        v.dedup();
        v
    }

    /// The first seed every deterministic construction starts from
    /// (splitmix64 output #1 for rngcounter = 1).
    const FIRST_SEED: u64 = 0x910a_2dec_8902_5cc1;

    #[test]
    fn first_seed_pinned() {
        let mut rngcounter = 1u64;
        assert_eq!(splitmix64(&mut rngcounter), FIRST_SEED);
    }

    /// The ported Go math must be bit-exact: expected bit patterns generated
    /// with Go `math.Log` (portable FDLIBM path, as used on the vectorgen
    /// platform) and `math.Round` from the pinned toolchain.
    #[test]
    fn go_math_bit_exact() {
        const LOG_CASES: &[(f64, u64)] = &[
            (2.0, 0x3fe62e42fefa39ef),
            (3.33, 0x3ff33f5fe2fa413a),
            (2.91, 0x3ff11727af6cde95),
            (10.0, 0x40026bb1bbb55516),
            (600_000.0, 0x402a9bffa9e7ee5b),
            (1_000_000.0, 0x402ba18a998fffa0),
            (123_456.0, 0x40277280f4671181),
            (4_583_149.0, 0x402ead00d1b217db),
            (4_294_967_295.0, 0x40362e42fef939ef),
        ];
        for &(x, bits) in LOG_CASES {
            assert_eq!(go_log(x).to_bits(), bits, "log({x})");
        }
        // math.Round: half away from zero.
        const ROUND_CASES: &[(f64, f64)] = &[
            (0.5, 1.0),
            (-0.5, -1.0),
            (2.5, 3.0),
            (-2.5, -3.0),
            (1.5, 2.0),
            (2.499_999_999_999_999_6, 2.0),
            (8.5, 9.0),
            (112.5, 113.0),
            (138_887.999_999_999_97, 138_888.0),
        ];
        for &(x, want) in ROUND_CASES {
            assert_eq!(go_round(x), want, "round({x})");
        }
    }

    /// Go TestBinaryFuseParams: the (size range → segment length / segment
    /// count) table produced by initializeParameters. This pins the ported
    /// go_log/go_round/floor geometry derivation across the whole domain.
    #[test]
    fn params_table_matches_go() {
        // (segment_length, start_size, start_segment_count, end_size, end_segment_count)
        const TABLE: &[(u32, u32, u32, u32, u32)] = &[
            (4, 1, 1, 2, 1),
            (8, 3, 1, 8, 1),
            (16, 9, 1, 27, 2),
            (32, 28, 1, 91, 3),
            (64, 92, 1, 303, 5),
            (128, 304, 2, 1009, 9),
            (256, 1010, 4, 3361, 16),
            (512, 3362, 7, 11192, 26),
            (1024, 11193, 12, 37272, 42),
            (2048, 37273, 20, 124117, 69),
            (4096, 124118, 34, 413309, 114),
            (8192, 413310, 56, 1376321, 188),
            (16384, 1376322, 93, 4583149, 313),
        ];
        for &(seg_len, start, start_cnt, end, end_cnt) in TABLE {
            let f = BinaryFuse16::initialize_parameters(start);
            assert_eq!(
                (f.segment_length, f.segment_count),
                (seg_len, start_cnt),
                "size {start}"
            );
            let f = BinaryFuse16::initialize_parameters(end);
            assert_eq!(
                (f.segment_length, f.segment_count),
                (seg_len, end_cnt),
                "size {end}"
            );
            // The next size starts a new segment length.
            let f = BinaryFuse16::initialize_parameters(end + 1);
            assert_ne!(f.segment_length, seg_len, "size {}", end + 1);
        }
    }

    /// Known-answer cross-check against Go FastFilter/xorfilter v0.5.1
    /// (values captured from the pinned Go module; see port-notes). Each case
    /// checks the final seed (which encodes how many construction iterations
    /// ran), the derived geometry, and a CRC-32C over the serialized section
    /// (covering every fingerprint bit).
    #[test]
    fn go_reference_sections() {
        struct Case {
            label: &'static str,
            keys: Vec<u64>,
            seed: u64,
            seg_len: u32,
            seg_cnt: u32,
            scl: u32,
            fp: usize,
            crc: u32,
        }
        let c = |label, keys, seed, seg_len, seg_cnt, scl, fp, crc| Case {
            label,
            keys,
            seed,
            seg_len,
            seg_cnt,
            scl,
            fp,
            crc,
        };
        // Seed 0xbeeb8da1658eec67 is splitmix64 output #2, 0xf893a2eefb32555e
        // is output #3: those cases retried, exercising the iterations%4
        // segment-resize dance (halved seg_len kept on iteration 2; halved
        // then restored on iteration 3).
        let cases = vec![
            c("n=0", vec![], FIRST_SEED, 4, 1, 4, 12, 0x6691c0b8),
            c(
                "n=1",
                sorted_dedup(u64s(42, 1)),
                FIRST_SEED,
                4,
                1,
                4,
                12,
                0x5aab39b8,
            ),
            c(
                "n=2",
                sorted_dedup(u64s(42, 2)),
                FIRST_SEED,
                4,
                1,
                4,
                12,
                0x52b30d3c,
            ),
            c(
                "n=3",
                sorted_dedup(u64s(42, 3)),
                FIRST_SEED,
                8,
                1,
                8,
                24,
                0xdf0c128c,
            ),
            c(
                "n=4",
                sorted_dedup(u64s(42, 4)),
                FIRST_SEED,
                8,
                1,
                8,
                24,
                0x1a87bcb1,
            ),
            c(
                "n=5",
                sorted_dedup(u64s(42, 5)),
                FIRST_SEED,
                8,
                1,
                8,
                24,
                0xf2c1828c,
            ),
            c(
                "n=8",
                sorted_dedup(u64s(42, 8)),
                FIRST_SEED,
                8,
                1,
                8,
                24,
                0xe7cc4319,
            ),
            c(
                "n=9",
                sorted_dedup(u64s(42, 9)),
                FIRST_SEED,
                16,
                1,
                16,
                48,
                0xc4cbb2fc,
            ),
            c(
                "n=10",
                sorted_dedup(u64s(42, 10)),
                FIRST_SEED,
                16,
                1,
                16,
                48,
                0x6f492d7a,
            ),
            c(
                "n=27",
                sorted_dedup(u64s(42, 27)),
                FIRST_SEED,
                16,
                2,
                32,
                64,
                0x7b40e4a2,
            ),
            c(
                "n=28",
                sorted_dedup(u64s(42, 28)),
                FIRST_SEED,
                32,
                1,
                32,
                96,
                0xde4689d6,
            ),
            c(
                "n=91",
                sorted_dedup(u64s(42, 91)),
                FIRST_SEED,
                32,
                3,
                96,
                160,
                0x07b40fc1,
            ),
            c(
                "n=92",
                sorted_dedup(u64s(42, 92)),
                FIRST_SEED,
                64,
                1,
                64,
                192,
                0x888d8be7,
            ),
            c(
                "n=100",
                sorted_dedup(u64s(42, 100)),
                FIRST_SEED,
                64,
                1,
                64,
                192,
                0x0cf7d0dd,
            ),
            c(
                "n=303",
                sorted_dedup(u64s(42, 303)),
                FIRST_SEED,
                64,
                5,
                320,
                448,
                0x804eab0a,
            ),
            c(
                "n=304",
                sorted_dedup(u64s(42, 304)),
                FIRST_SEED,
                128,
                2,
                256,
                512,
                0xd643184b,
            ),
            c(
                "n=1000",
                sorted_dedup(u64s(42, 1000)),
                FIRST_SEED,
                128,
                9,
                1152,
                1408,
                0xdcae2355,
            ),
            c(
                "n=1009",
                sorted_dedup(u64s(42, 1009)),
                FIRST_SEED,
                128,
                9,
                1152,
                1408,
                0x8e410a7c,
            ),
            c(
                "n=1010",
                sorted_dedup(u64s(42, 1010)),
                FIRST_SEED,
                256,
                4,
                1024,
                1536,
                0x418a074b,
            ),
            c(
                "n=3361",
                sorted_dedup(u64s(42, 3361)),
                FIRST_SEED,
                256,
                16,
                4096,
                4608,
                0x9cfa8c8a,
            ),
            c(
                "n=3362",
                sorted_dedup(u64s(42, 3362)),
                FIRST_SEED,
                512,
                7,
                3584,
                4608,
                0x24ea72f7,
            ),
            c(
                "n=10000",
                sorted_dedup(u64s(42, 10000)),
                FIRST_SEED,
                512,
                23,
                11776,
                12800,
                0x1e639c89,
            ),
            c(
                "n=11192",
                sorted_dedup(u64s(42, 11192)),
                FIRST_SEED,
                512,
                26,
                13312,
                14336,
                0x6c4fb8a7,
            ),
            c(
                "n=11193",
                sorted_dedup(u64s(42, 11193)),
                FIRST_SEED,
                1024,
                12,
                12288,
                14336,
                0xf54e9cea,
            ),
            c(
                "n=123456",
                sorted_dedup(u64s(42, 123456)),
                FIRST_SEED,
                2048,
                69,
                141312,
                145408,
                0x0818aec0,
            ),
            // Duplicate handling: the same 50 keys twice, unsorted.
            c(
                "dup50x2",
                {
                    let mut k = u64s(7, 50);
                    k.extend(u64s(7, 50));
                    k
                },
                0xbeeb_8da1_658e_ec67,
                32,
                4,
                128,
                192,
                0x0eee8bb1,
            ),
            // All keys identical.
            c(
                "same10",
                vec![0xDEAD_BEEF; 10],
                FIRST_SEED,
                16,
                1,
                16,
                48,
                0xa535c5fe,
            ),
            // iterations%4 == 2: succeeded on iteration 2 with seg_len halved
            // (512 → 256) and seg_cnt doubled+2.
            c(
                "iter2-n=11192",
                sorted_dedup(u64s(4, 11192)),
                0xbeeb_8da1_658e_ec67,
                256,
                54,
                13824,
                14336,
                0xdfda3f17,
            ),
            c(
                "iter2-n=1009",
                sorted_dedup(u64s(25, 1009)),
                0xbeeb_8da1_658e_ec67,
                64,
                20,
                1280,
                1408,
                0x59a9225c,
            ),
            // iterations%4 == 3: seg_len halved then restored (256 → 128 → 256).
            c(
                "iter3-n=3361",
                sorted_dedup(u64s(53, 3361)),
                0xf893_a2ee_fb32_555e,
                256,
                16,
                4096,
                4608,
                0x39a57cb0,
            ),
            c(
                "iter3b-n=3361",
                sorted_dedup(u64s(461, 3361)),
                0xf893_a2ee_fb32_555e,
                256,
                16,
                4096,
                4608,
                0x3da3f97b,
            ),
        ];
        for case in cases {
            let f = BinaryFuse16::new(&case.keys)
                .unwrap_or_else(|e| panic!("{}: build: {e}", case.label));
            assert_eq!(f.seed, case.seed, "{}: seed", case.label);
            assert_eq!(
                f.segment_length, case.seg_len,
                "{}: segment_length",
                case.label
            );
            assert_eq!(
                f.segment_length_mask,
                case.seg_len - 1,
                "{}: segment_length_mask",
                case.label
            );
            assert_eq!(
                f.segment_count, case.seg_cnt,
                "{}: segment_count",
                case.label
            );
            assert_eq!(
                f.segment_count_length, case.scl,
                "{}: segment_count_length",
                case.label
            );
            assert_eq!(
                f.fingerprints.len(),
                case.fp,
                "{}: fingerprint count",
                case.label
            );
            assert_eq!(
                crc32c::crc32c(&f.section_bytes()),
                case.crc,
                "{}: section CRC-32C",
                case.label
            );
            for &k in &case.keys {
                assert!(f.contains(k), "{}: must contain {k:#x}", case.label);
            }
        }
    }

    /// Every inserted key must be found, across small and awkward sizes
    /// (0, 1, 2, 3 keys and each side of the geometry breakpoints).
    #[test]
    fn contains_all_across_sizes() {
        let mut sizes: Vec<usize> = (0..=32).collect();
        sizes.extend([91, 92, 100, 303, 304, 1000, 1009, 1010, 3361, 3362]);
        for n in sizes {
            let keys = sorted_dedup(u64s(1000 + n as u64, n));
            let f = BinaryFuse16::new(&keys).unwrap_or_else(|e| panic!("n={n}: {e}"));
            for &k in &keys {
                assert!(f.contains(k), "n={n}: missing key {k:#x}");
            }
        }
    }

    /// Go TestBinaryFuseN_ZeroSet: the empty set is not an error.
    #[test]
    fn zero_set() {
        let f = BinaryFuse16::new(&[]).unwrap();
        assert_eq!(f.fingerprints, vec![0u16; 12]);
        assert_eq!(
            (f.segment_length, f.segment_count, f.segment_count_length),
            (4, 1, 4)
        );
    }

    /// Go TestBinaryFuseN_DuplicateKeysBinaryFuseDup.
    #[test]
    fn duplicate_keys() {
        let keys = [303u64, 1, 77, 31, 241, 303];
        let f = BinaryFuse16::new(&keys).unwrap();
        for &k in &keys {
            assert!(f.contains(k));
        }
    }

    /// Go TestBinaryFuseN_DuplicateKeysBinaryFuseDup_Issue30.
    #[test]
    fn duplicate_keys_issue30() {
        let keys: Vec<u64> = vec![
            14032282262966018013,
            14032282273189634013,
            14434670549455045197,
            14434670549455045197,
            14434715112030278733,
            14434715112030278733,
            1463031668069456414,
            1463031668069456414,
            15078258904550751789,
            15081947205023144749,
            15087793929176324909,
            15087793929514872877,
            15428597303557855302,
            15431797104190473360,
            15454853113467544134,
            1577077805634642122,
            15777410361767557472,
            15907998856512513094,
            15919978655645680696,
            1592170445630803483,
            15933058486048027407,
            15933070362921612719,
            15949859010628284683,
            15950094057516674097,
            15950094057516674097,
            15950492113755294966,
            15999960652771912055,
            16104958339467613609,
            16115083045828466089,
            16115119760717288873,
            16126347135921205846,
            16180939948277777353,
            16205881181578942897,
            16207480993107654476,
            1627916223119626716,
            16303139460042870203,
            16303139460042870203,
            1630429337332308348,
            16309304071237318790,
            16314547479302655419,
            16314547479302655419,
            16369820198817029405,
            16448390727851746333,
            16465049428524180509,
            16465073162513458205,
            16465073285148156957,
            16465073285149870384,
            16465073285149877277,
            16465073285893104669,
            16555387163297522125,
            16592146351271542115,
            16682791020048538670,
            16683514177871458902,
            16699277535828137630,
            16716099852308345174,
            16716099868253794902,
            16856736053711445064,
            16856736054253850696,
            16856736060613333064,
            16877690937235789198,
            16963977918744734769,
            16976350133984177557,
            16976376109946388059,
            17041493382094423395,
            17053822556128759139,
            17067586192959011138,
            17088637646961899303,
            17121323146925062160,
            17130440365429237769,
            17130440365429237769,
            17130440597658279433,
            17130440597658279433,
            17181620514756131957,
            17193256430982721885,
            17193256636319002973,
            17264031033993538756,
            17321155670529409646,
            17514402547088160271,
            17514402547088160271,
            1823133498679825084,
            1823180415377412796,
            18278489907932484471,
            1831024066115736252,
            18341786752172751552,
            18378944050902766168,
            18378944052194427480,
            18403514326223737719,
            18405070344654600695,
            2164472587301781504,
            2164472587301781504,
            2290190445057074187,
            2471837983693302824,
            2471837983693302824,
            3138094539259513280,
            3138094539259513280,
            3138153989894179264,
            3138153989894179264,
            3566850904877432832,
            3566850904877432832,
            3868495676835528327,
            3868495676835528327,
            3981182070595518464,
            3981182070595518464,
            3998521163612422144,
            3998521163612422144,
            3998521164578160640,
            3998521164578160640,
            3998521164581306368,
            3998521164581306368,
            3998521164581329296,
            3998521164581329296,
            4334725363086930304,
            4334725363086930304,
            4337388653622853632,
            4337388653622853632,
            4587006656968527746,
            4587006656968527746,
            4587006831041087252,
            4587006831041087252,
            4825061103098367168,
            4825061103098367168,
        ];
        let f = BinaryFuse16::new(&keys).unwrap();
        for &k in &keys {
            assert!(f.contains(k));
        }
    }

    /// Construction is a pure function of the input.
    #[test]
    fn deterministic_rebuild() {
        let keys = sorted_dedup(u64s(99, 5000));
        let a = BinaryFuse16::new(&keys).unwrap();
        let b = BinaryFuse16::new(&keys).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.section_bytes(), b.section_bytes());
    }

    /// False-positive sanity: ~2^-16 expected for 16-bit fingerprints.
    #[test]
    fn false_positive_rate() {
        let keys = sorted_dedup(u64s(42, 10000));
        let f = BinaryFuse16::new(&keys).unwrap();
        let probes = u64s(0xFACE, 100_000);
        let mut matches = 0usize;
        for p in probes {
            if keys.binary_search(&p).is_err() && f.contains(p) {
                matches += 1;
            }
        }
        // Expected ≈ 1.5 matches; 50 would mean the filter is broken.
        assert!(
            matches < 50,
            "false positive rate too high: {matches}/100000"
        );
    }

    #[test]
    fn section_round_trip() {
        for n in [0usize, 1, 2, 3, 100, 1000] {
            let keys = sorted_dedup(u64s(7 + n as u64, n));
            let f = BinaryFuse16::new(&keys).unwrap();
            let sec = f.section_bytes();
            assert_eq!(sec.len(), SECTION_HEADER_SIZE + 2 * f.fingerprints.len());
            assert_eq!(sec[0], SECTION_TYPE_BINARY_FUSE16);
            let g = BinaryFuse16::parse_section(&sec).unwrap();
            assert_eq!(f, g, "n={n}");
            for &k in &keys {
                assert!(g.contains(k), "n={n}: parsed filter missing {k:#x}");
            }
        }
    }

    /// parse_section validation, in Go parseFilterSection's order.
    #[test]
    fn parse_section_errors() {
        let f = BinaryFuse16::new(&sorted_dedup(u64s(11, 100))).unwrap();
        let sec = f.section_bytes();

        // Too short (checked before anything else).
        let err = BinaryFuse16::parse_section(&sec[..SECTION_HEADER_SIZE - 1]).unwrap_err();
        assert!(err.is_corrupt());
        assert_eq!(err.to_string(), "filter section too short: 28 bytes");

        // Unknown type byte.
        let mut bad = sec.clone();
        bad[0] = 2;
        let err = BinaryFuse16::parse_section(&bad).unwrap_err();
        assert!(err.is_corrupt());
        assert_eq!(err.to_string(), "unknown filter type 2");

        // Length inconsistent with the fingerprint count.
        let err = BinaryFuse16::parse_section(&sec[..sec.len() - 2]).unwrap_err();
        assert!(err.is_corrupt());
        assert!(err.to_string().starts_with("filter section is "), "{err}");

        // Geometry: segment_length zero.
        let mut bad = sec.clone();
        bad[9..13].copy_from_slice(&0u32.to_be_bytes());
        assert_geometry_invalid(&bad);

        // Geometry: segment_length not a power of two.
        let mut bad = sec.clone();
        bad[9..13].copy_from_slice(&48u32.to_be_bytes());
        assert_geometry_invalid(&bad);

        // Geometry: wrong mask.
        let mut bad = sec.clone();
        bad[13..17].copy_from_slice(&(f.segment_length).to_be_bytes());
        assert_geometry_invalid(&bad);

        // Geometry: segment_count_length != segment_count * segment_length.
        let mut bad = sec.clone();
        bad[21..25].copy_from_slice(&(f.segment_count_length + 1).to_be_bytes());
        assert_geometry_invalid(&bad);

        // Geometry: fingerprint count != segment_count_length + 2*segment_length.
        let mut bad = sec.clone();
        bad[17..21].copy_from_slice(&(f.segment_count + 1).to_be_bytes());
        bad[21..25].copy_from_slice(&((f.segment_count + 1) * f.segment_length).to_be_bytes());
        assert_geometry_invalid(&bad);

        // The pristine section still parses.
        assert!(BinaryFuse16::parse_section(&sec).is_ok());
    }

    fn assert_geometry_invalid(b: &[u8]) {
        let err = BinaryFuse16::parse_section(b).unwrap_err();
        assert!(err.is_corrupt());
        assert_eq!(err.to_string(), "filter geometry invalid");
    }

    /// A crafted section with `segment_count == 0` satisfies every geometry
    /// equation Go checks (0 * seg_len == 0, fp_count == 0 + 2 * seg_len),
    /// but `contains` would then index `fingerprints[2 * seg_len ..]` out of
    /// bounds — a panic on untrusted input. Go's `parseFilterSection` shares
    /// the hole (its `Contains` panics identically); this port closes it, the
    /// one intentional strictness divergence (see port-notes): real filters
    /// always have `segment_count >= 1`, so no Go-written section is rejected.
    #[test]
    fn parse_section_rejects_zero_segment_count() {
        let mut sec = vec![0u8; SECTION_HEADER_SIZE + 2 * 8];
        sec[0] = SECTION_TYPE_BINARY_FUSE16;
        // seed: zero (bytes 1..9)
        sec[9..13].copy_from_slice(&4u32.to_be_bytes()); // segment_length
        sec[13..17].copy_from_slice(&3u32.to_be_bytes()); // segment_length_mask
        sec[17..21].copy_from_slice(&0u32.to_be_bytes()); // segment_count
        sec[21..25].copy_from_slice(&0u32.to_be_bytes()); // segment_count_length
        sec[25..29].copy_from_slice(&8u32.to_be_bytes()); // fingerprint count
        assert_geometry_invalid(&sec);
    }

    #[test]
    fn error_messages() {
        assert_eq!(Error::TooManyIterations.to_string(), "too many iterations");
        assert!(!Error::TooManyIterations.is_corrupt());
    }
}
