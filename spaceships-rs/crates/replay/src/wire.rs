//! The byte layer: a cursor, a reader, and one trait both sides implement.
//!
//! # Why this is hand-rolled
//!
//! `sim` has no `serde` derives and must not grow any — its `Cargo.toml` is
//! deliberately empty, and a dependency is the easiest way to smuggle a
//! `HashMap` iteration order or a platform float path into a crate whose whole
//! job is to be bit-identical everywhere. So the codec lives here, outside
//! `sim`, and reaches in through public fields.
//!
//! # The shape
//!
//! [`Wire`] pairs `put` with `get`, and [`wire_struct`] writes both halves of a
//! record from **one** list of field names. That is the property worth having:
//! a codec whose two directions are written separately will eventually disagree
//! about field order, and the failure mode is a replay that decodes into a
//! plausible-looking world with the wrong numbers in it. Here they cannot
//! disagree, because there is only one list.
//!
//! Everything is **little-endian, fixed width**. No varints: the log is
//! dominated by `f64` steering axes, which do not shrink as integers, and a
//! fixed stride is what lets the decoder reject a truncated file by length
//! rather than by running off the end.
//!
//! `f64` is written as its `to_bits()` pattern rather than as a decimal, so a
//! value round-trips **exactly**, including the last mantissa bit and `-0.0`.
//! That is not fussiness: the simulation is bit-deterministic, and a replay that
//! restored `0.1` as the nearest printable neighbour would diverge within a few
//! hundred ticks.

use spaceships_sim::math::{Quat, Vec3};

/// What can go wrong reading a recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// The file does not begin with [`crate::MAGIC`].
    NotARecording,
    /// The file's format version is not one this build reads.
    UnknownVersion {
        /// What the file says.
        found: u32,
        /// What this build writes.
        expected: u32,
    },
    /// The recording ran under different [`spaceships_sim::rules::Rules`] than
    /// this build has, so re-simulating it would not reproduce the match.
    ///
    /// See [`crate::Recording::rules_hash`] for why this is an error rather
    /// than a warning, and for the escape hatch.
    RulesChanged {
        /// The fingerprint the recording was made under.
        found: u64,
        /// This build's fingerprint.
        expected: u64,
    },
    /// The bytes ran out before the structure did.
    Truncated,
    /// A discriminant this version does not define. Almost always a corrupt
    /// file rather than a version skew, because the version is checked first.
    BadTag {
        /// What the bytes said.
        tag: u8,
        /// Which enum was being read.
        what: &'static str,
    },
    /// A length prefix larger than the bytes that remain. Checked rather than
    /// trusted, so a corrupt file cannot make the decoder try to allocate four
    /// gigabytes.
    BadLength {
        /// The count the file claimed.
        claimed: u64,
    },
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::NotARecording => write!(f, "not a Spaceships recording"),
            Error::UnknownVersion { found, expected } => {
                write!(
                    f,
                    "recording is format v{found}; this build reads v{expected}"
                )
            }
            Error::RulesChanged { found, expected } => write!(
                f,
                "recording ran under rules {found:#018x}; this build has {expected:#018x}",
            ),
            Error::Truncated => write!(f, "recording ends mid-record"),
            Error::BadTag { tag, what } => write!(f, "{tag} is not a {what}"),
            Error::BadLength { claimed } => {
                write!(f, "a length of {claimed} does not fit the file")
            }
        }
    }
}

impl std::error::Error for Error {}

/// Reading a recording either works or says why.
pub type Result<T> = core::result::Result<T, Error>;

// ---------------------------------------------------------------------------
// The cursor
// ---------------------------------------------------------------------------

/// Bytes being written.
#[derive(Debug, Default)]
pub struct Enc {
    bytes: Vec<u8>,
}

impl Enc {
    /// An empty buffer.
    #[must_use]
    pub fn new() -> Enc {
        Enc::default()
    }

    /// The buffer's contents.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// How many bytes have been written so far.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Whether nothing has been written yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Appends raw bytes.
    pub fn raw(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
}

/// Bytes being read, and how far through them we are.
#[derive(Debug)]
pub struct Dec<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Dec<'a> {
    /// A reader positioned at the start of `bytes`.
    #[must_use]
    pub fn new(bytes: &'a [u8]) -> Dec<'a> {
        Dec { bytes, at: 0 }
    }

    /// How many bytes are left.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.bytes.len() - self.at
    }

