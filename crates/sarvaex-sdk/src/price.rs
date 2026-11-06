//! Integer-safe conversion between exchange ticks and human-readable prices.
//!
//! The exchange only understands `price_ticks`. How a tick maps to a display
//! unit is per contract: binary contracts are cents (`51` ticks = `$0.51`),
//! futures use the contract's `settlement_rule.encoding` (`{divider, decimals,
//! prefix, suffix}`), e.g. `430` ticks with divider 100 = `4.30%`.
//!
//! Nothing here uses floating point. Parsing is exact: `4.305` on a
//! divider-100 contract is an error, not a rounding.

use crate::error::{Result, SarvaexError};
use crate::models::{Contract, ContractKind};
use serde::{Deserialize, Serialize};

/// How to render `price_ticks` for one contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct PriceEncoding {
    /// `display = ticks / divider`
    pub divider: i64,
    /// Decimal places to show.
    pub decimals: u32,
    #[serde(default)]
    pub prefix: String,
    #[serde(default)]
    pub suffix: String,
    #[serde(default)]
    pub compact_thousands: bool,
    /// True for YES/NO contracts, where ticks are cents of a $1 payout.
    #[serde(default)]
    pub binary: bool,
}

impl PriceEncoding {
    pub fn binary() -> Self {
        Self {
            divider: 100,
            decimals: 2,
            prefix: "$".into(),
            suffix: String::new(),
            compact_thousands: false,
            binary: true,
        }
    }

    /// Resolve the encoding for a contract: `settlement_rule.encoding` first,
    /// then the top-level `divider`, then a kind-based default.
    pub fn for_contract(c: &Contract) -> Self {
        if c.kind() == ContractKind::Binary {
            return Self::binary();
        }
        if let Some(enc) = c.settlement_rule.as_ref().and_then(|r| r.get("encoding")) {
            let divider = enc.get("divider").and_then(as_i64).unwrap_or(0);
            if divider > 0 {
                return Self {
                    divider,
                    decimals: enc.get("decimals").and_then(as_i64).unwrap_or(0).max(0) as u32,
                    prefix: enc
                        .get("prefix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    suffix: enc
                        .get("suffix")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_owned(),
                    compact_thousands: enc
                        .get("compactThousands")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false),
                    binary: false,
                };
            }
        }
        if c.divider > 0 {
            return Self {
                divider: c.divider,
                decimals: decimals_for_divider(c.divider),
                prefix: String::new(),
                suffix: String::new(),
                compact_thousands: false,
                binary: false,
            };
        }
        Self {
            divider: 100,
            decimals: 2,
            prefix: String::new(),
            suffix: String::new(),
            compact_thousands: false,
            binary: false,
        }
    }

    /// Render ticks as a display price, e.g. `$0.51`, `4.30%`, `$80,000`.
    pub fn format(&self, ticks: i64) -> String {
        let body = format_scaled(ticks, self.divider, self.decimals, self.compact_thousands);
        format!("{}{}{}", self.prefix, body, self.suffix)
    }

    /// Bare number without prefix/suffix, e.g. `0.51`, `4.30`.
    pub fn format_plain(&self, ticks: i64) -> String {
        format_scaled(ticks, self.divider, self.decimals, false)
    }

