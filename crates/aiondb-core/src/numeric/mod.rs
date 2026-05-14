use std::cmp::Ordering;
use std::fmt;
use std::hash::{Hash, Hasher};

mod big_coefficient;
#[path = "numeric_parse.rs"]
mod parse_support;
pub(crate) use big_coefficient::BigCoefficient;
pub use parse_support::checked_ten_pow;

use crate::convert::u32_to_i32_saturating;
use big_coefficient::{MAX_BIG_LIMBS, SPECIAL_SCALE};

#[inline]
fn i32_to_usize_saturating(value: i32) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

#[inline]
fn approx_f64_from_decimal_parts(abs_digits: &str, scale: u32, negative: bool) -> f64 {
    let trimmed = abs_digits.trim_start_matches('0');
    if trimmed.is_empty() {
        return 0.0;
    }

    let total_digits = i64::try_from(trimmed.len()).unwrap_or(i64::MAX);
    let exp10 = total_digits.saturating_sub(1) - i64::from(scale);

    // Keep enough leading digits so f64 parser rounds to nearest representable value.
    let sig = trimmed.len().min(64);
    let mut mantissa_text = String::with_capacity(sig + 1);
    mantissa_text.push_str(&trimmed[..1]);
    if sig > 1 {
        mantissa_text.push('.');
        mantissa_text.push_str(&trimmed[1..sig]);
    }
    let mantissa = mantissa_text.parse::<f64>().unwrap_or(0.0);

    let value = if !mantissa.is_finite() || exp10 > 308 {
        f64::INFINITY
    } else if exp10 < -324 {
        0.0
    } else {
        let exp = i32::try_from(exp10).unwrap_or(if exp10 < 0 { i32::MIN } else { i32::MAX });
        mantissa * 10f64.powi(exp)
    };

    if negative {
        -value
    } else {
        value
    }
}

const MAX_BIG_DECIMAL_DIGITS: usize = MAX_BIG_LIMBS * 9;
const MAX_NUMERIC_LITERAL_LEN: usize = (MAX_BIG_DECIMAL_DIGITS * 4) + 32;
const DECIMAL_ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

#[inline]
fn exceeds_big_decimal_digits(digits: &str) -> bool {
    digits.trim_start_matches('0').len() > MAX_BIG_DECIMAL_DIGITS
}

/// The public numeric type.  For values fitting in i128 (up to 38 decimal
/// digits), `coefficient` holds the exact value and `big` is `None`.
/// For larger values, `coefficient` is set to 0 and `big` stores the
/// arbitrary-precision representation.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub struct NumericValue {
    pub coefficient: i128,
    pub scale: u32,
    big: Option<Box<BigCoefficient>>,
}

impl PartialEq for NumericValue {
    fn eq(&self, other: &Self) -> bool {
        if self.big.is_none() && other.big.is_none() {
            return self.coefficient == other.coefficient && self.scale == other.scale;
        }
        if self.scale != other.scale {
            return false;
        }
        let a = self.to_big_coefficient();
        let b = other.to_big_coefficient();
        a == b
    }
}

impl Eq for NumericValue {}

impl Hash for NumericValue {
    fn hash<H: Hasher>(&self, state: &mut H) {
        if let Some(ref big) = self.big {
            1u8.hash(state);
            big.hash(state);
        } else {
            0u8.hash(state);
            self.coefficient.hash(state);
        }
        self.scale.hash(state);
    }
}

impl NumericValue {
    #[must_use]
    pub const fn new(coefficient: i128, scale: u32) -> Self {
        Self {
            coefficient,
            scale,
            big: None,
        }
    }

    fn from_big(big: BigCoefficient, scale: u32) -> Self {
        if let Some(v) = big.to_i128() {
            Self {
                coefficient: v,
                scale,
                big: None,
            }
        } else {
            Self {
                coefficient: 0,
                scale,
                big: Some(Box::new(big)),
            }
        }
    }

    /// Returns true if this value uses the big-coefficient path.
    #[must_use]
    pub fn is_big(&self) -> bool {
        self.big.is_some()
    }

    fn to_big_coefficient(&self) -> BigCoefficient {
        if let Some(ref big) = self.big {
            (**big).clone()
        } else {
            BigCoefficient::from_i128(self.coefficient)
        }
    }

    pub const NAN: Self = Self {
        coefficient: 0,
        scale: SPECIAL_SCALE,
        big: None,
    };

