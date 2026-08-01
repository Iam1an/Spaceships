//! JavaScript number formatting, reproduced exactly.
//!
//! The JSON this server emits has to be indistinguishable from what
//! `JSON.stringify` produces in `server/db.js`, and JS gets to two things Rust
//! does differently:
//!
//! 1. **`JSON.stringify` prints an integral double without a decimal point.**
//!    A trial time of exactly `28` seconds comes out of SQLite as the REAL
//!    `28.0` and reaches the browser as `28`, not `28.0`. `serde_json` would
//!    write `28.0`. [`JsNum`] fixes that.
//! 2. **`Number.prototype.toFixed` rounds halves *up*, not to even.** `kdr` is
//!    the only place this shows, and it shows immediately: 1 kill over 8 deaths
//!    is `0.125`, which JS renders `"0.13"` and Rust's `{:.2}` renders `"0.12"`.
//!    [`to_fixed_2`] fixes that.
//!
//! Neither is cosmetic. `kdr` is a *string* in the API response, so a
//! half-rounding difference is a literally different byte sequence on the wire,
//! and the leaderboard renders it verbatim.

use serde::{Serialize, Serializer};

/// A number that serializes the way `JSON.stringify` would.
///
/// `Int` is for values that are integers in the JS source too (a `target: 30`
/// literal, a kill count). `Real` is for SQLite REAL columns, which JS receives
/// as doubles and prints without a decimal point when they happen to be whole.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JsNum {
    /// An integer, always printed without a decimal point.
    Int(i64),
    /// A double, printed as an integer when its value is integral.
    Real(f64),
}

impl JsNum {
    /// The value as an `f64`, for comparisons.
    #[must_use]
    pub fn as_f64(self) -> f64 {
        match self {
            JsNum::Int(v) => v as f64,
            JsNum::Real(v) => v,
        }
    }
}

impl Serialize for JsNum {
    fn serialize<S: Serializer>(&self, ser: S) -> Result<S::Ok, S::Error> {
        match *self {
            JsNum::Int(v) => ser.serialize_i64(v),
            JsNum::Real(v) => serialize_js_f64(v, ser),
        }
    }
}

/// Writes an `f64` the way `JSON.stringify` does: as an integer when the value
/// is integral and fits, otherwise as a double.
///
/// Non-finite values would be `null` in JS. They cannot occur here — every
/// caller reads a SQLite REAL that the JS server itself wrote — so they are
/// passed through to `serde_json`, which errors rather than inventing a value.
pub fn serialize_js_f64<S: Serializer>(v: f64, ser: S) -> Result<S::Ok, S::Error> {
    if v.is_finite() && v.fract() == 0.0 && v.abs() < 9.007_199_254_740_992e15 {
        ser.serialize_i64(v as i64)
    } else {
        ser.serialize_f64(v)
    }
}

/// `Option<f64>` field helper for `#[serde(serialize_with = ...)]`.
///
/// SQLite `NULL` becomes JSON `null`, exactly as `better-sqlite3` delivers it.
pub fn serialize_opt_js_f64<S: Serializer>(v: &Option<f64>, ser: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(x) => serialize_js_f64(*x, ser),
        None => ser.serialize_none(),
    }
}

