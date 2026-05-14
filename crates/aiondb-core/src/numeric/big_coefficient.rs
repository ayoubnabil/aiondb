use std::cmp::Ordering;
use std::fmt::Write as _;
use std::hash::{Hash, Hasher};

pub(super) const MAX_BIG_LIMBS: usize = 320;
pub(super) const LIMB_BASE: u64 = 1_000_000_000;

#[derive(Clone, Debug, serde::Serialize)]
pub(crate) struct BigCoefficient {
    pub(crate) limbs: Vec<u32>,
    pub(crate) negative: bool,
}

impl<'de> serde::Deserialize<'de> for BigCoefficient {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        // Reject payloads claiming more limbs than the parser would ever
        // produce. Without this cap a malicious snapshot/WAL frame could
        // request gigabytes of allocation via `Vec<u32>`.
        #[derive(serde::Deserialize)]
        struct Helper {
            limbs: Vec<u32>,
            negative: bool,
        }
        let h = Helper::deserialize(deserializer)?;
        if h.limbs.len() > MAX_BIG_LIMBS {
            return Err(<D::Error as serde::de::Error>::custom(format!(
                "big coefficient has {} limbs, exceeds MAX_BIG_LIMBS = {}",
                h.limbs.len(),
                MAX_BIG_LIMBS
            )));
        }
        Ok(Self {
            limbs: h.limbs,
            negative: h.negative,
        })
    }
}

impl BigCoefficient {
    pub(crate) fn zero() -> Self {
        Self {
            limbs: Vec::new(),
            negative: false,
        }
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.limbs.is_empty()
    }

    pub(crate) fn from_i128(v: i128) -> Self {
        if v == 0 {
            return Self::zero();
        }
        let negative = v < 0;
        let mut abs = v.unsigned_abs();
        let mut limbs = Vec::new();
        while abs > 0 {
            limbs.push(u32::try_from(abs % u128::from(LIMB_BASE)).unwrap_or(u32::MAX));
            abs /= u128::from(LIMB_BASE);
        }
        Self { limbs, negative }
    }

    pub(crate) fn to_i128(&self) -> Option<i128> {
        if self.limbs.is_empty() {
            return Some(0);
        }
        let mut result: i128 = 0;
        let mut base_power: i128 = 1;
        for (i, &limb) in self.limbs.iter().enumerate() {
            if i > 0 {
                base_power = base_power.checked_mul(i128::from(LIMB_BASE))?;
            }
            result = result.checked_add(base_power.checked_mul(i128::from(limb))?)?;
        }
        if self.negative {
            result = result.checked_neg()?;
        }
        Some(result)
    }

    pub(crate) fn normalize(&mut self) {
        while self.limbs.last() == Some(&0) {
            self.limbs.pop();
        }
        if self.limbs.is_empty() {
            self.negative = false;
        }
    }

    pub(crate) fn add(&self, other: &Self) -> Self {
        if self.negative == other.negative {
            let mut result = Self::add_magnitudes(&self.limbs, &other.limbs);
            result.negative = self.negative;
            result
        } else {
            let ord = Self::cmp_magnitudes(&self.limbs, &other.limbs);
            match ord {
                Ordering::Equal => Self::zero(),
                Ordering::Greater => {
                    let mut result = Self::sub_magnitudes(&self.limbs, &other.limbs);
                    result.negative = self.negative;
                    result
                }
                Ordering::Less => {
                    let mut result = Self::sub_magnitudes(&other.limbs, &self.limbs);
                    result.negative = other.negative;
                    result
                }
            }
        }
    }

    pub(crate) fn sub(&self, other: &Self) -> Self {
        let neg_other = Self {
            limbs: other.limbs.clone(),
            negative: if other.is_zero() {
                false
            } else {
                !other.negative
            },
        };
        self.add(&neg_other)
    }