    pub const INFINITY: Self = Self {
        coefficient: 1,
        scale: SPECIAL_SCALE,
        big: None,
    };

    pub const NEG_INFINITY: Self = Self {
        coefficient: -1,
        scale: SPECIAL_SCALE,
        big: None,
    };

    #[must_use]
    pub fn is_special(&self) -> bool {
        self.scale == SPECIAL_SCALE && self.big.is_none()
    }

    #[must_use]
    pub fn is_nan(&self) -> bool {
        self.is_special() && self.coefficient == 0
    }

    #[must_use]
    pub fn is_infinite(&self) -> bool {
        self.is_special() && self.coefficient != 0
    }

    #[must_use]
    pub fn is_pos_infinity(&self) -> bool {
        self.is_special() && self.coefficient == 1
    }

    #[must_use]
    pub fn is_neg_infinity(&self) -> bool {
        self.is_special() && self.coefficient == -1
    }

    #[must_use]
    pub fn from_i32(v: i32) -> Self {
        Self::new(i128::from(v), 0)
    }

    #[must_use]
    pub fn from_i64(v: i64) -> Self {
        Self::new(i128::from(v), 0)
    }

    /// Construct from a signed decimal coefficient string and a scale.
    /// Used for WAL deserialization of big numeric values.
    ///
    /// # Errors
    ///
    /// Returns an error when the coefficient string is empty, contains
    /// non-decimal digits, or exceeds the supported precision bound.
    #[allow(clippy::missing_errors_doc)]
    pub fn from_coefficient_string(s: &str, scale: u32) -> Result<Self, String> {
        let s = s.trim();
        if s.is_empty() {
            return Err("empty coefficient string".to_owned());
        }
        // Try i128 first
        if let Ok(v) = s.parse::<i128>() {
            return Ok(Self::new(v, scale));
        }
        let negative = s.starts_with('-');
        let digits = if negative { &s[1..] } else { s };
        if !digits.chars().all(|c| c.is_ascii_digit()) {
            return Err(format!("invalid coefficient: {s}"));
        }
        if exceeds_big_decimal_digits(digits) {
            return Err("coefficient too large".to_owned());
        }
        let big = BigCoefficient::from_decimal_string(digits, negative);
        if big.limbs.len() > MAX_BIG_LIMBS {
            return Err("coefficient too large".to_owned());
        }
        Ok(Self::from_big(big, scale))
    }

    /// Returns true if the value is negative.
    #[must_use]
    pub fn is_negative(&self) -> bool {
        if let Some(ref big) = self.big {
            big.negative
        } else {
            self.coefficient < 0
        }
    }

    /// Returns the coefficient as a signed decimal string.
    #[must_use]
    pub fn coefficient_to_string(&self) -> String {
        if let Some(ref big) = self.big {
            let s = big.to_decimal_string_unsigned();
            if big.negative {
                format!("-{s}")
            } else {
                s
            }
        } else {
            self.coefficient.to_string()
        }
    }

    /// Returns the absolute coefficient as an unsigned decimal string.
    #[must_use]
    pub fn coefficient_abs_string(&self) -> String {
        if let Some(ref big) = self.big {
            big.to_decimal_string_unsigned()
        } else {
            self.coefficient.unsigned_abs().to_string()
        }
    }

    /// Try to get the coefficient as i128. Returns None for big values.
    #[must_use]
    pub fn try_coefficient_i128(&self) -> Option<i128> {
        if let Some(ref big) = self.big {
            big.to_i128()
        } else {
            Some(self.coefficient)
        }
    }

    /// Convert to f64.
    #[must_use]
    pub fn to_f64(&self) -> f64 {
        if self.is_nan() {
            return f64::NAN;
        }
        if self.is_pos_infinity() {
            return f64::INFINITY;
        }
        if self.is_neg_infinity() {
            return f64::NEG_INFINITY;
        }
        let abs_digits = if self.big.is_some() {
            self.coefficient_abs_string()
        } else {
            self.coefficient.unsigned_abs().to_string()
        };
        approx_f64_from_decimal_parts(&abs_digits, self.scale, self.is_negative())
    }

