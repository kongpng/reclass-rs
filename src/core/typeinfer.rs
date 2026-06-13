//! Heuristic type-inference engine — scores byte patterns into candidate
//! `NodeKind`s for the editor's "type hint chips".
//!
//! Faithful 1:1 port of `src/typeinfer.h` (550 lines, header-only). The public
//! surface (`InferHints`, `TypeSuggestion`, `infer_types`, `format_hint`) and
//! the whole-width / uniform-split candidate generation, the per-kind feature
//! checkers, and `prune_and_rank` all mirror the C++ scoring exactly.
//!
//! Endianness: the C++ feature checkers read multi-byte values with `memcpy`,
//! i.e. native byte order. The oracle and tests run on little-endian x86_64, so
//! the loads here use `from_le_bytes` to match (`detail::loadU16/U32/U64/F32/F64`).

use super::kind::{kind_meta, NodeKind};
use smallvec::{smallvec, SmallVec};

/// `struct InferHints` (`typeinfer.h:13-20`).
#[derive(Clone, Copy, Debug)]
pub struct InferHints<'a> {
    /// raw bytes, same len as `data`.
    pub min_observed: Option<&'a [u8]>,
    pub max_observed: Option<&'a [u8]>,
    /// value only increases or only decreases.
    pub monotonic: bool,
    /// identical across all samples.
    pub never_changed: bool,
    /// 0 = no history.
    pub sample_count: i32,
    pub ptr_size: i32,
}

impl Default for InferHints<'_> {
    fn default() -> Self {
        // C++ defaults: ptrSize = 8 (typeinfer.h:19), everything else
        // false/null/0.
        InferHints {
            min_observed: None,
            max_observed: None,
            monotonic: false,
            never_changed: false,
            sample_count: 0,
            ptr_size: 8,
        }
    }
}

/// `struct TypeSuggestion` (`typeinfer.h:24-28`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeSuggestion {
    /// `len==1`: convert; `len>1`: uniform split.
    pub kinds: SmallVec<[NodeKind; 16]>,
    /// 0-100 feature ratio (passed / checked × 100).
    pub score: i32,
    /// 0=hidden, 1=weak, 2=moderate, 3=strong.
    pub strength: i32,
}

/// `formatHint` (`typeinfer.h:38-44`) — short type label (e.g. "ptr64",
/// "int32_t×2").
pub fn format_hint(s: &TypeSuggestion) -> String {
    let Some(first) = s.kinds.first() else {
        return String::new();
    };
    let name = kind_meta(*first).map_or("", |m| m.type_name);
    if s.kinds.len() == 1 {
        name.to_string()
    } else {
        format!("{name}\u{00D7}{}", s.kinds.len())
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// detail:: — header-only implementation (`typeinfer.h:48-518`).
// ─────────────────────────────────────────────────────────────────────────────

#[inline]
fn load_u32(p: &[u8]) -> u32 {
    u32::from_le_bytes([p[0], p[1], p[2], p[3]])
}
#[inline]
fn load_u64(p: &[u8]) -> u64 {
    u64::from_le_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]])
}
#[inline]
fn load_u16(p: &[u8]) -> u16 {
    u16::from_le_bytes([p[0], p[1]])
}
// `loadF32` (`typeinfer.h:59-61`) — part of the faithful `detail::` helper set.
// Like the C++, the float feature checkers operate on a pre-loaded `u32`, so
// this loader has no live caller; kept for parity.
#[inline]
#[allow(dead_code)]
fn load_f32(p: &[u8]) -> f32 {
    f32::from_bits(load_u32(p))
}
#[inline]
fn load_f64(p: &[u8]) -> f64 {
    f64::from_bits(load_u64(p))
}

#[inline]
fn all_zero(p: &[u8]) -> bool {
    p.iter().all(|&b| b == 0)
}

#[inline]
fn popcount32(v: u32) -> i32 {
    v.count_ones() as i32
}

#[inline]
fn is_printable(c: u8) -> bool {
    (0x20..=0x7E).contains(&c)
}

/// `struct FeatureResult` (`typeinfer.h:86`).
#[derive(Clone, Copy)]
struct FeatureResult {
    passed: i32,
    checked: i32,
}