    pub(crate) fn mul(&self, other: &Self) -> Option<Self> {
        if self.is_zero() || other.is_zero() {
            return Some(Self::zero());
        }
        let n = self.limbs.len();
        let m = other.limbs.len();
        if n + m > MAX_BIG_LIMBS + 1 {
            return None;
        }
        let mut result_limbs = vec![0u64; n + m];
        for i in 0..n {
            let mut carry: u64 = 0;
            for j in 0..m {
                let prod = u64::from(self.limbs[i]) * u64::from(other.limbs[j])
                    + result_limbs[i + j]
                    + carry;
                result_limbs[i + j] = prod % LIMB_BASE;
                carry = prod / LIMB_BASE;
            }
            if carry > 0 {
                result_limbs[i + m] += carry;
            }
        }
        for k in 0..result_limbs.len() - 1 {
            if result_limbs[k] >= LIMB_BASE {
                result_limbs[k + 1] += result_limbs[k] / LIMB_BASE;
                result_limbs[k] %= LIMB_BASE;
            }
        }
        let limbs: Vec<u32> = result_limbs
            .iter()
            .map(|&x| u32::try_from(x).unwrap_or(u32::MAX))
            .collect();
        let mut result = Self {
            limbs,
            negative: self.negative != other.negative,
        };
        result.normalize();
        if result.limbs.len() > MAX_BIG_LIMBS {
            return None;
        }
        Some(result)
    }

    pub(crate) fn div_rem(&self, other: &Self) -> Option<(Self, Self)> {
        if other.is_zero() {
            return None;
        }
        if self.is_zero() {
            return Some((Self::zero(), Self::zero()));
        }
        let ord = Self::cmp_magnitudes(&self.limbs, &other.limbs);
        if ord == Ordering::Less {
            return Some((
                Self::zero(),
                Self {
                    limbs: self.limbs.clone(),
                    negative: self.negative,
                },
            ));
        }
        if ord == Ordering::Equal {
            return Some((
                Self {
                    limbs: vec![1],
                    negative: self.negative != other.negative,
                },
                Self::zero(),
            ));
        }
        let self_str = self.to_decimal_string_unsigned();
        let other_str = other.to_decimal_string_unsigned();
        let (q_str, r_str) = big_decimal_div_rem(&self_str, &other_str);
        let quotient = Self::from_decimal_string(&q_str, self.negative != other.negative);
        let remainder = Self::from_decimal_string(&r_str, self.negative);
        Some((quotient, remainder))
    }

    pub(crate) fn neg(&self) -> Self {
        if self.is_zero() {
            return Self::zero();
        }
        Self {
            limbs: self.limbs.clone(),
            negative: !self.negative,
        }
    }

    pub(crate) fn abs(&self) -> Self {
        Self {
            limbs: self.limbs.clone(),
            negative: false,
        }
    }

    pub(crate) fn cmp_magnitudes(a: &[u32], b: &[u32]) -> Ordering {
        if a.len() != b.len() {
            return a.len().cmp(&b.len());
        }
        for i in (0..a.len()).rev() {
            if a[i] != b[i] {
                return a[i].cmp(&b[i]);
            }
        }
        Ordering::Equal
    }