    /// Add two numeric values, aligning scales.
    #[must_use]
    pub fn add(&self, other: &Self) -> Self {
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        if self.is_infinite() || other.is_infinite() {
            match (self.is_infinite(), other.is_infinite()) {
                (true, true) => {
                    if self.coefficient == other.coefficient {
                        return self.clone();
                    }
                    return Self::NAN;
                }
                (true, false) => return self.clone(),
                (false, true) => return other.clone(),
                (false, false) => {}
            }
        }
        // Fast path: both small
        if self.big.is_none() && other.big.is_none() {
            if let Some((a, b, scale)) = Self::align_small(self, other) {
                if let Some(sum) = a.checked_add(b) {
                    return Self::new(sum, scale);
                }
            }
        }
        // Big path
        let scale = self.scale.max(other.scale);
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let Some(a_scaled) = (if scale > self.scale {
            a_big.mul_pow10(scale - self.scale)
        } else {
            Some(a_big)
        }) else {
            return Self::NAN;
        };
        let Some(b_scaled) = (if scale > other.scale {
            b_big.mul_pow10(scale - other.scale)
        } else {
            Some(b_big)
        }) else {
            return Self::NAN;
        };
        let result = a_scaled.add(&b_scaled);
        Self::from_big(result, scale)
    }

    /// Subtract another numeric value, aligning scales.
    #[must_use]
    pub fn sub(&self, other: &Self) -> Self {
        if self.is_nan() || other.is_nan() {
            return Self::NAN;
        }
        if self.is_infinite() || other.is_infinite() {
            match (self.is_infinite(), other.is_infinite()) {
                (true, true) => {
                    if self.coefficient != other.coefficient {
                        return self.clone();
                    }
                    return Self::NAN;
                }
                (true, false) => return self.clone(),
                (false, true) => return other.neg(),
                (false, false) => {}
            }
        }
        // Fast path
        if self.big.is_none() && other.big.is_none() {
            if let Some((a, b, scale)) = Self::align_small(self, other) {
                if let Some(diff) = a.checked_sub(b) {
                    return Self::new(diff, scale);
                }
            }
        }
        // Big path
        let scale = self.scale.max(other.scale);
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let Some(a_scaled) = (if scale > self.scale {
            a_big.mul_pow10(scale - self.scale)
        } else {
            Some(a_big)
        }) else {
            return Self::NAN;
        };
        let Some(b_scaled) = (if scale > other.scale {
            b_big.mul_pow10(scale - other.scale)
        } else {
            Some(b_big)
        }) else {
            return Self::NAN;
        };
        let result = a_scaled.sub(&b_scaled);
        Self::from_big(result, scale)
    }

    /// Multiply two numeric values.
    #[must_use]
    pub fn mul(&self, other: &Self) -> Option<Self> {
        if self.is_nan() || other.is_nan() {
            return Some(Self::NAN);
        }
        if self.is_infinite() || other.is_infinite() {
            let (inf, finite) = if self.is_infinite() {
                (self, other)
            } else {
                (other, self)
            };
            if finite.is_zero() && !finite.is_infinite() {
                return Some(Self::NAN);
            }
            let inf_neg = inf.coefficient < 0;
            let finite_neg = finite.is_negative();
            let sign = if inf_neg ^ finite_neg { -1 } else { 1 };
            return Some(Self::new(sign, SPECIAL_SCALE));
        }
        let new_scale = self.scale.checked_add(other.scale)?;
        // Fast path
        if self.big.is_none() && other.big.is_none() {
            if let Some(product) = self.coefficient.checked_mul(other.coefficient) {
                return Some(Self::new(product, new_scale));
            }
            // Fall through to the big-coefficient path when i128 overflows.
        }
        // Big path (at least one operand already uses BigCoefficient)
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let result = a_big.mul(&b_big)?;
        Some(Self::from_big(result, new_scale))
    }

