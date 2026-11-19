//! Qualification oracle. Variables and prefixes must be explicitly declared by the
//! inventory for that expectation. Current legacy inventory fixtures declare neither.
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
pub enum Variable {
    String,
    Number,
    Integer,
    Boolean,
    Object,
    Array,
    Null,
    Timestamp,
}

impl Variable {
    fn accepts(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Number => value.is_number(),
            Self::Integer => value.as_number().is_some_and(|n| decimal(n).2 >= 0),
            Self::Boolean => value.is_boolean(),
            Self::Object => value.is_object(),
            Self::Array => value.is_array(),
            Self::Null => value.is_null(),
            Self::Timestamp => value.as_str().is_some_and(timestamp),
        }
    }
}

/// UTC RFC3339, with optional decimal seconds; calendar dates are checked as well.
fn timestamp(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() < 20
        || [4, 7, 10, 13, 16]
            .iter()
            .zip(b"--T::")
            .any(|(i, c)| b[*i] != *c)
        || b.last() != Some(&b'Z')
    {
        return false;
    }
    if b[..19]
        .iter()
        .enumerate()
        .any(|(i, c)| !matches!(i, 4 | 7 | 10 | 13 | 16) && !c.is_ascii_digit())
    {
        return false;
    }
    if b.len() > 20
        && (b[19] != b'.' || b.len() == 21 || !b[20..b.len() - 1].iter().all(u8::is_ascii_digit))
    {
        return false;
    }
    let number = |start: usize, end: usize| {
        text.get(start..end)
            .and_then(|v| v.parse::<u32>().ok())
            .unwrap_or(u32::MAX)
    };
    let year = number(0, 4);
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = match number(5, 7) {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 0,
    };
    (1..=days).contains(&number(8, 10))
        && number(11, 13) < 24
        && number(14, 16) < 60
        && number(17, 19) < 60
}

fn decimal(number: &serde_json::Number) -> (bool, String, i64) {
    let text = number.to_string();
    let negative = text.starts_with('-');
    let text = text.strip_prefix('-').unwrap_or(&text);
    let (mantissa, exponent) = text
        .split_once(['e', 'E'])
        .map_or((text, 0), |(m, e)| (m, e.parse::<i64>().unwrap()));
    let fractional = mantissa.split_once('.').map_or(0, |(_, f)| f.len());
    let mut digits = mantissa.replace('.', "").trim_start_matches('0').to_owned();
    if digits.is_empty() {
        return (false, "0".into(), 0);
    }
    let mut exponent = exponent - fractional as i64;
    while digits.ends_with('0') {
        digits.pop();
        exponent += 1;
    }
    (negative, digits, exponent)
}

pub fn envelope(actual: &Value, expected: &Value, variables: &BTreeMap<String, Variable>) -> bool {
    if variables
        .keys()
        .any(|path| actual.pointer(path).is_none() || expected.pointer(path).is_none())
    {
        return false;
    }
    compare(actual, expected, "", variables)
}

fn compare(
    actual: &Value,
    expected: &Value,
    path: &str,
    variables: &BTreeMap<String, Variable>,
) -> bool {
    if let Some(variable) = variables.get(path) {
        return variable.accepts(actual);
    }
    match (actual, expected) {
        (Value::Object(a), Value::Object(e)) => {
            a.len() == e.len()
                && e.iter().all(|(key, value)| {
                    let key_path = format!("{path}/{}", key.replace('~', "~0").replace('/', "~1"));
                    a.get(key)
                        .is_some_and(|a| compare(a, value, &key_path, variables))
                })
        }
        (Value::Array(a), Value::Array(e)) => {
            a.len() == e.len()
                && a.iter()
                    .zip(e)
                    .enumerate()
                    .all(|(i, (a, e))| compare(a, e, &format!("{path}/{i}"), variables))
        }
        (Value::Number(a), Value::Number(e)) => decimal(a) == decimal(e),
        _ => actual == expected,
    }
}

pub enum Prefix<'a> {
    Literal(&'a str),
    Timestamp,
}

fn strip<'a>(mut text: &'a str, prefixes: &[Prefix<'_>]) -> &'a str {
    for prefix in prefixes {
        text = match prefix {
            Prefix::Literal(prefix) => text.strip_prefix(prefix).unwrap_or(text),
            Prefix::Timestamp => text
                .split_once(' ')
                .filter(|(t, _)| timestamp(t))
                .map_or(text, |(_, message)| message),
        };
    }
    text
}

pub fn logs(actual: &[String], expected: &[String], prefixes: &[Prefix<'_>]) -> bool {
    actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected)
            .all(|(a, e)| strip(a, prefixes) == strip(e, prefixes))
}