/// `isGoodFloat` (`typeinfer.h:88-95`).
fn is_good_float(bits: u32) -> bool {
    let exp = (bits >> 23) & 0xFF;
    if exp == 0xFF {
        return false; // inf/nan
    }
    if exp == 0 && (bits & 0x7FFFFF) != 0 {
        return false; // denormal
    }
    let f = f32::from_bits(bits);
    let af = (f as f64).abs();
    f == 0.0f32 || (1e-6..=1e7).contains(&af)
}

/// `countFloatFeatures` (`typeinfer.h:97-131`).
fn count_float_features(
    cur: u32,
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
) -> FeatureResult {
    let mut passed = 0;
    let mut checked = 4;
    let f = f32::from_bits(cur);

    // Feature 1: finite
    passed += i32::from((f as f64).is_finite());
    // Feature 2: non-denormal (exponent > 0 or value is ±0)
    let exp = (cur >> 23) & 0xFF;
    passed += i32::from(exp > 0 || (cur & 0x7FFFFFFF) == 0);
    // Feature 3: reasonable range
    let af = (f as f64).abs();
    passed += i32::from(f == 0.0f32 || (1e-6..=1e7).contains(&af));
    // Feature 4: has fractional part (not just a reinterpreted integer)
    let frac = (f as f64).fract().abs();
    passed += i32::from(frac > 0.0001);

    if h.sample_count > 0 {
        if let (Some(min_p), Some(max_p)) = (min_p, max_p) {
            checked += 4;
            let min_bits = load_u32(min_p);
            let max_bits = load_u32(max_p);
            // Feature 5-6: min/max are also valid floats
            passed += i32::from(is_good_float(min_bits));
            passed += i32::from(is_good_float(max_bits));
            // Feature 7: field changes
            passed += i32::from(min_bits != max_bits);
            // Feature 8: range is game-plausible
            let fmin = f32::from_bits(min_bits) as f64;
            let fmax = f32::from_bits(max_bits) as f64;
            let range = (fmax - fmin).abs();
            passed += i32::from(range < 1e6);
        }
    }
    FeatureResult { passed, checked }
}

/// `countIntFeatures` (`typeinfer.h:135-163`).
fn count_int_features(
    val: u32,
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
) -> FeatureResult {
    // Hard reject: zero and sentinel are never useful integers
    if val == 0 || val == 0xFFFFFFFF {
        return FeatureResult {
            passed: 0,
            checked: 3,
        };
    }

    let mut passed = 0;
    let mut checked = 3;
    let sv = val as i32;

    // Feature 1: non-zero and not sentinel (always passes after hard reject)
    passed += 1;
    // Feature 2: small absolute value
    passed += i32::from(val <= 1_000_000u32 || (sv.wrapping_add(1_000_000) as u32) <= 2_000_000u32);
    // Feature 3: fits int16 range
    passed += i32::from((-32768..=32767).contains(&sv));

    if h.sample_count > 0 {
        if let (Some(min_p), Some(max_p)) = (min_p, max_p) {
            checked += 3;
            let min_v = load_u32(min_p);
            let max_v = load_u32(max_p);
            // Feature 4: min/max in reasonable range
            passed += i32::from(min_v <= 1_000_000u32 && max_v <= 1_000_000u32);
            // Feature 5: monotonic (counter/timer)
            passed += i32::from(h.monotonic);
            // Feature 6: field varies
            passed += i32::from(min_v != max_v);
        }
    }
    FeatureResult { passed, checked }
}

/// `countFlagFeatures` (`typeinfer.h:167-189`).
fn count_flag_features(
    val: u32,
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
) -> FeatureResult {
    let mut passed = 0;
    let mut checked = 2;
    let pc = popcount32(val);

    // Feature 1: sparse bits (1-3 set)
    passed += i32::from((1..=3).contains(&pc));
    // Feature 2: not a small sequential integer (flags are usually not 1,2,3...)
    passed += i32::from(val > 256 || (val & val.wrapping_sub(1)) != 0);

    if h.sample_count > 0 {
        if let (Some(min_p), Some(max_p)) = (min_p, max_p) {
            checked += 3;
            let min_v = load_u32(min_p);
            let max_v = load_u32(max_p);
            // Feature 3: XOR of min/max has low popcount (specific bits toggle)
            passed += i32::from(popcount32(min_v ^ max_v) <= 4);
            // Feature 4: field varies
            passed += i32::from(min_v != max_v);
            // Feature 5: max is superset of min bits
            passed += i32::from((min_v & max_v) == min_v);
        }
    }
    FeatureResult { passed, checked }
}