    /// Divide, returning `None` on division by zero.
    #[must_use]
    pub fn div(&self, other: &Self) -> Option<Self> {
        if self.is_nan() || other.is_nan() {
            return Some(Self::NAN);
        }
        if self.is_infinite() {
            if other.is_infinite() {
                return Some(Self::NAN);
            }
            if other.is_zero() {
                return None;
            }
            let other_neg = other.is_negative();
            let sign = if (self.coefficient < 0) ^ other_neg {
                -1
            } else {
                1
            };
            return Some(Self::new(sign, SPECIAL_SCALE));
        }
        if other.is_infinite() {
            return Some(Self::new(0, 0));
        }
        if other.is_zero() {
            return None;
        }
        let result_scale = self.pg_select_div_scale(other);
        let scale_up = result_scale
            .saturating_add(other.scale)
            .saturating_sub(self.scale);
        let scale_up_r = scale_up.saturating_add(1);

        // Fast path
        if self.big.is_none() && other.big.is_none() {
            if let Some(factor) = checked_ten_pow(scale_up_r) {
                if let Some(scaled_dividend) = self.coefficient.checked_mul(factor) {
                    let raw = scaled_dividend / other.coefficient;
                    let last = (raw % 10).abs();
                    let mut coefficient = raw / 10;
                    if last >= 5 {
                        if coefficient >= 0 {
                            coefficient += 1;
                        } else {
                            coefficient -= 1;
                        }
                    }
                    return Some(Self::new(coefficient, result_scale));
                }
            }
        }

        // Big path
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let a_scaled = a_big.mul_pow10(scale_up_r)?;
        let (raw_q, _) = a_scaled.div_rem(&b_big)?;

        let ten = BigCoefficient::from_i128(10);
        let (q_div10, r_big) = raw_q.abs().div_rem(&ten)?;
        let r_val = r_big.to_i128().unwrap_or(0).unsigned_abs();
        let mut result_big = if r_val >= 5 {
            q_div10.add(&BigCoefficient::from_i128(1))
        } else {
            q_div10
        };
        result_big.negative = raw_q.negative;
        if result_big.is_zero() {
            result_big.negative = false;
        }
        Some(Self::from_big(result_big, result_scale))
    }

    /// Integer-truncated division at scale 0.
    #[must_use]
    pub fn div_trunc_int(&self, other: &Self) -> Option<Self> {
        if self.is_nan() || other.is_nan() {
            return Some(Self::NAN);
        }
        if self.is_infinite() {
            if other.is_infinite() {
                return Some(Self::NAN);
            }
            if other.is_zero() {
                return None;
            }
            return None;
        }
        if other.is_infinite() {
            return Some(Self::new(0, 0));
        }
        if other.is_zero() {
            return None;
        }
        // Fast path
        if self.big.is_none() && other.big.is_none() {
            let result = if other.scale >= self.scale {
                let diff = other.scale - self.scale;
                checked_ten_pow(diff)
                    .and_then(|factor| self.coefficient.checked_mul(factor))
                    .map(|scaled| scaled / other.coefficient)
            } else {
                let diff = self.scale - other.scale;
                checked_ten_pow(diff)
                    .and_then(|factor| other.coefficient.checked_mul(factor))
                    .map(|scaled_other| self.coefficient / scaled_other)
            };
            if let Some(quotient) = result {
                return Some(Self::new(quotient, 0));
            }
            // Fall through to big path
        }
        // Big path
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let a_scaled = a_big.mul_pow10(other.scale)?;
        let b_scaled = b_big.mul_pow10(self.scale)?;
        let (quotient, _) = a_scaled.div_rem(&b_scaled)?;
        Some(Self::from_big(quotient, 0))
    }

    /// Division with a specified result scale.
    #[must_use]
    pub fn div_with_scale(&self, other: &Self, result_scale: u32) -> Option<Self> {
        if self.is_nan() || other.is_nan() {
            return Some(Self::NAN);
        }
        if self.is_infinite() {
            if other.is_infinite() {
                return Some(Self::NAN);
            }
            if other.is_zero() {
                return None;
            }
            let other_neg = other.is_negative();
            let sign = if (self.coefficient < 0) ^ other_neg {
                -1
            } else {
                1
            };
            return Some(Self::new(sign, SPECIAL_SCALE));
        }
        if other.is_infinite() {
            return Some(Self::new(0, 0));
        }
        if other.is_zero() {
            return None;
        }
        // Fast path
        if self.big.is_none() && other.big.is_none() {
            let scale_up = result_scale
                .saturating_add(other.scale)
                .saturating_sub(self.scale.min(result_scale));
            if let Some(factor) = checked_ten_pow(scale_up) {
                if let Some(scaled_dividend) = self.coefficient.checked_mul(factor) {
                    let coefficient = scaled_dividend / other.coefficient;
                    return Some(Self::new(coefficient, result_scale));
                }
            }
        }
        // Big path
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let scale_up = result_scale
            .saturating_add(other.scale)
            .saturating_sub(self.scale.min(result_scale));
        let a_scaled = a_big.mul_pow10(scale_up)?;
        let (quotient, _) = a_scaled.div_rem(&b_big)?;
        Some(Self::from_big(quotient, result_scale))
    }

