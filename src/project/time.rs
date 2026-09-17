//! Exact time primitives used by the project model.
//!
//! The editor keeps time as reduced rationals.  Conversion to `f64` is
//! intentionally confined to the small boundary helpers in this module; all
//! clip ranges, frame schedules, and source mappings use checked integer
//! arithmetic.

use serde::de::{self, Deserializer};
use serde::{Deserialize, Serialize};
use std::cmp::Ordering;
use std::error::Error;
use std::fmt;

/// Errors returned by exact rational operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RationalError {
    ZeroDenominator,
    Overflow,
    NonFinite,
    InvalidDecimal,
}

impl fmt::Display for RationalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ZeroDenominator => formatter.write_str("rational denominator must be positive"),
            Self::Overflow => formatter.write_str("rational operation overflowed"),
            Self::NonFinite => formatter.write_str("time must be finite"),
            Self::InvalidDecimal => formatter.write_str("invalid decimal time"),
        }
    }
}

impl Error for RationalError {}

/// A reduced rational number with a positive, bounded denominator.
///
/// The representation is canonical: the denominator is positive and the
/// numerator and denominator have no common factor.  This makes exact time
/// values stable as map keys and stable across JSON round trips.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rational {
    pub numerator: i64,
    pub denominator: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RationalWire {
    numerator: i64,
    denominator: u32,
}

impl<'de> Deserialize<'de> for Rational {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = RationalWire::deserialize(deserializer)?;
        Self::new(wire.numerator, wire.denominator).map_err(de::Error::custom)
    }
}

impl Rational {
    pub const ZERO: Self = Self {
        numerator: 0,
        denominator: 1,
    };

    pub const ONE: Self = Self {
        numerator: 1,
        denominator: 1,
    };

    /// Construct and reduce a rational value.
    pub fn new(numerator: i64, denominator: u32) -> Result<Self, RationalError> {
        if denominator == 0 {
            return Err(RationalError::ZeroDenominator);
        }
        Self::from_i128_ratio(i128::from(numerator), i128::from(denominator))
    }

    pub const fn from_integer(value: i64) -> Self {
        Self {
            numerator: value,
            denominator: 1,
        }
    }

    /// Parse a finite decimal using its decimal spelling, avoiding the binary
    /// rounding error that would result from multiplying an `f64` by a large
    /// frame count.
    pub fn from_seconds(seconds: f64) -> Result<Self, RationalError> {
        if !seconds.is_finite() {
            return Err(RationalError::NonFinite);
        }
        Self::from_decimal_str(&seconds.to_string())
    }