    pub(crate) fn cmp_signed(&self, other: &Self) -> Ordering {
        if self.is_zero() && other.is_zero() {
            return Ordering::Equal;
        }
        if self.is_zero() {
            return if other.negative {
                Ordering::Greater
            } else {
                Ordering::Less
            };
        }
        if other.is_zero() {
            return if self.negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        if self.negative && !other.negative {
            return Ordering::Less;
        }
        if !self.negative && other.negative {
            return Ordering::Greater;
        }
        let mag_ord = Self::cmp_magnitudes(&self.limbs, &other.limbs);
        if self.negative {
            mag_ord.reverse()
        } else {
            mag_ord
        }
    }

    pub(crate) fn add_magnitudes(a: &[u32], b: &[u32]) -> Self {
        let max_len = a.len().max(b.len());
        let mut limbs = Vec::with_capacity(max_len + 1);
        let mut carry: u64 = 0;
        for i in 0..max_len {
            let av = if i < a.len() { u64::from(a[i]) } else { 0 };
            let bv = if i < b.len() { u64::from(b[i]) } else { 0 };
            let sum = av + bv + carry;
            limbs.push(u32::try_from(sum % LIMB_BASE).unwrap_or(u32::MAX));
            carry = sum / LIMB_BASE;
        }
        if carry > 0 {
            limbs.push(u32::try_from(carry).unwrap_or(u32::MAX));
        }
        let mut result = Self {
            limbs,
            negative: false,
        };
        result.normalize();
        result
    }

    pub(crate) fn sub_magnitudes(a: &[u32], b: &[u32]) -> Self {
        let mut limbs = Vec::with_capacity(a.len());
        let mut borrow: i64 = 0;
        for i in 0..a.len() {
            let av = i64::from(a[i]);
            let bv = if i < b.len() { i64::from(b[i]) } else { 0 };
            let mut diff = av - bv - borrow;
            if diff < 0 {
                diff += i64::try_from(LIMB_BASE).unwrap_or(i64::MAX);
                borrow = 1;
            } else {
                borrow = 0;
            }
            limbs.push(u32::try_from(diff).unwrap_or(0));
        }
        let mut result = Self {
            limbs,
            negative: false,
        };
        result.normalize();
        result
    }

    pub(crate) fn mul_u64(&self, factor: u64) -> Option<Self> {
        if factor == 0 || self.is_zero() {
            return Some(Self::zero());
        }
        if factor == 1 {
            return Some(self.clone());
        }
        let mut limbs = Vec::with_capacity(self.limbs.len() + 2);
        let mut carry: u128 = 0;
        let base = u128::from(LIMB_BASE);
        let factor = u128::from(factor);
        for &limb in &self.limbs {
            let prod = u128::from(limb) * factor + carry;
            limbs.push(u32::try_from(prod % base).unwrap_or(u32::MAX));
            carry = prod / base;
        }
        while carry > 0 {
            limbs.push(u32::try_from(carry % base).unwrap_or(u32::MAX));
            carry /= base;
        }
        let mut result = Self {
            limbs,
            negative: self.negative,
        };
        result.normalize();
        if result.limbs.len() > MAX_BIG_LIMBS {
            return None;
        }
        Some(result)
    }

    pub(crate) fn div_u64(&self, divisor: u64) -> Option<(Self, u64)> {
        if divisor == 0 {
            return None;
        }
        if self.is_zero() {
            return Some((Self::zero(), 0));
        }
        let mut result_limbs = vec![0u32; self.limbs.len()];
        let mut remainder: u64 = 0;
        for i in (0..self.limbs.len()).rev() {
            let cur = remainder * LIMB_BASE + u64::from(self.limbs[i]);
            result_limbs[i] = u32::try_from(cur / divisor).unwrap_or(u32::MAX);
            remainder = cur % divisor;
        }
        let mut quotient = Self {
            limbs: result_limbs,
            negative: self.negative,
        };
        quotient.normalize();
        Some((quotient, remainder))
    }

    pub(crate) fn to_decimal_string_unsigned(&self) -> String {
        if self.is_zero() {
            return "0".to_owned();
        }
        // Divide the magnitude by 10^9 repeatedly. A previous
        // implementation called `div_u64` in a loop, which allocated a
        // fresh `Vec<u32>` for the quotient on every iteration. Instead,
        // clone the limbs once into a single working buffer and divide in
        // place - exactly one Vec allocation regardless of coefficient size.
        let mut working = self.limbs.clone();
        // Output base-10^9 limbs in least-significant-first order.
        let mut limbs_out: Vec<u64> = Vec::with_capacity(working.len());
        loop {
            // div_u64 step on `working`: walk from MSB to LSB,
            // accumulating remainder. After this loop `working` holds
            // the quotient and `remainder` is what was left.
            let mut remainder: u64 = 0;
            for i in (0..working.len()).rev() {
                let cur = remainder * LIMB_BASE + u64::from(working[i]);
                working[i] = u32::try_from(cur / LIMB_BASE).unwrap_or(u32::MAX);
                remainder = cur % LIMB_BASE;
            }
            // Trim trailing-zero limbs (the in-place equivalent of
            // `BigCoefficient::normalize`'s leading-zero strip from the
            // most-significant end of the limbs vec).
            while working.last() == Some(&0) {
                working.pop();
            }
            limbs_out.push(remainder);
            if working.is_empty() {
                break;
            }
        }
        let mut result = String::with_capacity(limbs_out.len() * 9);
        // Emit most-significant limb first (no leading zeros) then the
        // rest zero-padded to 9 digits.
        let mut iter = limbs_out.iter().rev();
        if let Some(&first) = iter.next() {
            let _ = write!(&mut result, "{first}");
        }
        for &r in iter {
            let _ = write!(&mut result, "{r:09}");
        }
        result
    }

    pub(crate) fn from_decimal_string(s: &str, negative: bool) -> Self {
        let s = s.trim_start_matches('0');
        if s.is_empty() {
            return Self::zero();
        }
        let bytes = s.as_bytes();
        let mut limbs = Vec::new();
        let mut pos = bytes.len();
        while pos > 0 {
            let start = pos.saturating_sub(9);
            let chunk = &s[start..pos];
            let val: u32 = chunk.parse().unwrap_or(0);
            limbs.push(val);
            pos = start;
        }
        let mut result = Self { limbs, negative };
        result.normalize();
        result
    }

    pub(crate) fn mul_pow10(&self, n: u32) -> Option<Self> {
        if self.is_zero() || n == 0 {
            return Some(self.clone());
        }
        let mut result = self.clone();
        let mut remaining = n;
        while remaining > 0 {
            let chunk = remaining.min(18);
            let factor = 10u64.pow(chunk);
            result = result.mul_u64(factor)?;
            remaining -= chunk;
        }
        Some(result)
    }
}

impl PartialEq for BigCoefficient {
    fn eq(&self, other: &Self) -> bool {
        self.cmp_signed(other) == Ordering::Equal
    }
}

impl Eq for BigCoefficient {}

impl Hash for BigCoefficient {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.negative.hash(state);
        self.limbs.hash(state);
    }
}