    fn pg_select_div_scale(&self, other: &Self) -> u32 {
        const NUMERIC_MIN_SIG_DIGITS: i32 = 8;
        const DEC_DIGITS: i32 = 4;
        const NUMERIC_MIN_DISPLAY_SCALE: i32 = 0;
        const NUMERIC_MAX_DISPLAY_SCALE: i32 = 1000;

        let dscale1 = u32_to_i32_saturating(self.scale);
        let dscale2 = u32_to_i32_saturating(other.scale);

        let (weight1, firstdigit1) = self.pg_weight_and_first_digit();
        let (weight2, firstdigit2) = other.pg_weight_and_first_digit();

        let mut qweight = weight1 - weight2;
        if firstdigit1 <= firstdigit2 {
            qweight -= 1;
        }

        let mut rscale = NUMERIC_MIN_SIG_DIGITS - qweight * DEC_DIGITS;
        rscale = rscale.max(dscale1);
        rscale = rscale.max(dscale2);
        rscale = rscale.max(NUMERIC_MIN_DISPLAY_SCALE);
        rscale = rscale.min(NUMERIC_MAX_DISPLAY_SCALE);
        u32::try_from(rscale).unwrap_or(0)
    }

    fn pg_weight_and_first_digit(&self) -> (i32, i32) {
        const DEC_DIGITS: i32 = 4;

        let is_zero_val = if let Some(ref big) = self.big {
            big.is_zero()
        } else {
            self.coefficient == 0
        };
        if is_zero_val {
            return (0, 0);
        }

        let abs_coeff_str = self.coefficient_abs_string();
        let coeff_digits = i32::try_from(abs_coeff_str.len()).unwrap_or(i32::MAX);
        let int_digits = coeff_digits - u32_to_i32_saturating(self.scale);

        let weight = if int_digits > 0 {
            (int_digits - 1) / DEC_DIGITS
        } else {
            (int_digits - 1).div_euclid(DEC_DIGITS)
        };

        let leading_len = int_digits - weight * DEC_DIGITS;
        let shift = coeff_digits - leading_len;

        let first_digit = if shift >= 0 && i32_to_usize_saturating(shift) < abs_coeff_str.len() {
            // Extract the first `leading_len` digits
            let shift_usize = i32_to_usize_saturating(shift);
            let take = i32_to_usize_saturating(leading_len).min(abs_coeff_str.len() - shift_usize);
            if take > 0 {
                abs_coeff_str[..(abs_coeff_str.len() - shift_usize)]
                    .get(..take)
                    .and_then(|s| s.parse::<i32>().ok())
                    .unwrap_or(0)
            } else {
                0
            }
        } else if shift < 0 {
            let take =
                i32_to_usize_saturating(leading_len).min(i32_to_usize_saturating(coeff_digits));
            if take > 0 {
                let base: i32 = abs_coeff_str[..take].parse().unwrap_or(0);
                let mul_exp = u32::try_from((-shift).max(0)).unwrap_or(u32::MAX).min(9);
                base.saturating_mul(10i32.pow(mul_exp))
            } else {
                0
            }
        } else {
            0
        };

        (weight, first_digit)
    }

    /// Negate the value.
    #[must_use]
    pub fn neg(&self) -> Self {
        if self.is_nan() {
            return Self::NAN;
        }
        if let Some(ref big) = self.big {
            Self::from_big(big.neg(), self.scale)
        } else if let Some(coefficient) = self.coefficient.checked_neg() {
            Self::new(coefficient, self.scale)
        } else {
            let big = BigCoefficient::from_i128(self.coefficient);
            Self::from_big(big.neg(), self.scale)
        }
    }

    /// Absolute value.
    #[must_use]
    pub fn abs(&self) -> Self {
        if self.is_nan() {
            return Self::NAN;
        }
        if self.is_neg_infinity() {
            return Self::INFINITY;
        }
        if let Some(ref big) = self.big {
            Self::from_big(big.abs(), self.scale)
        } else if let Some(coefficient) = self.coefficient.checked_abs() {
            Self::new(coefficient, self.scale)
        } else {
            let big = BigCoefficient::from_i128(self.coefficient);
            Self::from_big(big.abs(), self.scale)
        }
    }

