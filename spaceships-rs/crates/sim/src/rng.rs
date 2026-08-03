//! Seeded, deterministic pseudo-random number generation.
//!
//! # Why this exists instead of a crate
//!
//! The asteroid field is server-authoritative today: `generateAsteroidField` in
//! `server/index.js` builds 60 records with `Math.random()` and ships all of
//! them inside the `start` message. That works, but it means the field can only
//! ever come from the server, and the payload grows with the field.
//!
//! With a reproducible generator, the server can instead send a single `u64`
//! seed and both sides generate a bit-identical field locally. That only holds
//! if the generator is:
//!
//! 1. **Seeded explicitly** — no OS entropy, no thread-local state, no
//!    `Math.random()`.
//! 2. **Bit-identical everywhere** — same output on x86-64, aarch64, and
//!    wasm32. Integer arithmetic is exact on all of them, so the algorithm is
//!    built purely from wrapping integer ops.
//! 3. **Pinned forever** — a "harmless improvement" to the algorithm silently
//!    desynchronizes clients from servers. [`golden_sequence_is_pinned`] exists
//!    to make that break loudly at test time.
//!
//! [`golden_sequence_is_pinned`]: #
//!
//! # Algorithm
//!
//! PCG-XSH-RR 64/32 (O'Neill, 2014): a 64-bit LCG whose output is permuted by an
//! xorshift and a data-dependent rotation. Small (two `u64` of state), fast, and
//! far better distributed than the bare LCG or a plain xorshift. It is *not*
//! cryptographically secure, which is fine — nothing here guards a secret.
//!
//! ```
//! use spaceships_sim::rng::Rng;
//!
//! // The same seed always replays the same stream.
//! let mut a = Rng::new(0xC0FFEE);
//! let mut b = Rng::new(0xC0FFEE);
//! assert_eq!(a.next_u32(), b.next_u32());
//! ```

/// LCG multiplier from the reference PCG implementation.
const PCG_MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// Default stream selector, used when the caller does not pick one.
const PCG_DEFAULT_STREAM: u64 = 1_442_695_040_888_963_407;

/// A seeded, reproducible pseudo-random generator (PCG-XSH-RR 64/32).
///
/// Clone it to snapshot a stream position, e.g. to replay a subsystem without
/// disturbing the main sequence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rng {
    state: u64,
    /// Stream selector. Always odd, which is what makes the underlying LCG
    /// full-period.
    inc: u64,
}

