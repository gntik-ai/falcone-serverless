//! UTC deadline syntax shared by the OCI tester and its local reference.

pub fn utc_deadline(text: &str) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() < 20
        || !bytes.is_ascii()
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || !text.ends_with('Z')
    {
        return false;
    }
    let number = |start, end| {
        text[start..end]
            .bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| text[start..end].parse::<u32>().ok())
            .flatten()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    let fraction = &bytes[19..bytes.len() - 1];
    day > 0
        && day <= days
        && hour < 24
        && minute < 60
        && (second < 60 || (second == 60 && hour == 23 && minute == 59))
        && (fraction.is_empty()
            || (fraction[0] == b'.'
                && (2..=10).contains(&fraction.len())
                && fraction[1..].iter().all(u8::is_ascii_digit)))
}