    /// Check if the value is zero.
    #[must_use]
    pub fn is_zero(&self) -> bool {
        if self.is_special() {
            return false;
        }
        if let Some(ref big) = self.big {
            big.is_zero()
        } else {
            self.coefficient == 0
        }
    }

    /// Round to the given number of decimal places.
    #[must_use]
    pub fn round(&self, target_scale: u32) -> Self {
        if self.is_special() {
            return self.clone();
        }
        if self.is_zero() {
            return Self::new(0, target_scale);
        }
        if target_scale >= self.scale {
            let diff = target_scale - self.scale;
            if let Some(ref big) = self.big {
                if let Some(scaled) = big.mul_pow10(diff) {
                    return Self::from_big(scaled, target_scale);
                }
                return Self::NAN;
            }
            let Some(factor) = checked_ten_pow(diff) else {
                return Self::NAN;
            };
            let Some(coefficient) = self.coefficient.checked_mul(factor) else {
                // Overflow: promote to big
                let big = self.to_big_coefficient();
                if let Some(scaled) = big.mul_pow10(diff) {
                    return Self::from_big(scaled, target_scale);
                }
                return Self::NAN;
            };
            return Self::new(coefficient, target_scale);
        }
        let diff = self.scale - target_scale;

        // Big path for large diff or big values
        if self.big.is_some() || diff > 38 {
            let big = self.to_big_coefficient();
            let Some(divisor_big) = BigCoefficient::from_i128(1).mul_pow10(diff) else {
                return Self::new(0, target_scale);
            };
            let Some((quotient, remainder)) = big.abs().div_rem(&divisor_big) else {
                return Self::new(0, target_scale);
            };
            let half_divisor = divisor_big
                .div_u64(2)
                .map_or_else(BigCoefficient::zero, |(h, _)| h);
            let needs_round = BigCoefficient::cmp_magnitudes(&remainder.limbs, &half_divisor.limbs)
                != Ordering::Less;
            let mut result = if needs_round {
                quotient.add(&BigCoefficient::from_i128(1))
            } else {
                quotient
            };
            result.negative = big.negative;
            if result.is_zero() {
                result.negative = false;
            }
            return Self::from_big(result, target_scale);
        }

        let Some(divisor) = checked_ten_pow(diff) else {
            return Self::new(0, target_scale);
        };
        let remainder = self.coefficient % divisor;
        let half = divisor / 2;
        let mut result = self.coefficient / divisor;
        if self.coefficient >= 0 {
            if remainder >= half {
                result = result.saturating_add(1);
            }
        } else if remainder.checked_neg().unwrap_or(i128::MAX) >= half {
            result = result.saturating_sub(1);
        }
        Self::new(result, target_scale)
    }

    /// Round to a negative number of decimal places.
    #[must_use]
    pub fn round_neg(&self, neg_scale: u32) -> Self {
        if self.is_special() || self.is_zero() {
            return self.clone();
        }
        let int_val = self.round(0);
        let Some(divisor) = checked_ten_pow(neg_scale) else {
            return Self::new(0, 0);
        };
        if int_val.big.is_some() {
            let big = int_val.to_big_coefficient();
            let divisor_big = BigCoefficient::from_i128(divisor);
            let Some((quotient, remainder)) = big.abs().div_rem(&divisor_big) else {
                return Self::new(0, 0);
            };
            let half_divisor = divisor_big
                .div_u64(2)
                .map_or_else(BigCoefficient::zero, |(h, _)| h);
            let needs_round = BigCoefficient::cmp_magnitudes(&remainder.limbs, &half_divisor.limbs)
                != Ordering::Less;
            let rounded = if needs_round {
                quotient.add(&BigCoefficient::from_i128(1))
            } else {
                quotient
            };
            if let Some(result) = rounded.mul(&divisor_big) {
                let mut final_result = result;
                final_result.negative = big.negative;
                return Self::from_big(final_result, 0);
            }
            return Self::NAN;
        }
        let remainder = int_val.coefficient % divisor;
        let half = divisor / 2;
        let mut result = int_val.coefficient / divisor;
        if int_val.coefficient >= 0 {
            if remainder >= half {
                result = result.saturating_add(1);
            }
        } else if remainder.checked_neg().unwrap_or(i128::MAX) >= half {
            result = result.saturating_sub(1);
        }
        match result.checked_mul(divisor) {
            Some(coefficient) => Self::new(coefficient, 0),
            None => Self::NAN,
        }
    }