    /// Parse a human price into ticks. Exact; never rounds.
    ///
    /// Binary contracts accept `0.51`, `.51`, `$0.51`, `51c`, `51¢`. A bare
    /// integer `>= 1` on a binary contract is rejected as ambiguous.
    pub fn parse(&self, input: &str) -> Result<i64> {
        let raw = input.trim();
        if raw.is_empty() {
            return Err(SarvaexError::Validation("price is empty".into()));
        }
        let mut s = raw.to_owned();
        // Strip the contract's own prefix/suffix and common currency symbols.
        if !self.prefix.is_empty() {
            s = s
                .strip_prefix(self.prefix.as_str())
                .map(str::to_owned)
                .unwrap_or(s);
        }
        if !self.suffix.is_empty() {
            s = s
                .strip_suffix(self.suffix.as_str())
                .map(str::to_owned)
                .unwrap_or(s);
        }
        s = s.trim_start_matches(['$', '€', '£']).trim().to_owned();
        let mut cents_mode = false;
        if self.binary
            && let Some(t) = s.strip_suffix(['c', 'C', '¢'])
        {
            cents_mode = true;
            s = t.trim().to_owned();
        }
        s = s.replace([',', '_'], "");
        let (mantissa, scale) = parse_decimal(&s)
            .ok_or_else(|| SarvaexError::Validation(format!("'{raw}' is not a valid price")))?;

        if cents_mode {
            if scale != 0 {
                return Err(SarvaexError::Validation(format!(
                    "'{raw}': cents must be a whole number"
                )));
            }
            return Ok(mantissa);
        }
        if self.binary && scale == 0 && mantissa >= 1 {
            return Err(SarvaexError::Validation(format!(
                "'{raw}' is ambiguous for a YES/NO contract: use dollars (0.{mantissa:02}) or cents ({mantissa}c), or --price-ticks {mantissa}"
            )));
        }
        // ticks = mantissa * divider / 10^scale, must be exact.
        let pow = 10i64
            .checked_pow(scale)
            .ok_or_else(|| SarvaexError::Validation("too many decimals".into()))?;
        let num = mantissa
            .checked_mul(self.divider)
            .ok_or_else(|| SarvaexError::Validation("price overflow".into()))?;
        if num % pow != 0 {
            return Err(SarvaexError::Validation(format!(
                "'{raw}' has more precision than this contract supports (1 tick = {})",
                self.format(1)
            )));
        }
        Ok(num / pow)
    }
}

fn as_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
        .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        .or_else(|| v.as_f64().map(|f| f as i64))
}

fn decimals_for_divider(divider: i64) -> u32 {
    let mut d = divider;
    let mut n = 0;
    while d > 1 && d % 10 == 0 {
        d /= 10;
        n += 1;
    }
    n
}

/// Parse `"-12.345"` into `(mantissa = -12345, scale = 3)`.
pub(crate) fn parse_decimal(s: &str) -> Option<(i64, u32)> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    let (int_part, frac_part) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    if int_part.is_empty() && frac_part.is_empty() {
        return None;
    }
    if !int_part.chars().all(|c| c.is_ascii_digit())
        || !frac_part.chars().all(|c| c.is_ascii_digit())
    {
        return None;
    }
    let digits = format!("{int_part}{frac_part}");
    let digits = if digits.is_empty() {
        "0".to_owned()
    } else {
        digits
    };
    let mut mantissa: i64 = digits.parse().ok()?;
    if neg {
        mantissa = -mantissa;
    }
    Some((mantissa, frac_part.len() as u32))
}

