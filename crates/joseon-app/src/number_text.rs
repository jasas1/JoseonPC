//! The one way the app prints a number: a real minus sign "−" (U+2212), never
//! the ASCII hyphen.
//!
//! Port source: `Sources/JoseonApp/NumberText.swift`.

pub const MINUS: &str = "\u{2212}";

/// "−11.5", "2.5", and with `plus` "+2.5". A value that rounds to zero has no sign.
pub fn signed(value: f64, decimals: usize, plus: bool) -> String {
    if !value.is_finite() {
        return "\u{2014}".to_string();
    }
    let body = format!("{:.*}", decimals, value.abs());
    let is_zero = !body.chars().any(|c| c != '0' && c != '.');
    if is_zero {
        body
    } else if value < 0.0 {
        format!("{MINUS}{body}")
    } else if plus {
        format!("+{body}")
    } else {
        body
    }
}

/// Text from another module (stress flag titles and details) can carry an ASCII hyphen as a minus sign:
/// "-21.6 dBFS". A hyphen that starts a number becomes a real minus. A hyphen inside a word or between two
/// numbers ("1/6-octave", "32-bit", "3-5") stays.
pub fn typographic(text: &str) -> String {
    if !text.contains('-') {
        return text.to_string();
    }
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    for (i, c) in chars.iter().enumerate() {
        let starts_number = chars.get(i + 1).is_some_and(|n| n.is_numeric() || *n == '.');
        let after_word = i > 0 && chars[i - 1].is_alphanumeric();
        if *c == '-' && starts_number && !after_word {
            out.push_str(MINUS);
        } else {
            out.push(*c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_carry_a_real_minus() {
        assert_eq!(signed(-11.5, 1, false), "\u{2212}11.5");
        assert_eq!(signed(2.5, 1, false), "2.5");
        assert_eq!(signed(2.5, 1, true), "+2.5");
        assert_eq!(signed(-0.04, 1, true), "0.0");
        assert_eq!(signed(-14.0, 0, false), "\u{2212}14");
        assert_eq!(signed(f64::NAN, 1, false), "\u{2014}");
    }

    #[test]
    fn a_hyphen_before_a_number_becomes_a_minus() {
        assert_eq!(typographic("Peak at -21.6 dBFS"), "Peak at \u{2212}21.6 dBFS");
        assert_eq!(typographic("-3 dB and -.5"), "\u{2212}3 dB and \u{2212}.5");
        assert_eq!(
            typographic("1/6-octave, 32-bit, 3-5 kHz, well-known"),
            "1/6-octave, 32-bit, 3-5 kHz, well-known"
        );
        assert_eq!(typographic("no hyphen"), "no hyphen");
    }
}