    /// Truncate to the given number of decimal places.
    #[must_use]
    pub fn trunc(&self, target_scale: u32) -> Self {
        if self.is_special() {
            return self.clone();
        }
        if self.is_zero() {
            return Self::new(0, target_scale);
        }
        if target_scale >= self.scale {
            let diff = target_scale - self.scale;
            if let Some(ref big) = self.big {
                if let Some(scaled) = big.mul_pow10(diff) {
                    return Self::from_big(scaled, target_scale);
                }
                return Self::NAN;
            }
            let Some(factor) = checked_ten_pow(diff) else {
                return Self::NAN;
            };
            let Some(coefficient) = self.coefficient.checked_mul(factor) else {
                let big = self.to_big_coefficient();
                if let Some(scaled) = big.mul_pow10(diff) {
                    return Self::from_big(scaled, target_scale);
                }
                return Self::NAN;
            };
            return Self::new(coefficient, target_scale);
        }
        let diff = self.scale - target_scale;
        if self.big.is_some() || diff > 38 {
            let big = self.to_big_coefficient();
            let Some(divisor_big) = BigCoefficient::from_i128(1).mul_pow10(diff) else {
                return Self::new(0, target_scale);
            };
            let Some((mut quotient, _)) = big.abs().div_rem(&divisor_big) else {
                return Self::new(0, target_scale);
            };
            quotient.negative = big.negative;
            if quotient.is_zero() {
                quotient.negative = false;
            }
            return Self::from_big(quotient, target_scale);
        }
        let Some(divisor) = checked_ten_pow(diff) else {
            return Self::new(0, target_scale);
        };
        Self::new(self.coefficient / divisor, target_scale)
    }

    fn align_small(a: &Self, b: &Self) -> Option<(i128, i128, u32)> {
        let scale = a.scale.max(b.scale);
        let ca = a
            .coefficient
            .checked_mul(checked_ten_pow(scale - a.scale)?)?;
        let cb = b
            .coefficient
            .checked_mul(checked_ten_pow(scale - b.scale)?)?;
        Some((ca, cb, scale))
    }

    fn align(a: &Self, b: &Self) -> Option<(i128, i128, u32)> {
        debug_assert!(
            !a.is_special() && !b.is_special(),
            "align() must not be called with special values (NaN/Infinity)"
        );
        if a.big.is_some() || b.big.is_some() {
            return None;
        }
        let scale = a.scale.max(b.scale);
        let ca = a
            .coefficient
            .checked_mul(checked_ten_pow(scale - a.scale)?)?;
        let cb = b
            .coefficient
            .checked_mul(checked_ten_pow(scale - b.scale)?)?;
        Some((ca, cb, scale))
    }

    fn compare_aligned(&self, other: &Self) -> Ordering {
        if self.big.is_none() && other.big.is_none() {
            if let Some((a, b, _)) = Self::align(self, other) {
                return a.cmp(&b);
            }
        }
        self.compare_big(other)
    }

    fn compare_big(&self, other: &Self) -> Ordering {
        let a_big = self.to_big_coefficient();
        let b_big = other.to_big_coefficient();
        let scale = self.scale.max(other.scale);
        let a_scaled = if scale > self.scale {
            a_big.mul_pow10(scale - self.scale).unwrap_or(a_big)
        } else {
            a_big
        };
        let b_scaled = if scale > other.scale {
            b_big.mul_pow10(scale - other.scale).unwrap_or(b_big)
        } else {
            b_big
        };
        a_scaled.cmp_signed(&b_scaled)
    }
}

impl PartialOrd for NumericValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for NumericValue {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.is_nan() && other.is_nan() {
            return Ordering::Equal;
        }
        if self.is_nan() {
            return Ordering::Greater;
        }
        if other.is_nan() {
            return Ordering::Less;
        }
        if self.is_special() || other.is_special() {
            let self_rank = if self.is_pos_infinity() {
                2
            } else if self.is_neg_infinity() {
                -2
            } else if self.is_negative() {
                -1
            } else {
                i32::from(!self.is_zero())
            };
            let other_rank = if other.is_pos_infinity() {
                2
            } else if other.is_neg_infinity() {
                -2
            } else if other.is_negative() {
                -1
            } else {
                i32::from(!other.is_zero())
            };
            return self_rank.cmp(&other_rank);
        }
        self.compare_aligned(other)
    }
}