/// `countPtrFeatures64` (`typeinfer.h:193-236`).
fn count_ptr_features64(val: u64) -> FeatureResult {
    // Hard reject: common sentinel values
    if val == 0 || val == 0xFFFFFFFFFFFFFFFF || val == 0x00000000FFFFFFFF {
        return FeatureResult {
            passed: 0,
            checked: 5,
        };
    }

    // Hard reject: non-canonical address — impossible to dereference on x64.
    if val > 0x00007FFFFFFFFFFF && val < 0xFFFF800000000000 {
        return FeatureResult {
            passed: 0,
            checked: 5,
        };
    }

    let mut passed = 0;
    let checked = 5;
    // Feature 1: aligned to 8 (heap/vtable allocations)
    passed += i32::from((val & 7) == 0);
    // Feature 2: above null guard pages (real addresses >= 64KB)
    passed += i32::from(val >= 0x10000);
    // Feature 3: has upper 32 bits (real 64-bit address, not a small constant)
    passed += i32::from((val >> 32) != 0);
    // Feature 4: above 4GB (in real 64-bit address space)
    passed += i32::from(val > 0x100000000);
    // Feature 5: user-mode address range (not kernel)
    passed += i32::from(val < 0xFFFF800000000000);
    FeatureResult { passed, checked }
}

/// `countPtrFeatures32` (`typeinfer.h:238-247`).
fn count_ptr_features32(val: u32) -> FeatureResult {
    let mut passed = 0;
    let checked = 3;
    // Feature 1: non-zero and not sentinel
    passed += i32::from(val != 0 && val != 0xFFFFFFFF);
    // Feature 2: aligned to 4
    passed += i32::from((val & 3) == 0);
    // Feature 3: above null guard pages (>= 64KB)
    passed += i32::from(val >= 0x10000);
    FeatureResult { passed, checked }
}

/// `countStringFeatures` (`typeinfer.h:251-272`).
fn count_string_features(data: &[u8], len: i32) -> FeatureResult {
    if len < 2 {
        return FeatureResult {
            passed: 0,
            checked: 4,
        };
    }
    let mut printable = 0;
    let mut letters = 0;
    let mut consecutive = 0;
    let mut max_consec = 0;
    for i in 0..len as usize {
        let c = data[i];
        if is_printable(c) {
            printable += 1;
            consecutive += 1;
            max_consec = std::cmp::max(max_consec, consecutive);
            if c.is_ascii_uppercase() || c.is_ascii_lowercase() {
                letters += 1;
            }
        } else {
            consecutive = 0;
        }
    }
    let ratio = printable as f64 / len as f64;
    let mut passed = 0;
    let checked = 4;
    passed += i32::from(max_consec >= 4);
    passed += i32::from(ratio > 0.75);
    passed += i32::from(letters >= 1);
    passed += i32::from(ratio > 0.90);
    FeatureResult { passed, checked }
}

/// `countInt16Features` (`typeinfer.h:276-291`).
fn count_int16_features(
    val: u16,
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
) -> FeatureResult {
    let mut passed = 0;
    let mut checked = 2;
    let sv = val as i16;
    passed += i32::from(val != 0);
    passed += i32::from((-16384..=16384).contains(&sv));

    if h.sample_count > 0 {
        if let (Some(min_p), Some(max_p)) = (min_p, max_p) {
            checked += 2;
            let min_v = load_u16(min_p);
            let max_v = load_u16(max_p);
            passed += i32::from(min_v <= 4096 && max_v <= 4096);
            passed += i32::from(min_v != max_v);
        }
    }
    FeatureResult { passed, checked }
}

/// `featureScore` (`typeinfer.h:295-298`).
#[inline]
fn feature_score(r: FeatureResult) -> i32 {
    if r.checked == 0 {
        return 0;
    }
    (r.passed * 100) / r.checked
}

/// `strengthFromScore` (`typeinfer.h:280-285`).
#[inline]
fn strength_from_score(score: i32) -> i32 {
    if score >= 75 {
        3
    } else if score >= 50 {
        2
    } else if score >= 25 {
        1
    } else {
        0
    }
}