fn format_scaled(ticks: i64, divider: i64, decimals: u32, compact_thousands: bool) -> String {
    let divider = divider.max(1);
    let neg = ticks < 0;
    let abs = ticks.unsigned_abs();
    let div = divider as u64;
    let int_part = abs / div;
    let mut frac = abs % div;
    // Scale the fractional remainder to `decimals` digits exactly when
    // divider is a power of ten; otherwise fall back to long division.
    let frac_str = if decimals == 0 {
        String::new()
    } else {
        let mut out = String::with_capacity(decimals as usize);
        for _ in 0..decimals {
            frac *= 10;
            out.push(char::from(b'0' + (frac / div) as u8));
            frac %= div;
        }
        out
    };
    let int_str = if compact_thousands || int_part >= 1000 {
        group_thousands(int_part)
    } else {
        int_part.to_string()
    };
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.push_str(&int_str);
    if !frac_str.is_empty() {
        s.push('.');
        s.push_str(&frac_str);
    }
    s
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// Render micro-USDC as a decimal USDC string with two to six decimals.
pub fn micro_to_usdc(micro: i64) -> String {
    let neg = micro < 0;
    let abs = micro.unsigned_abs();
    let int = abs / 1_000_000;
    let frac = abs % 1_000_000;
    let mut frac_s = format!("{frac:06}");
    while frac_s.len() > 2 && frac_s.ends_with('0') {
        frac_s.pop();
    }
    format!(
        "{}{}.{}",
        if neg { "-" } else { "" },
        group_thousands(int),
        frac_s
    )
}

/// Parse a USDC amount like `10000`, `10,000.50` into micro-USDC exactly.
pub fn usdc_to_micro(input: &str) -> Result<i64> {
    let s = input.trim().trim_start_matches('$').replace(',', "");
    let (mantissa, scale) = parse_decimal(&s)
        .ok_or_else(|| SarvaexError::Validation(format!("'{input}' is not a valid USDC amount")))?;
    if scale > 6 {
        return Err(SarvaexError::Validation(
            "USDC supports at most 6 decimal places".into(),
        ));
    }
    let pow = 10i64.pow(6 - scale);
    mantissa
        .checked_mul(pow)
        .ok_or_else(|| SarvaexError::Validation("amount overflow".into()))
}

/// Binary implied probability in basis points (ticks of cents → bp), for display.
pub fn binary_probability_pct(price_ticks: i64) -> String {
    format!("{price_ticks}%")
}

/// Complement price for a binary contract (`NO = 100 - YES`).
pub fn binary_complement(price_ticks: i64) -> i64 {
    100 - price_ticks
}

/// Validate a limit price against the contract's tick grid and bounds.
pub fn validate_limit_price(c: &Contract, ticks: i64) -> Result<()> {
    if ticks <= 0 {
        return Err(SarvaexError::Validation(
            "price_ticks must be positive".into(),
        ));
    }
    if c.min_price_ticks > 0 && ticks < c.min_price_ticks {
        return Err(SarvaexError::Validation(format!(
            "price {ticks} ticks is below the contract minimum {} ticks",
            c.min_price_ticks
        )));
    }
    if c.max_price_ticks > 0 && ticks > c.max_price_ticks {
        return Err(SarvaexError::Validation(format!(
            "price {ticks} ticks is above the contract maximum {} ticks",
            c.max_price_ticks
        )));
    }
    if c.tick_size > 1 && ticks % c.tick_size != 0 {
        return Err(SarvaexError::Validation(format!(
            "price {ticks} ticks is not aligned to tick_size {}",
            c.tick_size
        )));
    }
    Ok(())
}

/// Validate order count against the contract's max order size.
pub fn validate_count(c: &Contract, count: i64) -> Result<()> {
    if count <= 0 {
        return Err(SarvaexError::Validation(
            "count must be a positive integer".into(),
        ));
    }
    if c.max_order_size > 0 && count > c.max_order_size {
        return Err(SarvaexError::Validation(format!(
            "count {count} exceeds this contract's max_order_size {}",
            c.max_order_size
        )));
    }
    Ok(())
}

/// Best-effort notional in micro-USDC for a limit order, used only for
/// client-side safety guards. Binary: `ticks × count × 10_000` (1 tick = 1¢).
/// Futures: `ticks × count × tick_value_micro` when the contract publishes it.
pub fn estimated_notional_micro(c: &Contract, price_ticks: i64, count: i64) -> Option<i64> {
    match c.kind() {
        ContractKind::Binary => price_ticks.checked_mul(count)?.checked_mul(10_000),
        ContractKind::Futures if c.tick_value_micro > 0 => price_ticks
            .checked_mul(count)?
            .checked_mul(c.tick_value_micro),
        ContractKind::Futures if c.multiplier_micro_per_display_unit > 0 && c.divider > 0 => {
            // display_units × multiplier_per_display_unit
            price_ticks
                .checked_mul(count)?
                .checked_mul(c.multiplier_micro_per_display_unit)?
                .checked_div(c.divider)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binary() -> Contract {
        Contract {
            ticker: "SX-FEDDEC-26OCT-H25".into(),
            kind: 1,
            tick_size: 1,
            min_price_ticks: 1,
            max_price_ticks: 99,
            ..Default::default()
        }
    }
    fn futures(encoding: serde_json::Value, divider: i64) -> Contract {
        Contract {
            ticker: "SXF-USCPIYOY-26SEP".into(),
            kind: 2,
            tick_size: 1,
            min_price_ticks: 1,
            max_price_ticks: 100_000,
            divider,
            settlement_rule: Some(json!({ "encoding": encoding })),
            ..Default::default()
        }
    }

    #[test]
    fn binary_formats_as_dollars() {
        let e = PriceEncoding::for_contract(&binary());
        assert_eq!(e.format(51), "$0.51");
        assert_eq!(e.format(5), "$0.05");
        assert_eq!(e.format(99), "$0.99");
    }

    #[test]
    fn binary_parses_dollars_and_cents() {
        let e = PriceEncoding::for_contract(&binary());
        assert_eq!(e.parse("0.51").unwrap(), 51);
        assert_eq!(e.parse(".51").unwrap(), 51);
        assert_eq!(e.parse("$0.51").unwrap(), 51);
        assert_eq!(e.parse("51c").unwrap(), 51);
        assert_eq!(e.parse("51¢").unwrap(), 51);
        assert_eq!(e.parse("0.5").unwrap(), 50);
        assert!(e.parse("51").is_err(), "bare integer is ambiguous");
        assert!(e.parse("0.515").is_err(), "sub-cent precision rejected");
    }

    #[test]
    fn futures_uses_settlement_rule_encoding() {
        let c = futures(json!({"divider": 100, "decimals": 2, "suffix": "%"}), 100);
        let e = PriceEncoding::for_contract(&c);
        assert_eq!(e.format(430), "4.30%");
        assert_eq!(e.parse("4.30").unwrap(), 430);
        assert_eq!(e.parse("4.3%").unwrap(), 430);
        assert_eq!(e.parse("4").unwrap(), 400);
        assert!(e.parse("4.305").is_err());
    }

    #[test]
    fn futures_compact_thousands() {
        let c = futures(
            json!({"divider": 1, "decimals": 0, "prefix": "$", "compactThousands": true}),
            1,
        );
        let e = PriceEncoding::for_contract(&c);
        assert_eq!(e.format(80000), "$80,000");
        assert_eq!(e.parse("80,000").unwrap(), 80000);
        assert_eq!(e.parse("$80000").unwrap(), 80000);
    }

    #[test]
    fn futures_falls_back_to_divider() {
        let mut c = futures(json!({}), 10000);
        c.settlement_rule = None;
        let e = PriceEncoding::for_contract(&c);
        assert_eq!(e.divider, 10000);
        assert_eq!(e.decimals, 4);
        assert_eq!(e.format(11600), "1.1600");
        assert_eq!(e.parse("1.16").unwrap(), 11600);
    }

    #[test]
    fn usdc_conversions() {
        assert_eq!(micro_to_usdc(10_000_000_000), "10,000.00");
        assert_eq!(micro_to_usdc(5_100_000), "5.10");
        assert_eq!(micro_to_usdc(1), "0.000001");
        assert_eq!(micro_to_usdc(-2_500_000), "-2.50");
        assert_eq!(usdc_to_micro("10000").unwrap(), 10_000_000_000);
        assert_eq!(usdc_to_micro("10,000.50").unwrap(), 10_000_500_000);
        assert!(usdc_to_micro("1.2345678").is_err());
    }

    #[test]
    fn validates_tick_grid_and_bounds() {
        let mut c = binary();
        c.tick_size = 5;
        assert!(validate_limit_price(&c, 50).is_ok());
        assert!(validate_limit_price(&c, 51).is_err());
        assert!(validate_limit_price(&c, 0).is_err());
        assert!(validate_limit_price(&c, 100).is_err());
        assert!(validate_count(&c, 0).is_err());
        c.max_order_size = 10;
        assert!(validate_count(&c, 11).is_err());
        assert!(validate_count(&c, 10).is_ok());
    }

    #[test]
    fn notional_binary() {
        assert_eq!(estimated_notional_micro(&binary(), 51, 10), Some(5_100_000));
    }
}