    pub fn from_decimal_str(value: &str) -> Result<Self, RationalError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(RationalError::InvalidDecimal);
        }

        let (mantissa, exponent) = match value.find(['e', 'E']) {
            Some(index) => {
                let exponent = value[index + 1..]
                    .parse::<i32>()
                    .map_err(|_| RationalError::InvalidDecimal)?;
                (&value[..index], exponent)
            }
            None => (value, 0),
        };

        let (negative, mantissa) = match mantissa.as_bytes().first() {
            Some(b'-') => (true, &mantissa[1..]),
            Some(b'+') => (false, &mantissa[1..]),
            _ => (false, mantissa),
        };
        let mut pieces = mantissa.split('.');
        let whole = pieces.next().unwrap_or_default();
        let fraction = pieces.next().unwrap_or_default();
        if pieces.next().is_some()
            || (whole.is_empty() && fraction.is_empty())
            || !whole.bytes().all(|byte| byte.is_ascii_digit())
            || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        {
            return Err(RationalError::InvalidDecimal);
        }

        let digits = format!("{whole}{fraction}");
        let mut numerator = digits
            .parse::<u128>()
            .map_err(|_| RationalError::Overflow)?;
        let decimal_places = i32::try_from(fraction.len()).map_err(|_| RationalError::Overflow)?;
        let scale = decimal_places
            .checked_sub(exponent)
            .ok_or(RationalError::Overflow)?;

        let denominator = if scale >= 0 {
            pow10(u32::try_from(scale).map_err(|_| RationalError::Overflow)?)?
        } else {
            let positive_scale = scale.checked_neg().ok_or(RationalError::Overflow)?;
            numerator = numerator
                .checked_mul(pow10(
                    u32::try_from(positive_scale).map_err(|_| RationalError::Overflow)?,
                )?)
                .ok_or(RationalError::Overflow)?;
            1
        };

        let numerator = i128::try_from(numerator).map_err(|_| RationalError::Overflow)?;
        let numerator = if negative { -numerator } else { numerator };
        Self::from_i128_ratio(
            numerator,
            i128::try_from(denominator).map_err(|_| RationalError::Overflow)?,
        )
    }

    pub fn is_zero(self) -> bool {
        self.numerator == 0
    }

    pub fn is_negative(self) -> bool {
        self.numerator < 0
    }

    pub fn checked_add(self, other: Self) -> Result<Self, RationalError> {
        let numerator = i128::from(self.numerator) * i128::from(other.denominator)
            + i128::from(other.numerator) * i128::from(self.denominator);
        let denominator = i128::from(self.denominator) * i128::from(other.denominator);
        Self::from_i128_ratio(numerator, denominator)
    }

    pub fn checked_sub(self, other: Self) -> Result<Self, RationalError> {
        self.checked_add(other.checked_neg()?)
    }

    pub fn checked_mul(self, other: Self) -> Result<Self, RationalError> {
        let numerator = i128::from(self.numerator) * i128::from(other.numerator);
        let denominator = i128::from(self.denominator) * i128::from(other.denominator);
        Self::from_i128_ratio(numerator, denominator)
    }

    pub fn checked_div(self, other: Self) -> Result<Self, RationalError> {
        if other.is_zero() {
            return Err(RationalError::ZeroDenominator);
        }
        let numerator = i128::from(self.numerator) * i128::from(other.denominator);
        let denominator = i128::from(self.denominator) * i128::from(other.numerator);
        Self::from_i128_ratio(numerator, denominator)
    }

    pub fn checked_mul_integer(self, value: i64) -> Result<Self, RationalError> {
        Self::from_i128_ratio(
            i128::from(self.numerator) * i128::from(value),
            i128::from(self.denominator),
        )
    }

    pub fn checked_neg(self) -> Result<Self, RationalError> {
        Self::from_i128_ratio(-i128::from(self.numerator), i128::from(self.denominator))
    }

    pub fn checked_abs(self) -> Result<Self, RationalError> {
        Self::from_i128_ratio(
            i128::from(self.numerator).abs(),
            i128::from(self.denominator),
        )
    }

    pub fn floor_i128(self) -> i128 {
        let numerator = i128::from(self.numerator);
        let denominator = i128::from(self.denominator);
        let quotient = numerator / denominator;
        let remainder = numerator % denominator;
        if remainder != 0 && numerator < 0 {
            quotient - 1
        } else {
            quotient
        }
    }

    pub fn ceil_i128(self) -> i128 {
        let numerator = i128::from(self.numerator);
        let denominator = i128::from(self.denominator);
        let quotient = numerator / denominator;
        let remainder = numerator % denominator;
        if remainder != 0 && numerator > 0 {
            quotient + 1
        } else {
            quotient
        }
    }

    pub fn to_f64(self) -> f64 {
        self.numerator as f64 / f64::from(self.denominator)
    }

    /// Return the exact numerator/denominator pair as signed/unsigned values.
    pub fn parts(self) -> (i64, u32) {
        (self.numerator, self.denominator)
    }

    fn from_i128_ratio(mut numerator: i128, mut denominator: i128) -> Result<Self, RationalError> {
        if denominator == 0 {
            return Err(RationalError::ZeroDenominator);
        }
        if denominator < 0 {
            numerator = numerator.checked_neg().ok_or(RationalError::Overflow)?;
            denominator = denominator.checked_neg().ok_or(RationalError::Overflow)?;
        }

        let divisor = gcd_u128(numerator.unsigned_abs(), denominator as u128);
        let divisor_i128 = i128::try_from(divisor).map_err(|_| RationalError::Overflow)?;
        numerator /= divisor_i128;
        denominator /= divisor_i128;
        let numerator = i64::try_from(numerator).map_err(|_| RationalError::Overflow)?;
        let denominator = u32::try_from(denominator).map_err(|_| RationalError::Overflow)?;
        if denominator == 0 {
            return Err(RationalError::ZeroDenominator);
        }
        Ok(Self {
            numerator,
            denominator,
        })
    }
}