/// `struct Candidate` (`typeinfer.h:313-316`).
#[derive(Clone)]
struct Candidate {
    kind: NodeKind,
    count: i32,
    score: i32,
}

type CandidateVec = SmallVec<[Candidate; 12]>;

/// `addCandidate` (`typeinfer.h:318-320`).
#[inline]
fn add_candidate(out: &mut CandidateVec, k: NodeKind, score: i32, min_score: i32) {
    if score >= min_score {
        out.push(Candidate {
            kind: k,
            count: 1,
            score,
        });
    }
}

/// `addSplitCandidate` (`typeinfer.h:322-327`).
#[inline]
fn add_split_candidate(
    out: &mut CandidateVec,
    k: NodeKind,
    count: i32,
    score: i32,
    min_score: i32,
) {
    if score >= min_score {
        out.push(Candidate {
            kind: k,
            count,
            score,
        });
    }
}

/// `tryWhole8` (`typeinfer.h:331-377`).
fn try_whole8(data: &[u8], h: &InferHints<'_>, out: &mut CandidateVec, min_candidate_score: i32) {
    let u64v = load_u64(data);

    // Pointer64
    if h.ptr_size == 8 {
        add_candidate(
            out,
            NodeKind::Pointer64,
            feature_score(count_ptr_features64(u64v)),
            min_candidate_score,
        );
    }

    // Double — rare in RE work; require strong evidence
    {
        let d = load_f64(data);
        let ad = d.abs();
        let mantissa = u64v & 0x000FFFFFFFFFFFFF;
        // Hard reject: outside plausible range [1e-6, 1e7] (matches float checker)
        let in_range = d == 0.0 || (1e-6..=1e7).contains(&ad);
        // Hard reject: lower 32 zero with non-zero mantissa (two 32-bit fields)
        let split_field = (u64v & 0xFFFFFFFF) == 0 && mantissa != 0;
        if in_range && !split_field {
            let exp = (u64v >> 52) & 0x7FF;
            let mut passed = 0;
            let checked = 4;
            // Feature 1: finite
            passed += i32::from(d.is_finite());
            // Feature 2: non-denormal
            passed += i32::from(exp > 0 || (u64v & 0x7FFFFFFFFFFFFFFF) == 0);
            // Feature 3: has fractional part or is a small special value
            let frac = d.fract().abs();
            passed += i32::from(frac > 0.001 || ad <= 1.0);
            // Feature 4: not a large exact integer (likely reinterpreted binary data)
            passed += i32::from(!(ad > 1000.0 && frac < 0.001));
            add_candidate(
                out,
                NodeKind::Double,
                feature_score(FeatureResult { passed, checked }),
                min_candidate_score,
            );
        }
    }

    // UTF8
    add_candidate(
        out,
        NodeKind::UTF8,
        feature_score(count_string_features(data, 8)),
        min_candidate_score,
    );

    // UInt64 / Int64 — only meaningful when value exceeds 32-bit range
    if (u64v >> 32) != 0 {
        let mut passed = 0;
        let checked = 3;
        // Feature 1: non-zero (always true after guard)
        passed += 1;
        // Feature 2: reasonable magnitude (below kernel range)
        passed += i32::from(u64v < 0x0000FFFFFFFFFFFF);
        // Feature 3: monotonic or page-aligned
        passed += i32::from(h.monotonic || (u64v & 0xFFF) == 0);
        add_candidate(
            out,
            NodeKind::UInt64,
            feature_score(FeatureResult { passed, checked }),
            min_candidate_score,
        );
    }
}

/// `tryWhole4` (`typeinfer.h:379-398`).
fn try_whole4(
    data: &[u8],
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
    out: &mut CandidateVec,
    min_candidate_score: i32,
) {
    let u32v = load_u32(data);

    // Float
    add_candidate(
        out,
        NodeKind::Float,
        feature_score(count_float_features(u32v, min_p, max_p, h)),
        min_candidate_score,
    );

    // Int32
    add_candidate(
        out,
        NodeKind::Int32,
        feature_score(count_int_features(u32v, min_p, max_p, h)),
        min_candidate_score,
    );

    // UInt32
    add_candidate(
        out,
        NodeKind::UInt32,
        feature_score(count_int_features(u32v, min_p, max_p, h)),
        min_candidate_score,
    );

    // Flags (only if sparse bits)
    add_candidate(
        out,
        NodeKind::UInt32,
        feature_score(count_flag_features(u32v, min_p, max_p, h)),
        min_candidate_score,
    );

    // Pointer32
    if h.ptr_size == 4 {
        add_candidate(
            out,
            NodeKind::Pointer32,
            feature_score(count_ptr_features32(u32v)),
            min_candidate_score,
        );
    }
}

