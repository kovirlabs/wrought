//! Money: integer amounts in a currency's minor unit (pence, cents, yen),
//! never floating point. ISO 4217 codes; the exponent says how many minor
//! units make a major one (JPY 0, GBP 2, KWD 3).

/// Currencies without minor units (ISO 4217 exponent 0).
const EXPONENT_0: &[&str] = &[
    "BIF", "CLP", "DJF", "GNF", "ISK", "JPY", "KMF", "KRW", "PYG", "RWF", "UGX", "UYI", "VND",
    "VUV", "XAF", "XOF", "XPF",
];

/// Currencies with three decimals.
const EXPONENT_3: &[&str] = &["BHD", "IQD", "JOD", "KWD", "LYD", "OMR", "TND"];

/// Symbols printed before the amount; other currencies print their code.
const SYMBOLS: &[(&str, &str)] = &[("GBP", "£"), ("USD", "$"), ("EUR", "€"), ("JPY", "¥")];

/// Whether `code` looks like an ISO 4217 code (three capital letters).
pub fn valid_currency(code: &str) -> bool {
    code.len() == 3 && code.bytes().all(|b| b.is_ascii_uppercase())
}

/// Minor-unit exponent of a currency (2 unless listed otherwise).
pub fn exponent(code: &str) -> u32 {
    if EXPONENT_0.contains(&code) {
        0
    } else if EXPONENT_3.contains(&code) {
        3
    } else {
        2
    }
}

/// `1234, "GBP"` -> `£12.34`; `1500, "JPY"` -> `¥1,500`;
/// `1234, "CHF"` -> `CHF 12.34`. Thousands are grouped with commas.
pub fn format(amount: i64, code: &str) -> String {
    let exp = exponent(code);
    let neg = amount < 0;
    let abs = amount.unsigned_abs();
    let unit = 10u64.pow(exp);
    let (major, minor) = (abs / unit, abs % unit);
    let digits = major.to_string();
    let mut grouped = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(c);
    }
    let number = if exp == 0 {
        grouped
    } else {
        format!("{grouped}.{minor:0width$}", width = exp as usize)
    };
    let sign = if neg { "-" } else { "" };
    match SYMBOLS.iter().find(|(c, _)| *c == code) {
        Some((_, s)) => format!("{sign}{s}{number}"),
        None => format!("{sign}{code} {number}"),
    }
}

/// Price × quantity, refusing overflow.
pub fn line_amount(unit: i64, quantity: i64) -> Option<i64> {
    unit.checked_mul(quantity)
}

/// The sum of line amounts, refusing overflow.
pub fn total(amounts: impl IntoIterator<Item = i64>) -> Option<i64> {
    amounts
        .into_iter()
        .try_fold(0i64, |acc, a| acc.checked_add(a))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exponents() {
        assert_eq!(exponent("GBP"), 2);
        assert_eq!(exponent("USD"), 2);
        assert_eq!(exponent("JPY"), 0);
        assert_eq!(exponent("KRW"), 0);
        assert_eq!(exponent("KWD"), 3);
    }

    #[test]
    fn formatting_follows_the_exponent() {
        assert_eq!(format(1234, "GBP"), "£12.34");
        assert_eq!(format(5, "GBP"), "£0.05");
        assert_eq!(format(0, "USD"), "$0.00");
        assert_eq!(format(123_456_789, "USD"), "$1,234,567.89");
        assert_eq!(format(1500, "JPY"), "¥1,500");
        assert_eq!(format(999, "JPY"), "¥999");
        assert_eq!(format(1234, "KWD"), "KWD 1.234");
        assert_eq!(format(1234, "CHF"), "CHF 12.34");
        assert_eq!(format(-250, "EUR"), "-€2.50");
        assert_eq!(format(i64::MIN, "USD"), "-$92,233,720,368,547,758.08");
    }

    #[test]
    fn currency_codes() {
        assert!(valid_currency("GBP"));
        for bad in ["gbp", "GB", "GBPX", "G1P", ""] {
            assert!(!valid_currency(bad), "{bad}");
        }
    }

    #[test]
    fn arithmetic_refuses_overflow() {
        assert_eq!(line_amount(250, 3), Some(750));
        assert_eq!(line_amount(i64::MAX, 2), None);
        assert_eq!(total([1, 2, 3]), Some(6));
        assert_eq!(total([i64::MAX, 1]), None);
    }
}