/// `Number.prototype.toFixed(2)`, for the non-negative finite values `kdr`
/// produces.
///
/// # Why not `format!("{:.2}")`
///
/// ECMA-262 defines `toFixed(f)` as: *let `n` be an integer for which
/// `n / 10^f - x` is as close to zero as possible; if there are two such `n`,
/// pick the **larger**.* That is round-half-**up**. Rust's float formatting
/// rounds half to **even**. They disagree on every exactly-representable tie,
/// and ties are reachable: `kdr` is `kills / deaths`, so 1/8 = `0.125`, 3/8 =
/// `0.375`, 5/8 = `0.625` and 7/8 = `0.875` all land on one, as does any
/// `k/2^n` with a short binary expansion.
///
/// | value | JS `toFixed(2)` | Rust `{:.2}` |
/// |-------|-----------------|--------------|
/// | 0.125 | `"0.13"`        | `"0.12"`     |
/// | 0.375 | `"0.38"`        | `"0.38"`     |
/// | 0.625 | `"0.63"`        | `"0.62"`     |
///
/// # How this gets it right
///
/// Format to 30 fractional digits first. Rust's formatter is exact — it prints
/// the correctly-rounded decimal expansion of the *binary* value, not of the
/// short literal a human wrote — so digit 3 onward tells us which side of the
/// tie we are on. Then round the decimal string half-up by hand.
///
/// 30 digits is enough because a tie at digit 3 requires the value to be
/// exactly `n/100 + 1/200`, which is a dyadic rational, which has a *finite*
/// binary and hence finite decimal expansion that terminates well before digit
/// 30. A value merely near a tie is separated from it by at least one ULP
/// (~2.2e-16 near 1.0), which digit 30 resolves with 14 orders of magnitude to
/// spare.
#[must_use]
pub fn to_fixed_2(x: f64) -> String {
    if x.is_nan() {
        return "NaN".to_string();
    }
    if x.is_infinite() {
        return if x > 0.0 { "Infinity" } else { "-Infinity" }.to_string();
    }
    let negative = x < 0.0;
    // Exact decimal expansion, far past the digit that decides the rounding.
    let exact = format!("{:.30}", x.abs());
    let (int_part, frac_part) = exact.split_once('.').unwrap_or((exact.as_str(), ""));

    let mut digits: Vec<u8> = int_part.bytes().map(|b| b - b'0').collect();
    let int_len = digits.len();
    digits.extend(frac_part.bytes().map(|b| b - b'0'));

    // Keep two fractional digits; everything after decides whether to round up.
    let keep = int_len + 2;
    let round_up = digits.get(keep).is_some_and(|&d| d >= 5);
    digits.truncate(keep);

    if round_up {
        let mut i = digits.len();
        loop {
            if i == 0 {
                digits.insert(0, 1);
                break;
            }
            i -= 1;
            if digits[i] == 9 {
                digits[i] = 0;
            } else {
                digits[i] += 1;
                break;
            }
        }
    }

    // A carry out of the most significant digit lengthened the integer part.
    let int_len = digits.len() - 2;
    let mut out = String::with_capacity(digits.len() + 2);
    if negative {
        out.push('-');
    }
    for (i, d) in digits.iter().enumerate() {
        if i == int_len {
            out.push('.');
        }
        out.push((b'0' + d) as char);
    }
    out
}

/// `server/db.js`'s `kdrStr`: kills over deaths, or just kills when the pilot
/// has never died.
///
/// The zero-deaths branch is `kills.toFixed(2)`, **not** a division — the JS
/// deliberately avoids `Infinity` by reporting the raw kill count with two
/// decimal places, so a pilot with 7 kills and 0 deaths has a `kdr` of
/// `"7.00"`.
#[must_use]
pub fn kdr_str(kills: i64, deaths: i64) -> String {
    if deaths > 0 {
        to_fixed_2(kills as f64 / deaths as f64)
    } else {
        to_fixed_2(kills as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_fixed_matches_js_on_ties() {
        // Every one of these was checked against `node -e "..."`. The four
        // eighths are the cases Rust's round-half-to-even gets wrong.
        assert_eq!(to_fixed_2(0.125), "0.13");
        assert_eq!(to_fixed_2(0.375), "0.38");
        assert_eq!(to_fixed_2(0.625), "0.63");
        assert_eq!(to_fixed_2(0.875), "0.88");
        assert_eq!(to_fixed_2(1.125), "1.13");
        assert_eq!(to_fixed_2(2.625), "2.63");
    }

    #[test]
    fn to_fixed_matches_js_on_near_ties() {
        // 1.005 is really 1.00499999999999989..., so JS rounds it *down*
        // despite looking like a tie. Naive `(x * 100).round() / 100` would
        // round it up and be wrong.
        assert_eq!(to_fixed_2(1.005), "1.00");
        assert_eq!(to_fixed_2(1.015), "1.01");
        assert_eq!(to_fixed_2(8.835), "8.84");
    }

    #[test]
    fn to_fixed_handles_carries_and_whole_numbers() {
        assert_eq!(to_fixed_2(0.0), "0.00");
        assert_eq!(to_fixed_2(7.0), "7.00");
        assert_eq!(to_fixed_2(0.999), "1.00");
        assert_eq!(to_fixed_2(9.999), "10.00");
        assert_eq!(to_fixed_2(99.999), "100.00");
        assert_eq!(to_fixed_2(1.0 / 3.0), "0.33");
        assert_eq!(to_fixed_2(2.0 / 3.0), "0.67");
    }

    #[test]
    fn kdr_matches_the_js_helper() {
        assert_eq!(kdr_str(0, 0), "0.00");
        // No deaths reports the kill count, not infinity.
        assert_eq!(kdr_str(7, 0), "7.00");
        assert_eq!(kdr_str(1, 8), "0.13");
        assert_eq!(kdr_str(10, 4), "2.50");
        assert_eq!(kdr_str(1, 3), "0.33");
    }

    #[test]
    fn js_numbers_drop_the_decimal_point_when_integral() {
        let v = serde_json::to_string(&JsNum::Real(28.0)).unwrap();
        assert_eq!(v, "28");
        assert_eq!(serde_json::to_string(&JsNum::Real(28.5)).unwrap(), "28.5");
        assert_eq!(serde_json::to_string(&JsNum::Int(30)).unwrap(), "30");
        assert_eq!(serde_json::to_string(&JsNum::Real(-0.0)).unwrap(), "0");
    }
}