    /// Whether every byte has been consumed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.remaining() == 0
    }

    /// Takes `n` bytes, or reports that the file ended.
    pub fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(n).ok_or(Error::Truncated)?;
        let slice = self.bytes.get(self.at..end).ok_or(Error::Truncated)?;
        self.at = end;
        Ok(slice)
    }

    /// Reads a length prefix, rejecting one the remaining bytes cannot hold.
    ///
    /// `min_stride` is the smallest number of bytes one element can occupy, so
    /// a claimed count is checked against the file's real size before a single
    /// element is allocated. Without it a corrupt four-byte length is a
    /// four-gigabyte `Vec::with_capacity`.
    pub fn count(&mut self, min_stride: usize) -> Result<usize> {
        let claimed = u32::get(self)?;
        let n = claimed as usize;
        if min_stride > 0 && n.saturating_mul(min_stride) > self.remaining() {
            return Err(Error::BadLength {
                claimed: u64::from(claimed),
            });
        }
        Ok(n)
    }
}

// ---------------------------------------------------------------------------
// The trait
// ---------------------------------------------------------------------------

/// A value that can be written to a recording and read back exactly.
pub trait Wire: Sized {
    /// Appends this value.
    fn put(&self, e: &mut Enc);
    /// Reads one value.
    fn get(d: &mut Dec<'_>) -> Result<Self>;
}

macro_rules! wire_int {
    ($($t:ty),* $(,)?) => {$(
        impl Wire for $t {
            fn put(&self, e: &mut Enc) {
                e.raw(&self.to_le_bytes());
            }
            fn get(d: &mut Dec<'_>) -> Result<$t> {
                const N: usize = core::mem::size_of::<$t>();
                let bytes: [u8; N] = d.take(N)?.try_into().map_err(|_| Error::Truncated)?;
                Ok(<$t>::from_le_bytes(bytes))
            }
        }
    )*};
}

wire_int!(u8, u32, u64, i32);

/// `f64` goes over as its bit pattern. See the module docs.
impl Wire for f64 {
    fn put(&self, e: &mut Enc) {
        self.to_bits().put(e);
    }
    fn get(d: &mut Dec<'_>) -> Result<f64> {
        Ok(f64::from_bits(u64::get(d)?))
    }
}

/// One byte — and a byte that is neither `0` nor `1` is corruption rather than
/// truthiness, so it is rejected instead of coerced.
impl Wire for bool {
    fn put(&self, e: &mut Enc) {
        u8::from(*self).put(e);
    }
    fn get(d: &mut Dec<'_>) -> Result<bool> {
        match u8::get(d)? {
            0 => Ok(false),
            1 => Ok(true),
            tag => Err(Error::BadTag { tag, what: "bool" }),
        }
    }
}

impl<T: Wire> Wire for Option<T> {
    fn put(&self, e: &mut Enc) {
        match self {
            None => 0u8.put(e),
            Some(v) => {
                1u8.put(e);
                v.put(e);
            }
        }
    }
    fn get(d: &mut Dec<'_>) -> Result<Option<T>> {
        match u8::get(d)? {
            0 => Ok(None),
            1 => Ok(Some(T::get(d)?)),
            tag => Err(Error::BadTag {
                tag,
                what: "presence flag",
            }),
        }
    }
}

impl<T: Wire, const N: usize> Wire for [T; N] {
    fn put(&self, e: &mut Enc) {
        for v in self {
            v.put(e);
        }
    }
    fn get(d: &mut Dec<'_>) -> Result<[T; N]> {
        // No `Default` bound and no `MaybeUninit`: collect, then convert. `N` is
        // 2, 3 or 4 everywhere this is used.
        let mut items = Vec::with_capacity(N);
        for _ in 0..N {
            items.push(T::get(d)?);
        }
        items.try_into().map_err(|_| Error::Truncated)
    }
}

/// A length-prefixed list. The prefix is checked against the bytes that remain
/// before anything is allocated — see [`Dec::count`].
impl<T: Wire> Wire for Vec<T> {
    fn put(&self, e: &mut Enc) {
        let n = u32::try_from(self.len())
            .expect("a replay list longer than u32::MAX is a bug, not a match");
        n.put(e);
        for v in self {
            v.put(e);
        }
    }
    fn get(d: &mut Dec<'_>) -> Result<Vec<T>> {
        // One byte is the least any `Wire` value occupies, which bounds the
        // claim without knowing `T`.
        let n = d.count(1)?;
        let mut items = Vec::with_capacity(n);
        for _ in 0..n {
            items.push(T::get(d)?);
        }
        Ok(items)
    }
}

impl Wire for String {
    fn put(&self, e: &mut Enc) {
        let bytes = self.as_bytes();
        let n = u32::try_from(bytes.len()).expect("a callsign longer than u32::MAX");
        n.put(e);
        e.raw(bytes);
    }
    fn get(d: &mut Dec<'_>) -> Result<String> {
        let n = d.count(1)?;
        let bytes = d.take(n)?;
        // Invalid UTF-8 is corruption, and lossy is the honest answer for a
        // display string: it cannot fail, and nothing in the simulation reads
        // it.
        Ok(String::from_utf8_lossy(bytes).into_owned())
    }
}

// ---------------------------------------------------------------------------
// Records
// ---------------------------------------------------------------------------

/// Writes both halves of a plain record from one list of field names.
///
/// The order the fields appear here is the order they go on the wire *and* the
/// order they come back off it, because one list generates both. See the module
/// docs for why that matters more than it looks like it should.
#[macro_export]
macro_rules! wire_struct {
    ($t:ident { $($f:ident),* $(,)? }) => {
        impl $crate::wire::Wire for $t {
            fn put(&self, e: &mut $crate::wire::Enc) {
                $( $crate::wire::Wire::put(&self.$f, e); )*
            }
            fn get(d: &mut $crate::wire::Dec<'_>) -> $crate::wire::Result<$t> {
                // Struct-literal fields are evaluated in the order written, so
                // this reads back in exactly the order `put` wrote.
                Ok($t { $( $f: $crate::wire::Wire::get(d)?, )* })
            }
        }
    };
}

/// Writes a fieldless enum as one tagged byte.
///
/// The tags are **numbers written down here, not discriminants**: reordering
/// the variants in `sim` must not silently change what an old recording decodes
/// to, and this is the list that stops it.
#[macro_export]
macro_rules! wire_enum {
    ($t:ident, $what:literal { $($tag:literal => $variant:ident),* $(,)? }) => {
        impl $crate::wire::Wire for $t {
            fn put(&self, e: &mut $crate::wire::Enc) {
                let tag: u8 = match self { $( $t::$variant => $tag, )* };
                $crate::wire::Wire::put(&tag, e);
            }
            fn get(d: &mut $crate::wire::Dec<'_>) -> $crate::wire::Result<$t> {
                match <u8 as $crate::wire::Wire>::get(d)? {
                    $( $tag => Ok($t::$variant), )*
                    tag => Err($crate::wire::Error::BadTag { tag, what: $what }),
                }
            }
        }
    };
}

wire_struct!(Vec3 { x, y, z });
wire_struct!(Quat { x, y, z, w });

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip<T: Wire + PartialEq + core::fmt::Debug>(v: T) {
        let mut e = Enc::new();
        v.put(&mut e);
        let bytes = e.finish();
        let mut d = Dec::new(&bytes);
        assert_eq!(T::get(&mut d).unwrap(), v);
        assert!(
            d.is_empty(),
            "the decoder must consume exactly what was written"
        );
    }