/// `tryWhole2` (`typeinfer.h:400-406`).
fn try_whole2(
    data: &[u8],
    min_p: Option<&[u8]>,
    max_p: Option<&[u8]>,
    h: &InferHints<'_>,
    out: &mut CandidateVec,
    min_candidate_score: i32,
) {
    let u16v = load_u16(data);
    let score_i = feature_score(count_int16_features(u16v, min_p, max_p, h));
    add_candidate(out, NodeKind::Int16, score_i, min_candidate_score);
    add_candidate(out, NodeKind::UInt16, score_i, min_candidate_score);
}

/// `tryWhole1` (`typeinfer.h:408-412`).
fn try_whole1(data: &[u8], out: &mut CandidateVec, min_candidate_score: i32) {
    let v = data[0];
    let score = if v == 0 || v == 1 { 50 } else { 25 };
    add_candidate(out, NodeKind::UInt8, score, min_candidate_score);
}

/// `trySplitUniform` (`typeinfer.h:416-477`).
fn try_split_uniform(
    data: &[u8],
    len: i32,
    h: &InferHints<'_>,
    out: &mut CandidateVec,
    min_candidate_score: i32,
) {
    // 8 → 2×4
    if len == 8 {
        let min_a = h.min_observed;
        let min_b = h.min_observed.map(|p| &p[4..]);
        let max_a = h.max_observed;
        let max_b = h.max_observed.map(|p| &p[4..]);
        let z_a = all_zero(&data[0..4]);
        let z_b = all_zero(&data[4..8]);

        // Float×2: both halves must be good floats and at least one non-zero
        if !z_a || !z_b {
            let bits_a = load_u32(&data[0..4]);
            let bits_b = load_u32(&data[4..8]);
            let f_a = z_a || is_good_float(bits_a);
            let f_b = z_b || is_good_float(bits_b);
            if f_a && f_b {
                let r_a = if z_a {
                    FeatureResult {
                        passed: 2,
                        checked: 4,
                    }
                } else {
                    count_float_features(bits_a, min_a, max_a, h)
                };
                let r_b = if z_b {
                    FeatureResult {
                        passed: 2,
                        checked: 4,
                    }
                } else {
                    count_float_features(bits_b, min_b, max_b, h)
                };
                let score = std::cmp::min(feature_score(r_a), feature_score(r_b));
                add_split_candidate(out, NodeKind::Float, 2, score, min_candidate_score);
            }
        }

        // Int32×2: both halves, at least one non-zero
        if !z_a || !z_b {
            let r_a = if z_a {
                FeatureResult {
                    passed: 1,
                    checked: 3,
                }
            } else {
                count_int_features(load_u32(&data[0..4]), min_a, max_a, h)
            };
            let r_b = if z_b {
                FeatureResult {
                    passed: 1,
                    checked: 3,
                }
            } else {
                count_int_features(load_u32(&data[4..8]), min_b, max_b, h)
            };
            let score = std::cmp::min(feature_score(r_a), feature_score(r_b));
            add_split_candidate(out, NodeKind::Int32, 2, score, min_candidate_score);
        }

        // UInt32×2
        if !z_a || !z_b {
            let r_a = if z_a {
                FeatureResult {
                    passed: 1,
                    checked: 3,
                }
            } else {
                count_int_features(load_u32(&data[0..4]), min_a, max_a, h)
            };
            let r_b = if z_b {
                FeatureResult {
                    passed: 1,
                    checked: 3,
                }
            } else {
                count_int_features(load_u32(&data[4..8]), min_b, max_b, h)
            };
            let score = std::cmp::min(feature_score(r_a), feature_score(r_b));
            add_split_candidate(out, NodeKind::UInt32, 2, score, min_candidate_score);
        }
    }

    // 8 → 4×2 or 4 → 2×2
    let half_len = len / 2;
    if half_len == 2 {
        let mut min_score = 100;
        let count = len / 2;
        let mut any_non_zero = false;
        for i in 0..count {
            let off = (i * 2) as usize;
            let part = &data[off..off + 2];
            if !all_zero(part) {
                any_non_zero = true;
            }
            let mp = h.min_observed.map(|p| &p[off..]);
            let xp = h.max_observed.map(|p| &p[off..]);
            let s = feature_score(count_int16_features(load_u16(part), mp, xp, h));
            min_score = std::cmp::min(min_score, s);
        }
        if any_non_zero {
            add_split_candidate(out, NodeKind::Int16, count, min_score, min_candidate_score);
            add_split_candidate(out, NodeKind::UInt16, count, min_score, min_candidate_score);
        }
    }
}