// ---------------------------------------------------------------------------
// Unsigned decimal string division
// ---------------------------------------------------------------------------

fn big_decimal_div_rem(dividend: &str, divisor: &str) -> (String, String) {
    let dividend = dividend.trim_start_matches('0');
    let divisor = divisor.trim_start_matches('0');
    if divisor.is_empty() || divisor == "0" {
        return ("0".to_owned(), "0".to_owned());
    }
    if dividend.is_empty() || dividend == "0" {
        return ("0".to_owned(), "0".to_owned());
    }
    if big_decimal_cmp(dividend, divisor) == Ordering::Less {
        return ("0".to_owned(), dividend.to_owned());
    }

    let divisor_digits: Vec<u8> = divisor.bytes().map(|b| b - b'0').collect();
    let mut remainder: Vec<u8> = Vec::new();
    let mut quotient = String::new();

    for byte in dividend.bytes() {
        remainder.push(byte - b'0');
        while remainder.len() > 1 && remainder[0] == 0 {
            remainder.remove(0);
        }
        let mut count = 0u8;
        while vec_cmp_digits(&remainder, &divisor_digits) != Ordering::Less {
            remainder = vec_sub_digits(&remainder, &divisor_digits);
            count += 1;
            if count > 9 {
                break;
            }
        }
        quotient.push((b'0' + count) as char);
    }

    let q = quotient.trim_start_matches('0');
    let q = if q.is_empty() { "0" } else { q };
    let r: String = remainder.iter().map(|&d| (b'0' + d) as char).collect();
    let r = r.trim_start_matches('0');
    let r = if r.is_empty() { "0" } else { r };
    (q.to_owned(), r.to_owned())
}

fn vec_cmp_digits(a: &[u8], b: &[u8]) -> Ordering {
    let a_trimmed = trim_leading_zeros(a);
    let b_trimmed = trim_leading_zeros(b);
    if a_trimmed.len() != b_trimmed.len() {
        return a_trimmed.len().cmp(&b_trimmed.len());
    }
    a_trimmed.cmp(b_trimmed)
}

fn trim_leading_zeros(a: &[u8]) -> &[u8] {
    let start = a.iter().position(|&d| d != 0).unwrap_or(a.len());
    if start == a.len() {
        &a[a.len().saturating_sub(1)..]
    } else {
        &a[start..]
    }
}

fn vec_sub_digits(a: &[u8], b: &[u8]) -> Vec<u8> {
    let mut a = a.to_vec();
    let mut b_padded = vec![0u8; a.len().saturating_sub(b.len())];
    b_padded.extend_from_slice(b);
    let mut borrow: i16 = 0;
    for i in (0..a.len()).rev() {
        let bi = if i < b_padded.len() { b_padded[i] } else { 0 };
        let mut diff = i16::from(a[i]) - i16::from(bi) - borrow;
        if diff < 0 {
            diff += 10;
            borrow = 1;
        } else {
            borrow = 0;
        }
        a[i] = u8::try_from(diff).unwrap_or(0);
    }
    while a.len() > 1 && a[0] == 0 {
        a.remove(0);
    }
    a
}

fn big_decimal_cmp(a: &str, b: &str) -> Ordering {
    let a = a.trim_start_matches('0');
    let b = b.trim_start_matches('0');
    let a = if a.is_empty() { "0" } else { a };
    let b = if b.is_empty() { "0" } else { b };
    if a.len() != b.len() {
        return a.len().cmp(&b.len());
    }
    a.cmp(b)
}

// ---------------------------------------------------------------------------
// NumericValue
// ---------------------------------------------------------------------------

/// Sentinel scale for special values (NaN, Infinity, -Infinity).
pub(super) const SPECIAL_SCALE: u32 = u32::MAX;