    #[test]
    fn primitives_round_trip() {
        round_trip(0u8);
        round_trip(u32::MAX);
        round_trip(-7i32);
        round_trip(true);
        round_trip(false);
        round_trip(Some(9u64));
        round_trip(None::<u64>);
        round_trip(vec![1u32, 2, 3]);
        round_trip([1u32, 2]);
        round_trip("PILOT".to_owned());
        round_trip(String::new());
    }

    /// The property the whole format rests on: a float comes back with every
    /// bit it went in with, so a re-simulation starts from the same numbers.
    #[test]
    fn floats_survive_to_the_last_bit() {
        for v in [
            0.1,
            -0.0,
            f64::MIN_POSITIVE,
            f64::MAX,
            core::f64::consts::PI,
            1.0 / 3.0,
        ] {
            let mut e = Enc::new();
            v.put(&mut e);
            let bytes = e.finish();
            let back = f64::get(&mut Dec::new(&bytes)).unwrap();
            assert_eq!(back.to_bits(), v.to_bits(), "{v} lost bits");
        }
    }

    #[test]
    fn a_truncated_record_is_reported_not_guessed() {
        let mut e = Enc::new();
        1234u64.put(&mut e);
        let bytes = e.finish();
        assert_eq!(u64::get(&mut Dec::new(&bytes[..4])), Err(Error::Truncated));
    }

    /// A corrupt length prefix must be rejected against the file's real size
    /// rather than believed and allocated.
    #[test]
    fn an_impossible_length_is_rejected_before_allocating() {
        let mut e = Enc::new();
        u32::MAX.put(&mut e);
        let bytes = e.finish();
        assert!(matches!(
            Vec::<u64>::get(&mut Dec::new(&bytes)),
            Err(Error::BadLength { .. })
        ));
    }

    #[test]
    fn a_byte_that_is_not_a_bool_is_corruption() {
        let bytes = [7u8];
        assert_eq!(
            bool::get(&mut Dec::new(&bytes)),
            Err(Error::BadTag {
                tag: 7,
                what: "bool"
            })
        );
    }

    #[test]
    fn vectors_and_orientations_round_trip() {
        round_trip(Vec3::new(1.5, -2.5, 1e300));
        round_trip(Quat::new(0.0, 0.5, -0.5, core::f64::consts::FRAC_1_SQRT_2));
    }
}