/// `pruneAndRank` (`typeinfer.h:481-518`).
fn prune_and_rank(
    mut cands: CandidateVec,
    max_results: i32,
    min_strength: i32,
) -> Vec<TypeSuggestion> {
    // Sort descending by score. `std::sort` is not stable; mirror it with an
    // unstable sort so dedup tie-breaks match the C++ as closely as possible.
    cands.sort_by(|a, b| b.score.cmp(&a.score));

    // Dedup: keep highest-scoring per unique kinds vector.
    let mut deduped: CandidateVec = SmallVec::new();
    for c in &cands {
        let dup = deduped
            .iter()
            .any(|d| d.kind == c.kind && d.count == c.count);
        if !dup {
            deduped.push(c.clone());
        }
    }

    // Dominance: if top >= 1.5× second AND a 10-point absolute gap, keep only top.
    if deduped.len() >= 2
        && deduped[0].score >= deduped[1].score * 3 / 2
        && (deduped[0].score - deduped[1].score) >= 10
    {
        deduped.truncate(1);
    } else if deduped.len() > max_results as usize {
        deduped.truncate(max_results as usize);
    }

    let mut result = Vec::with_capacity(deduped.len());
    for c in &deduped {
        let str_ = strength_from_score(c.score);
        if str_ >= min_strength {
            result.push(TypeSuggestion {
                kinds: smallvec![c.kind; c.count as usize],
                score: c.score,
                strength: str_,
            });
        }
    }
    result
}

/// `inferTypes` (`typeinfer.h:524-548`) — the entry point. Returns up to
/// `max_results` ranked suggestions for the given byte slice.
pub fn infer_types(data: &[u8], hints: &InferHints<'_>, max_results: i32) -> Vec<TypeSuggestion> {
    infer_types_with_min_strength(data, hints, max_results, 1)
}

/// Hot-path variant for editor chips. Compose and hover only render strong
/// suggestions, so avoid allocating result entries they will immediately drop.
pub fn infer_strong_types(
    data: &[u8],
    hints: &InferHints<'_>,
    max_results: i32,
) -> Vec<TypeSuggestion> {
    infer_types_with_min_strength(data, hints, max_results, 3)
}

fn infer_types_with_min_strength(
    data: &[u8],
    hints: &InferHints<'_>,
    max_results: i32,
    min_strength: i32,
) -> Vec<TypeSuggestion> {
    let len = data.len() as i32;
    if data.is_empty() {
        return Vec::new();
    }
    if all_zero(data) {
        return Vec::new(); // NULL → skip entirely (typeinfer.h:532).
    }

    let mut cands: CandidateVec = SmallVec::new();
    let min_candidate_score = min_score_for_strength(min_strength);

    // Whole-width candidates.
    if len >= 8 {
        try_whole8(data, hints, &mut cands, min_candidate_score);
    }
    if len == 4 {
        try_whole4(
            data,
            hints.min_observed,
            hints.max_observed,
            hints,
            &mut cands,
            min_candidate_score,
        );
    }
    if len == 2 {
        try_whole2(
            data,
            hints.min_observed,
            hints.max_observed,
            hints,
            &mut cands,
            min_candidate_score,
        );
    }
    if len == 1 {
        try_whole1(data, &mut cands, min_candidate_score);
    }

    // Uniform splits (compete directly with whole-width candidates).
    if len >= 4 {
        try_split_uniform(data, len, hints, &mut cands, min_candidate_score);
    }

    prune_and_rank(cands, max_results, min_strength)
}

#[inline]
fn min_score_for_strength(min_strength: i32) -> i32 {
    match min_strength {
        i32::MIN..=0 => 25,
        1 => 25,
        2 => 50,
        _ => 75,
    }
}