impl Rng {
    /// Creates a generator from a seed, using the default stream.
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self::with_stream(seed, PCG_DEFAULT_STREAM)
    }

    /// Creates a generator from a seed and an explicit stream selector.
    ///
    /// Two generators with the same seed but different streams produce
    /// unrelated, non-overlapping sequences. Use this to give each subsystem
    /// (asteroid field, spawn jitter, bot decisions) its own stream from one
    /// match seed, so adding a call in one subsystem cannot shift the numbers
    /// another subsystem sees.
    #[must_use]
    pub fn with_stream(seed: u64, stream: u64) -> Self {
        let mut rng = Rng {
            state: 0,
            inc: (stream << 1) | 1,
        };
        rng.step();
        rng.state = rng.state.wrapping_add(seed);
        rng.step();
        rng
    }

    /// This generator's position, as the two words it is made of.
    ///
    /// # Why this is public
    ///
    /// A replay is a seed plus an input log, re-simulated — but only a replay
    /// that starts at tick 0 can rebuild the streams with
    /// [`crate::world::WorldRng::from_seed`]. By the time the first tick runs,
    /// the field generator has drawn a whole asteroid belt and every bot has
    /// rolled its opening missile delay, so a recording taken at the start of a
    /// *match* is already several thousand draws into four of the five streams.
    ///
    /// Writing the position out and reading it back is therefore the only way to
    /// restore a world exactly, and it is what a periodic snapshot needs as
    /// well: seeking a replay clones a `World` from ten seconds ago and
    /// fast-forwards, which is wrong by every subsequent roll if the generator
    /// does not come with it.
    ///
    /// [`Rng::from_raw`] is the inverse and the pair round-trips exactly. It is
    /// deliberately *not* a way to invent a stream: `inc` must be odd for the
    /// LCG to be full-period, and `from_raw` forces it, so the only values worth
    /// passing in are ones this method produced.
    #[must_use]
    pub fn to_raw(&self) -> (u64, u64) {
        (self.state, self.inc)
    }

    /// Restores a generator [`Rng::to_raw`] recorded.
    ///
    /// `inc` is forced odd, which is the one invariant the algorithm has and the
    /// only thing a caller could get wrong.
    #[must_use]
    pub fn from_raw(state: u64, inc: u64) -> Rng {
        Rng {
            state,
            inc: inc | 1,
        }
    }

    /// Advances the underlying LCG one step.
    #[inline]
    fn step(&mut self) {
        self.state = self
            .state
            .wrapping_mul(PCG_MULTIPLIER)
            .wrapping_add(self.inc);
    }

    /// Returns the next 32 random bits.
    #[inline]
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.step();
        // XSH: xorshift the high bits down, then RR: rotate by the top 5 bits.
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Returns the next 64 random bits, as two 32-bit draws (low half first).
    #[inline]
    pub fn next_u64(&mut self) -> u64 {
        let lo = u64::from(self.next_u32());
        let hi = u64::from(self.next_u32());
        (hi << 32) | lo
    }

    /// Returns a uniform `f64` in `[0, 1)`.
    ///
    /// Built by taking 53 random bits — exactly the mantissa width of an `f64` —
    /// and scaling by `2^-53`. That scale factor is a power of two, so the
    /// multiply is exact and the result is reproducible bit-for-bit.
    #[inline]
    pub fn next_f64(&mut self) -> f64 {
        const SCALE: f64 = 1.0 / ((1u64 << 53) as f64);
        ((self.next_u64() >> 11) as f64) * SCALE
    }

    /// Returns a uniform `f64` in `[-1, 1)`.
    ///
    /// This is the `Math.random() * 2 - 1` idiom that the JS asteroid and spawn
    /// code uses throughout.
    #[inline]
    pub fn next_f64_signed(&mut self) -> f64 {
        self.next_f64() * 2.0 - 1.0
    }

    /// Returns a uniform `f64` in `[lo, hi)`.
    ///
    /// Returns `lo` when `hi <= lo` rather than producing a nonsensical range.
    #[inline]
    pub fn range_f64(&mut self, lo: f64, hi: f64) -> f64 {
        if hi <= lo {
            lo
        } else {
            lo + self.next_f64() * (hi - lo)
        }
    }

    /// Returns a uniform `u32` in `[0, bound)`.
    ///
    /// Uses rejection sampling to stay unbiased — plain `next_u32() % bound`
    /// over-represents the low values whenever `bound` is not a power of two.
    /// Returns `0` for `bound == 0`.
    pub fn bounded_u32(&mut self, bound: u32) -> u32 {
        if bound == 0 {
            return 0;
        }
        // Values below this threshold would come from a short final block and
        // would skew the distribution, so they are redrawn.
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return r % bound;
            }
        }
    }

    /// Returns a uniform `usize` in `[0, bound)`. Returns `0` for `bound == 0`.
    ///
    /// Bounds above `u32::MAX` are not supported and will panic in debug builds;
    /// nothing in this simulation indexes collections that large.
    pub fn bounded_usize(&mut self, bound: usize) -> usize {
        debug_assert!(bound <= u32::MAX as usize, "bound exceeds u32::MAX");
        self.bounded_u32(bound as u32) as usize
    }

    /// Returns `true` with probability `p`, clamped to `[0, 1]`.
    #[inline]
    pub fn bool_with_probability(&mut self, p: f64) -> bool {
        self.next_f64() < p
    }

    /// Picks an index into a weighted table, mirroring `pickAsteroidTier` in
    /// `server/index.js`: draw once, walk the cumulative weights, return the
    /// first bucket the draw falls into.
    ///
    /// Weights need not sum to 1; the draw is scaled by the total. Returns the
    /// last index if the accumulation falls short (floating-point slack), and
    /// `0` for an empty table.
    pub fn weighted_index(&mut self, weights: &[f64]) -> usize {
        if weights.is_empty() {
            return 0;
        }
        let total: f64 = weights.iter().sum();
        // Guards an all-zero, negative, or NaN table; `is_finite` is what
        // catches the NaN case, since every comparison against NaN is false.
        if !total.is_finite() || total <= 0.0 {
            return 0;
        }
        let r = self.next_f64() * total;
        let mut acc = 0.0;
        for (i, w) in weights.iter().enumerate() {
            acc += w;
            if r < acc {
                return i;
            }
        }
        weights.len() - 1
    }

    /// Shuffles a slice in place (Fisher-Yates, back to front).
    pub fn shuffle<T>(&mut self, slice: &mut [T]) {
        for i in (1..slice.len()).rev() {
            let j = self.bounded_usize(i + 1);
            slice.swap(i, j);
        }
    }

    /// Derives an independent child generator without disturbing this one's
    /// position beyond the two draws it consumes.
    ///
    /// Useful for handing a subsystem its own stream from a parent seed.
    #[must_use]
    pub fn fork(&mut self) -> Rng {
        let seed = self.next_u64();
        let stream = self.next_u64();
        Rng::with_stream(seed, stream)
    }
}

