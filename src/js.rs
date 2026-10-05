use std::fmt::Write;

pub fn is_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{0B}' | '\u{0C}' | '\r' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200A}' | '\u{2028}' | '\u{2029}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

pub fn trim(s: &str) -> &str {
    s.trim_matches(is_space)
}

pub fn utf16_len(s: &str) -> u32 {
    if s.is_ascii() {
        return s.len() as u32;
    }
    s.chars().map(|c| c.len_utf16() as u32).sum()
}

pub fn char_utf16(c: char) -> u32 {
    c.len_utf16() as u32
}

pub fn number_to_string(value: f64) -> String {
    let mut out = String::new();
    write_number(&mut out, value);
    out
}

pub fn write_number(out: &mut String, value: f64) {
    if value.is_nan() {
        out.push_str("NaN");
        return;
    }
    if value.is_infinite() {
        out.push_str(if value > 0.0 { "Infinity" } else { "-Infinity" });
        return;
    }
    if value == 0.0 {
        out.push('0');
        return;
    }
    if value.fract() == 0.0 && value.abs() < 9007199254740992.0 {
        let _ = write!(out, "{}", value as i128);
        return;
    }
    if value < 0.0 {
        out.push('-');
    }
    let sci = format!("{:e}", value.abs());
    let (mantissa, exponent) = sci.split_once('e').unwrap_or((&sci, "0"));
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let exponent: i32 = exponent.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let n = exponent + 1;
    if k <= n && n <= 21 {
        out.push_str(&digits);
        for _ in 0..(n - k) {
            out.push('0');
        }
    } else if 0 < n && n <= 21 {
        out.push_str(&digits[..n as usize]);
        out.push('.');
        out.push_str(&digits[n as usize..]);
    } else if -6 < n && n <= 0 {
        out.push_str("0.");
        for _ in 0..(-n) {
            out.push('0');
        }
        out.push_str(&digits);
    } else {
        out.push_str(&digits[..1]);
        if k > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        let _ = write!(out, "e{}{}", if n > 0 { "+" } else { "-" }, (n - 1).abs());
    }
}

pub fn json_quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

pub fn to_lower(s: &str) -> String {
    s.to_lowercase()
}

pub fn to_upper(s: &str) -> String {
    s.to_uppercase()
}

pub fn utf16_prefix(s: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in s.char_indices() {
        if count >= units {
            return &s[..index];
        }
        count += c.len_utf16();
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_format_like_javascript() {
        let cases = [
            (1.0, "1"),
            (0.1, "0.1"),
            (100.0, "100"),
            (1.5e-7, "1.5e-7"),
            (1e21, "1e+21"),
            (123456789012345680000.0, "123456789012345680000"),
            (0.000001, "0.000001"),
            (1e-7, "1e-7"),
            (-2.5, "-2.5"),
            (3.25, "3.25"),
            (200000.0, "200000"),
            (1.7976931348623157e308, "1.7976931348623157e+308"),
            (5e-324, "5e-324"),
            (0.1 + 0.2, "0.30000000000000004"),
            (-0.0, "0"),
            (12.5e3, "12500"),
        ];
        for (value, expected) in cases {
            assert_eq!(number_to_string(value), expected, "{value}");
        }
    }
}