impl Default for Rational {
    fn default() -> Self {
        Self::ZERO
    }
}

impl PartialOrd for Rational {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Rational {
    fn cmp(&self, other: &Self) -> Ordering {
        (i128::from(self.numerator) * i128::from(other.denominator))
            .cmp(&(i128::from(other.numerator) * i128::from(self.denominator)))
    }
}

impl fmt::Display for Rational {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.denominator == 1 {
            write!(formatter, "{}", self.numerator)
        } else {
            write!(formatter, "{}/{}", self.numerator, self.denominator)
        }
    }
}

impl From<i64> for Rational {
    fn from(value: i64) -> Self {
        Self::from_integer(value)
    }
}

/// The time type used by projects and clips.
pub type Time = Rational;

/// Backwards-friendly name for APIs that want to make the rational contract
/// explicit at call sites.
pub type RationalTime = Rational;

/// A non-empty half-open interval `[start, end)`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TimeRange {
    pub start: Time,
    pub end: Time,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeRangeWire {
    start: Time,
    end: Time,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimeRangeError {
    EmptyOrReversed,
    Arithmetic(RationalError),
}

impl fmt::Display for TimeRangeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyOrReversed => {
                formatter.write_str("time range must have end greater than start")
            }
            Self::Arithmetic(error) => error.fmt(formatter),
        }
    }
}

impl Error for TimeRangeError {}

impl From<RationalError> for TimeRangeError {
    fn from(error: RationalError) -> Self {
        Self::Arithmetic(error)
    }
}

impl<'de> Deserialize<'de> for TimeRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TimeRangeWire::deserialize(deserializer)?;
        Self::new(wire.start, wire.end).map_err(de::Error::custom)
    }
}

impl TimeRange {
    pub fn new(start: Time, end: Time) -> Result<Self, TimeRangeError> {
        if end <= start {
            return Err(TimeRangeError::EmptyOrReversed);
        }
        Ok(Self { start, end })
    }

    pub fn from_start_duration(start: Time, duration: Time) -> Result<Self, TimeRangeError> {
        if duration <= Time::ZERO {
            return Err(TimeRangeError::EmptyOrReversed);
        }
        Self::new(start, start.checked_add(duration)?)
    }

    pub fn duration(self) -> Result<Time, RationalError> {
        self.end.checked_sub(self.start)
    }

    pub fn contains(self, time: Time) -> bool {
        self.start <= time && time < self.end
    }

    pub fn intersects(self, other: Self) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn intersection(self, other: Self) -> Option<Self> {
        let start = self.start.max(other.start);
        let end = self.end.min(other.end);
        Self::new(start, end).ok()
    }

    pub fn shift(self, delta: Time) -> Result<Self, TimeRangeError> {
        Self::new(self.start.checked_add(delta)?, self.end.checked_add(delta)?)
    }

    pub fn with_start(self, start: Time) -> Result<Self, TimeRangeError> {
        Self::new(start, self.end)
    }

    pub fn with_end(self, end: Time) -> Result<Self, TimeRangeError> {
        Self::new(self.start, end)
    }
}