#[cfg(test)]
mod tests {
    use super::Rng;

    /// **Do not update these numbers to make a failing test pass.**
    ///
    /// They pin the exact output of the generator. If this test fails, the
    /// algorithm changed, and a changed algorithm means a Rust server and a WASM
    /// client built at different times would generate different asteroid fields
    /// from the same seed. Either revert the change, or accept it as a protocol
    /// break and version it.
    ///
    /// These values were cross-checked against an independent implementation of
    /// the reference PCG-XSH-RR 64/32 algorithm, so they pin the *published*
    /// algorithm, not merely whatever this file happened to do first.
    const GOLDEN_SEED: u64 = 0x5EED_0000_0000_0001;
    const GOLDEN_U32: [u32; 8] = [
        784_178_004,
        3_006_191_981,
        2_512_035_574,
        181_865_118,
        840_835_581,
        2_701_679_628,
        3_979_261_105,
        1_670_189_378,
    ];

    #[test]
    fn golden_sequence_is_pinned() {
        let mut rng = Rng::new(GOLDEN_SEED);
        let got: Vec<u32> = (0..GOLDEN_U32.len()).map(|_| rng.next_u32()).collect();
        assert_eq!(
            got.as_slice(),
            GOLDEN_U32.as_slice(),
            "the PRNG algorithm changed — see the comment above GOLDEN_U32"
        );
    }

    #[test]
    fn same_seed_replays_the_same_stream() {
        let mut a = Rng::new(12345);
        let mut b = Rng::new(12345);
        for _ in 0..1000 {
            assert_eq!(a.next_u32(), b.next_u32());
        }
        assert_eq!(a, b);
    }

    #[test]
    fn different_seeds_diverge() {
        let mut a = Rng::new(1);
        let mut b = Rng::new(2);
        let sa: Vec<u32> = (0..32).map(|_| a.next_u32()).collect();
        let sb: Vec<u32> = (0..32).map(|_| b.next_u32()).collect();
        assert_ne!(sa, sb);
    }

    #[test]
    fn different_streams_diverge_from_the_same_seed() {
        let mut a = Rng::with_stream(7, 1);
        let mut b = Rng::with_stream(7, 2);
        let sa: Vec<u32> = (0..32).map(|_| a.next_u32()).collect();
        let sb: Vec<u32> = (0..32).map(|_| b.next_u32()).collect();
        assert_ne!(sa, sb);
    }