impl fmt::Display for NumericValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_nan() {
            return f.write_str("NaN");
        }
        if self.is_pos_infinity() {
            return f.write_str("Infinity");
        }
        if self.is_neg_infinity() {
            return f.write_str("-Infinity");
        }
        if self.scale == 0 {
            if let Some(ref big) = self.big {
                if big.negative {
                    f.write_str("-")?;
                }
                return f.write_str(&big.to_decimal_string_unsigned());
            }
            return write!(f, "{}", self.coefficient);
        }

        // 64-byte ASCII '0' run reused for emitting the leading zeros of
        // small-magnitude numerics (`coefficient < 10^scale`). Mirrors the
        // iter160 chunked emit in pgwire's `write_decimal_str_into`.
        let (is_negative, abs_str) = if let Some(ref big) = self.big {
            (big.negative, big.to_decimal_string_unsigned())
        } else {
            let raw = self.coefficient.to_string();
            if let Some(rest) = raw.strip_prefix('-') {
                (true, rest.to_owned())
            } else {
                (false, raw)
            }
        };

        let scale = usize::try_from(self.scale)
            .unwrap_or(usize::MAX)
            .min(10_000);
        if is_negative {
            f.write_str("-")?;
        }
        if abs_str.len() <= scale {
            f.write_str("0.")?;
            // Chunked zero emission - for high-scale numerics
            // (`NUMERIC(20,18)` and similar) this collapses up to ~18
            // single-byte `write_str("0")` vtable hops into one or two
            // `write_str(64-byte slice)` calls.
            let mut remaining = scale - abs_str.len();
            while remaining > 0 {
                let chunk = remaining.min(DECIMAL_ZEROS.len());
                f.write_str(&DECIMAL_ZEROS[..chunk])?;
                remaining -= chunk;
            }
            f.write_str(&abs_str)
        } else {
            let split = abs_str.len() - scale;
            f.write_str(&abs_str[..split])?;
            f.write_str(".")?;
            f.write_str(&abs_str[split..])
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct IntervalValue {
    pub months: i32,
    pub days: i32,
    pub micros: i64,
}

impl IntervalValue {
    #[must_use]
    pub const fn new(months: i32, days: i32, micros: i64) -> Self {
        Self {
            months,
            days,
            micros,
        }
    }
}

impl fmt::Display for IntervalValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut need_space = false;

        if self.months != 0 {
            let years = self.months / 12;
            let mons = self.months % 12;
            if years != 0 {
                write!(f, "{years} {}", if years == 1 { "year" } else { "years" })?;
                need_space = true;
            }
            if mons != 0 {
                if need_space {
                    f.write_str(" ")?;
                }
                write!(f, "{mons} {}", if mons == 1 { "mon" } else { "mons" })?;
                need_space = true;
            }
        }

        if self.days != 0 {
            if need_space {
                f.write_str(" ")?;
            }
            write!(
                f,
                "{} {}",
                self.days,
                if self.days == 1 { "day" } else { "days" }
            )?;
            need_space = true;
        }

        if self.micros != 0 || !need_space {
            if need_space {
                f.write_str(" ")?;
            }
            let has_negative_date_part = self.months < 0 || self.days < 0;
            let sign = if self.micros < 0 {
                "-"
            } else if need_space && has_negative_date_part {
                "+"
            } else {
                ""
            };
            let abs_micros = self.micros.unsigned_abs();
            let total_secs = abs_micros / 1_000_000;
            let frac_micros = abs_micros % 1_000_000;
            let hours = total_secs / 3600;
            let mins = (total_secs % 3600) / 60;
            let secs = total_secs % 60;

            if frac_micros == 0 {
                write!(f, "{sign}{hours:02}:{mins:02}:{secs:02}")?;
            } else {
                let mut buf = [b'0'; 6];
                let mut v = u32::try_from(frac_micros).unwrap_or(u32::MAX);
                for b in buf.iter_mut().rev() {
                    *b = b'0' + u8::try_from(v % 10).unwrap_or(0);
                    v /= 10;
                }
                let trimmed_len = buf.iter().rposition(|&b| b != b'0').map_or(0, |i| i + 1);
                let Ok(trimmed) = std::str::from_utf8(&buf[..trimmed_len]) else {
                    return Err(std::fmt::Error);
                };
                write!(f, "{sign}{hours:02}:{mins:02}:{secs:02}.{trimmed}")?;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