// ─────────────────────────────────────────────────────────────────────────────
// Tests — translated 1:1 from `tests/test_typeinfer.cpp`.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── NULL / zero → empty ──

    #[test]
    fn null_ptr() {
        // C++ passes nullptr; the Rust API takes a slice, so the analogue is the
        // empty slice — both yield no suggestions.
        assert!(infer_types(&[], &InferHints::default(), 3).is_empty());
    }

    #[test]
    fn zero_len() {
        let d = [0u8; 4];
        assert!(infer_types(&d[..0], &InferHints::default(), 3).is_empty());
    }

    #[test]
    fn all_zeros8() {
        let d = [0u8; 8];
        assert!(infer_types(&d, &InferHints::default(), 3).is_empty());
    }

    #[test]
    fn all_zeros4() {
        let d = [0u8; 4];
        assert!(infer_types(&d, &InferHints::default(), 3).is_empty());
    }

    #[test]
    fn all_zeros2() {
        let d = [0u8; 2];
        assert!(infer_types(&d, &InferHints::default(), 3).is_empty());
    }

    // ── Hex64: float pair ──
    // {21.0488f, 547.3f} — two clear floats with fractional parts;
    // whole-width Double/Ptr64 score poorly → Float×2 dominates
    #[test]
    fn hex64_float_pair() {
        let a: f32 = 21.0488;
        let b: f32 = 547.3;
        let mut d = [0u8; 8];
        d[0..4].copy_from_slice(&a.to_le_bytes());
        d[4..8].copy_from_slice(&b.to_le_bytes());
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        let top = &r[0];
        assert_eq!(top.kinds.len(), 2);
        assert_eq!(top.kinds[0], NodeKind::Float);
        assert!(top.strength >= 3); // strong
    }

    // ── Hex64: int32 pair ──
    // {42, 99} — two small integers
    #[test]
    fn hex64_int_pair() {
        let a: i32 = 42;
        let b: i32 = 99;
        let mut d = [0u8; 8];
        d[0..4].copy_from_slice(&a.to_le_bytes());
        d[4..8].copy_from_slice(&b.to_le_bytes());
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        let top = &r[0];
        assert!(top.kinds.len() == 2);
        assert!(top.kinds[0] == NodeKind::Int32 || top.kinds[0] == NodeKind::UInt32);
    }

    // ── Hex64: UTF-8 string ──
    #[test]
    fn hex64_utf8() {
        let d: [u8; 8] = [b'I', b'C', b'h', b'o', b'o', b's', b'e', b'Y'];
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        // Top should be UTF8 (strong)
        let mut found_utf8 = false;
        for s in &r {
            if s.kinds.len() == 1 && s.kinds[0] == NodeKind::UTF8 {
                found_utf8 = true;
                assert!(s.strength >= 3); // strong
            }
        }
        assert!(found_utf8);
    }

    // ── Hex64: pointer-like value ──
    #[test]
    fn hex64_pointer() {
        // 0x00007FF6A0B01000 — typical Windows user-mode address
        let d: [u8; 8] = [0x00, 0x10, 0xB0, 0xA0, 0xF6, 0x7F, 0x00, 0x00];
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        let mut found_ptr = false;
        for s in &r {
            if s.kinds.len() == 1 && s.kinds[0] == NodeKind::Pointer64 {
                found_ptr = true;
            }
        }
        assert!(found_ptr);
    }

    // ── Hex32: clear float ──
    #[test]
    fn hex32_float() {
        // 21.0488f = 0x41A86600
        let d: [u8; 4] = [0x00, 0x66, 0xA8, 0x41];
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        assert_eq!(r[0].kinds.len(), 1);
        assert_eq!(r[0].kinds[0], NodeKind::Float);
        assert!(r[0].strength >= 2);
    }

    // ── Hex32: small integer with monotonic history ──
    #[test]
    fn hex32_int_monotonic() {
        // Value: 0x0000BFFC = 49148
        let d: [u8; 4] = [0xFC, 0xBF, 0x00, 0x00];
        let min_b: [u8; 4] = [0x10, 0x00, 0x00, 0x00]; // 16
        let max_b: [u8; 4] = [0xFC, 0xBF, 0x00, 0x00]; // 49148
        let h = InferHints {
            monotonic: true,
            sample_count: 10,
            min_observed: Some(&min_b),
            max_observed: Some(&max_b),
            ..InferHints::default()
        };
        let r = infer_types(&d, &h, 3);
        assert!(!r.is_empty());
        assert!(r[0].kinds[0] == NodeKind::Int32 || r[0].kinds[0] == NodeKind::UInt32);
        assert!(r[0].strength >= 2);
    }

    // ── Hex16: small unsigned ──
    #[test]
    fn hex16_uint() {
        let d: [u8; 2] = [0x5F, 0x00]; // 95
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        assert!(r[0].kinds[0] == NodeKind::Int16 || r[0].kinds[0] == NodeKind::UInt16);
    }

    // ── Hex8: uint8 ──
    #[test]
    fn hex8_uint() {
        let d: [u8; 1] = [1];
        let r = infer_types(&d, &InferHints::default(), 3);
        assert!(!r.is_empty());
        assert_eq!(r[0].kinds[0], NodeKind::UInt8);
    }

    // ── formatHint ──
    #[test]
    fn format_hint_single() {
        let s = TypeSuggestion {
            kinds: smallvec![NodeKind::Float],
            score: 0,
            strength: 3,
        };
        assert_eq!(format_hint(&s), "float");
    }

    #[test]
    fn format_hint_split() {
        let s = TypeSuggestion {
            kinds: smallvec![NodeKind::Float, NodeKind::Float],
            score: 0,
            strength: 3,
        };
        let h = format_hint(&s);
        assert_eq!(h, "float\u{00D7}2");
    }

    #[test]
    fn infer_strong_matches_filtering_public_results() {
        let fixtures = [
            {
                let mut d = [0u8; 8];
                d[..4].copy_from_slice(&1.5f32.to_le_bytes());
                d[4..].copy_from_slice(&2.25f32.to_le_bytes());
                d
            },
            {
                let mut d = [0u8; 8];
                d[..4].copy_from_slice(&14i32.to_le_bytes());
                d[4..].copy_from_slice(&20i32.to_le_bytes());
                d
            },
            0x0000_7FF6_A0B0_1000u64.to_le_bytes(),
            0x9E37_79B9_7F4A_7C15u64.to_le_bytes(),
        ];

        for data in fixtures {
            let expected: Vec<_> = infer_types(&data, &InferHints::default(), 3)
                .into_iter()
                .filter(|s| s.strength >= 3)
                .take(2)
                .collect();
            assert_eq!(
                infer_strong_types(&data, &InferHints::default(), 2),
                expected
            );
        }
    }

    // ── Denormal rejection ──
    #[test]
    fn denormal_rejected() {
        // Denormal float: exp=0, mantissa non-zero → 0x00000001
        let d: [u8; 4] = [0x01, 0x00, 0x00, 0x00];
        let r = infer_types(&d, &InferHints::default(), 3);
        // Should NOT suggest Float as top pick
        if !r.is_empty() && r[0].kinds.len() == 1 {
            assert!(r[0].kinds[0] != NodeKind::Float);
        }
    }

    // ── Benchmark inputs as plain smoke tests (no QBENCHMARK harness). ──
    #[test]
    fn bench_single_call_smoke() {
        let d: [u8; 8] = [0x00, 0x00, 0x80, 0x3F, 0xCD, 0xCC, 0x4C, 0x3E];
        // Must not panic; the C++ benchmark just calls it repeatedly.
        let _ = infer_types(&d, &InferHints::default(), 3);
    }

    #[test]
    fn bench_batch_refresh_smoke() {
        // Prepare 200 varied byte patterns (same generator as the C++ bench).
        for i in 0..200i64 {
            let seed = (i * 7919 + 1) as u32;
            let mut data = [0u8; 8];
            for j in 0..8i64 {
                data[j as usize] = ((seed >> (j * 3)) ^ ((i + j) as u32)) as u8;
            }
            let _ = infer_types(&data, &InferHints::default(), 3);
        }
    }

    #[test]
    fn infer_strong_matches_displayed_strong_prefix() {
        let hints = InferHints::default();
        for i in 0..512u64 {
            let data = i.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes();
            let displayed: Vec<_> = infer_types(&data, &hints, 3)
                .into_iter()
                .filter(|s| s.strength >= 3)
                .take(2)
                .collect();
            assert_eq!(infer_strong_types(&data, &hints, 2), displayed);
        }
    }
}
