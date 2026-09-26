//! Small helpers shared by core and first-party plugins, so each rule has one implementation.

/// Parse decimal text (`"-12.30"`) into an integer scaled by `10^scale` for an Arrow decimal.
/// Negative scales are allowed (`"1200"` at scale -2 is `12`). Digits that the scale would
/// drop must be zero: a value that doesn't fit the scale is an error, never silently cut.
pub fn scaled_decimal(text: &str, scale: i8) -> Result<i128, String> {
    let bad = || format!("`{text}` isn't a decimal that fits scale {scale}");
    let t = text.trim();
    let (neg, t) = match t.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, t.strip_prefix('+').unwrap_or(t)),
    };
    let (int, frac) = t.split_once('.').unwrap_or((t, ""));
    if int.is_empty() && frac.is_empty() || !int.bytes().chain(frac.bytes()).all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    // All digits, and the power of ten of the last one.
    let mut digits = format!("{int}{frac}");
    let mut exp = -(frac.len() as i32);
    let target = -i32::from(scale);
    // Drop trailing digits below the scale; they must be zeros.
    while exp < target {
        match digits.pop() {
            Some('0') => exp += 1,
            Some(_) => return Err(bad()),
            None => break,
        }
    }
    while exp > target {
        digits.push('0');
        exp -= 1;
    }
    let digits = digits.trim_start_matches('0');
    let v: i128 = if digits.is_empty() {
        0
    } else {
        digits.parse().map_err(|_| bad())?
    };
    Ok(if neg { -v } else { v })
}

/// Parse a cell reference (`B12`, `$B$12`) into zero-based `(row, col)`. `$` may only appear
/// directly before the column letters and before the row number.
pub fn parse_cell(s: &str) -> Option<(u32, u16)> {
    let s = s.trim();
    let s = s.strip_prefix('$').unwrap_or(s);
    let split = s.find(|c: char| !c.is_ascii_alphabetic())?;
    let (letters, rest) = s.split_at(split);
    let digits = rest.strip_prefix('$').unwrap_or(rest);
    if letters.is_empty()
        || letters.len() > 3
        || digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let col = letters
        .to_ascii_uppercase()
        .bytes()
        .fold(0u32, |acc, b| acc * 26 + u32::from(b - b'A' + 1));
    let row: u32 = digits.parse().ok()?;
    if col == 0 || col > 16_384 || row == 0 || row > 1_048_576 {
        return None;
    }
    Some((row - 1, (col - 1) as u16))
}

/// Percent-encode everything except RFC 3986 unreserved characters and, if `keep_slash`, `/`.
pub fn percent_encode(s: &str, keep_slash: bool) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b'/' if keep_slash => out.push('/'),
            b => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// One line, at most `max` characters, for quoting SQL in messages.
pub fn summarize(text: &str, max: usize) -> String {
    let one = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > max {
        format!("{}…", one.chars().take(max).collect::<String>())
    } else {
        one
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scales_decimals_exactly() {
        assert_eq!(scaled_decimal("12.34", 2), Ok(1234));
        assert_eq!(scaled_decimal("-0.5", 3), Ok(-500));
        assert_eq!(scaled_decimal("7", 2), Ok(700));
        assert_eq!(scaled_decimal("12.3400", 2), Ok(1234));
        assert_eq!(scaled_decimal("1200", -2), Ok(12));
        assert_eq!(scaled_decimal("0.000", 0), Ok(0));
    }

    #[test]
    fn refuses_to_drop_significant_digits() {
        assert!(scaled_decimal("12.345", 2).is_err());
        assert!(scaled_decimal("1250", -2).is_err());
        assert!(scaled_decimal("abc", 2).is_err());
        assert!(scaled_decimal("", 2).is_err());
    }

    #[test]
    fn parses_cell_references() {
        assert_eq!(parse_cell("A1"), Some((0, 0)));
        assert_eq!(parse_cell("$B$12"), Some((11, 1)));
        assert_eq!(parse_cell("b12"), Some((11, 1)));
        assert_eq!(parse_cell("XFD1048576"), Some((1_048_575, 16_383)));
        for bad in ["XFE1", "A0", "1A", "B$1$2", "$$A1", "A", "12", "A1B"] {
            assert_eq!(parse_cell(bad), None, "{bad}");
        }
    }

    #[test]
    fn percent_encodes() {
        assert_eq!(percent_encode("a b/c.csv", true), "a%20b/c.csv");
        assert_eq!(percent_encode("a b/c", false), "a%20b%2Fc");
    }
}