    #[test]
    fn clone_replays_from_the_snapshot_point() {
        let mut rng = Rng::new(99);
        for _ in 0..10 {
            rng.next_u32();
        }
        let mut snapshot = rng.clone();
        let a: Vec<u32> = (0..16).map(|_| rng.next_u32()).collect();
        let b: Vec<u32> = (0..16).map(|_| snapshot.next_u32()).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn next_u64_combines_two_draws() {
        let mut a = Rng::new(4242);
        let combined = a.next_u64();
        let mut b = Rng::new(4242);
        let lo = u64::from(b.next_u32());
        let hi = u64::from(b.next_u32());
        assert_eq!(combined, (hi << 32) | lo);
    }

    #[test]
    fn next_f64_stays_in_unit_interval() {
        let mut rng = Rng::new(7);
        for _ in 0..20_000 {
            let x = rng.next_f64();
            assert!((0.0..1.0).contains(&x), "out of range: {x}");
        }
    }

    #[test]
    fn next_f64_signed_stays_in_symmetric_interval() {
        let mut rng = Rng::new(8);
        let mut sum = 0.0;
        const N: usize = 20_000;
        for _ in 0..N {
            let x = rng.next_f64_signed();
            assert!((-1.0..1.0).contains(&x), "out of range: {x}");
            sum += x;
        }
        // Mean of a symmetric uniform draw should sit near zero.
        assert!((sum / N as f64).abs() < 0.05);
    }

    #[test]
    fn range_f64_respects_bounds_and_degenerate_input() {
        let mut rng = Rng::new(9);
        for _ in 0..10_000 {
            let x = rng.range_f64(5.0, 7.0);
            assert!((5.0..7.0).contains(&x), "out of range: {x}");
        }
        assert_eq!(rng.range_f64(3.0, 3.0), 3.0);
        assert_eq!(rng.range_f64(3.0, 1.0), 3.0);
    }

    #[test]
    fn bounded_u32_covers_its_whole_range_without_exceeding_it() {
        let mut rng = Rng::new(10);
        let mut seen = [0usize; 6];
        for _ in 0..60_000 {
            let v = rng.bounded_u32(6) as usize;
            assert!(v < 6);
            seen[v] += 1;
        }
        // Every bucket should appear, and roughly evenly (expected 10_000 each).
        for count in seen {
            assert!(count > 8_000, "bucket badly under-represented: {seen:?}");
            assert!(count < 12_000, "bucket badly over-represented: {seen:?}");
        }
    }

    #[test]
    fn bounded_u32_handles_edge_bounds() {
        let mut rng = Rng::new(11);
        assert_eq!(rng.bounded_u32(0), 0);
        assert_eq!(rng.bounded_u32(1), 0);
        for _ in 0..100 {
            assert!(rng.bounded_u32(u32::MAX) < u32::MAX);
        }
    }

    #[test]
    fn bounded_usize_matches_bounded_u32() {
        let mut a = Rng::new(12);
        let mut b = Rng::new(12);
        for _ in 0..100 {
            assert_eq!(a.bounded_usize(37), b.bounded_u32(37) as usize);
        }
    }

    #[test]
    fn bool_with_probability_hits_expected_frequency() {
        let mut rng = Rng::new(13);
        let hits = (0..20_000)
            .filter(|_| rng.bool_with_probability(0.25))
            .count();
        assert!((4_500..5_500).contains(&hits), "hits = {hits}");

        let mut rng = Rng::new(14);
        assert!(!(0..1_000).any(|_| rng.bool_with_probability(0.0)));
        let mut rng = Rng::new(15);
        assert!((0..1_000).all(|_| rng.bool_with_probability(1.0)));
    }

    #[test]
    fn weighted_index_matches_the_js_asteroid_tier_table() {
        // ASTEROID_TIERS weights from server/index.js.
        let weights = [0.45, 0.30, 0.18, 0.07];
        let mut rng = Rng::new(16);
        let mut counts = [0usize; 4];
        const N: usize = 100_000;
        for _ in 0..N {
            counts[rng.weighted_index(&weights)] += 1;
        }
        for (i, w) in weights.iter().enumerate() {
            let observed = counts[i] as f64 / N as f64;
            assert!(
                (observed - w).abs() < 0.01,
                "tier {i}: expected ~{w}, observed {observed}"
            );
        }
    }

    #[test]
    fn weighted_index_handles_degenerate_tables() {
        let mut rng = Rng::new(17);
        assert_eq!(rng.weighted_index(&[]), 0);
        assert_eq!(rng.weighted_index(&[0.0, 0.0]), 0);
        assert_eq!(rng.weighted_index(&[1.0]), 0);
    }

    #[test]
    fn shuffle_permutes_deterministically() {
        let mut a: Vec<u32> = (0..64).collect();
        let mut b = a.clone();
        Rng::new(18).shuffle(&mut a);
        Rng::new(18).shuffle(&mut b);
        assert_eq!(a, b, "same seed must produce the same permutation");

        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..64).collect::<Vec<u32>>(), "elements were lost");
        assert_ne!(a, sorted, "shuffle left the slice in order");

        // Degenerate lengths must not panic.
        Rng::new(19).shuffle(&mut [] as &mut [u32]);
        Rng::new(19).shuffle(&mut [1u32]);
    }

    #[test]
    fn fork_is_deterministic_and_independent() {
        let mut parent_a = Rng::new(20);
        let mut parent_b = Rng::new(20);
        let mut child_a = parent_a.fork();
        let mut child_b = parent_b.fork();
        let sa: Vec<u32> = (0..16).map(|_| child_a.next_u32()).collect();
        let sb: Vec<u32> = (0..16).map(|_| child_b.next_u32()).collect();
        assert_eq!(sa, sb, "forking is not reproducible");

        // The child stream should not simply echo the parent's.
        let parent_tail: Vec<u32> = (0..16).map(|_| parent_a.next_u32()).collect();
        assert_ne!(sa, parent_tail);
    }

    /// A generator written out mid-stream and read back must carry on from
    /// exactly where it was, not from where it started. This is the property a
    /// replay snapshot rests on.
    #[test]
    fn a_generator_round_trips_through_its_raw_words() {
        let mut original = Rng::with_stream(0xC0FFEE, 4);
        for _ in 0..97 {
            original.next_u32();
        }

        let (state, inc) = original.to_raw();
        let mut restored = Rng::from_raw(state, inc);

        assert_eq!(original, restored, "the words must describe the generator");
        let a: Vec<u32> = (0..32).map(|_| original.next_u32()).collect();
        let b: Vec<u32> = (0..32).map(|_| restored.next_u32()).collect();
        assert_eq!(a, b, "and the restored stream must continue, not restart");
    }

    /// `inc` must stay odd whatever a caller hands over.
    #[test]
    fn a_restored_generator_keeps_its_stream_odd() {
        assert_eq!(Rng::from_raw(1, 8).to_raw().1, 9);
    }
}