/// The project's constant output frame cadence.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FrameRate {
    pub numerator: u32,
    pub denominator: u32,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FrameRateWire {
    numerator: u32,
    denominator: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameRateError {
    NonPositive,
    Arithmetic(RationalError),
}

impl fmt::Display for FrameRateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonPositive => formatter.write_str("frame rate must be positive"),
            Self::Arithmetic(error) => error.fmt(formatter),
        }
    }
}

impl Error for FrameRateError {}

impl From<RationalError> for FrameRateError {
    fn from(error: RationalError) -> Self {
        Self::Arithmetic(error)
    }
}

impl<'de> Deserialize<'de> for FrameRate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = FrameRateWire::deserialize(deserializer)?;
        Self::new(wire.numerator, wire.denominator).map_err(de::Error::custom)
    }
}

impl FrameRate {
    pub const NTSC_24: Self = Self {
        numerator: 24_000,
        denominator: 1_001,
    };

    pub const FPS_24: Self = Self {
        numerator: 24,
        denominator: 1,
    };

    pub const FPS_30: Self = Self {
        numerator: 30,
        denominator: 1,
    };

    pub const FPS_60: Self = Self {
        numerator: 60,
        denominator: 1,
    };

    pub fn new(numerator: u32, denominator: u32) -> Result<Self, FrameRateError> {
        if numerator == 0 || denominator == 0 {
            return Err(FrameRateError::NonPositive);
        }
        let divisor = gcd_u128(u128::from(numerator), u128::from(denominator)) as u32;
        Ok(Self {
            numerator: numerator / divisor,
            denominator: denominator / divisor,
        })
    }

    pub fn from_fps(fps: f64) -> Result<Self, FrameRateError> {
        let rational = Rational::from_seconds(fps)?;
        if rational <= Time::ZERO {
            return Err(FrameRateError::NonPositive);
        }
        let numerator = u32::try_from(rational.numerator).map_err(|_| RationalError::Overflow)?;
        Self::new(numerator, rational.denominator)
    }

    pub fn as_rational(self) -> Result<Rational, RationalError> {
        Rational::new(self.numerator as i64, self.denominator)
    }

    pub fn fps(self) -> f64 {
        f64::from(self.numerator) / f64::from(self.denominator)
    }

    pub fn frame_duration(self) -> Result<Time, RationalError> {
        Rational::new(self.denominator as i64, self.numerator)
    }

    pub fn frame_start(self, index: u64) -> Result<Time, RationalError> {
        let numerator = i128::from(index)
            .checked_mul(i128::from(self.denominator))
            .ok_or(RationalError::Overflow)?;
        Rational::from_i128_ratio(numerator, i128::from(self.numerator))
    }

    pub fn frame_end(self, index: u64) -> Result<Time, RationalError> {
        self.frame_start(index.checked_add(1).ok_or(RationalError::Overflow)?)
    }

    /// Return the output frame index containing a non-negative project time.
    /// This is floor division, so an exact frame boundary belongs to the new
    /// frame and never to the preceding frame.
    pub fn frame_index_at(self, time: Time) -> Result<u64, RationalError> {
        if time < Time::ZERO {
            return Err(RationalError::InvalidDecimal);
        }
        let numerator = i128::from(time.numerator)
            .checked_mul(i128::from(self.numerator))
            .ok_or(RationalError::Overflow)?;
        let denominator = i128::from(time.denominator)
            .checked_mul(i128::from(self.denominator))
            .ok_or(RationalError::Overflow)?;
        u64::try_from(numerator / denominator).map_err(|_| RationalError::Overflow)
    }
}

fn pow10(power: u32) -> Result<u128, RationalError> {
    let mut result = 1_u128;
    for _ in 0..power {
        result = result.checked_mul(10).ok_or(RationalError::Overflow)?;
    }
    Ok(result)
}

fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left.max(1)
}
