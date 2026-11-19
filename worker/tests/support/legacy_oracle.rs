//! Inventory oracle. No fixture declares a variable implicitly; unknown fields stay exact.
use serde_json::Value;

pub struct Variable<'a> {
    pub pointer: &'a str,
    pub schema: Value,
}
pub enum Prefix<'a> {
    Timestamp,
    Stream(&'a str),
}

fn valid(schema: &Value, value: &Value) -> bool {
    if schema.get("format").is_some() && !value.is_string() {
        return false;
    }
    jsonschema::options()
        .should_validate_formats(true)
        .should_ignore_unknown_formats(false)
        .offline()
        .build(schema)
        .is_ok_and(|validator| validator.is_valid(value))
}

fn message<'a>(mut line: &'a str, prefixes: &[Prefix<'_>]) -> Result<&'a str, String> {
    for prefix in prefixes {
        line = match prefix {
            Prefix::Stream(prefix) => line
                .strip_prefix(prefix)
                .ok_or("log stream prefix missing")?,
            Prefix::Timestamp => {
                let (stamp, rest) = line.split_once(' ').ok_or("log timestamp missing")?;
                if !valid(
                    &serde_json::json!({"type":"string","format":"date-time"}),
                    &Value::String(stamp.into()),
                ) {
                    return Err("log timestamp malformed".into());
                }
                rest
            }
        };
    }
    Ok(line)
}

/// Preserve number lexemes in a parallel JSON tree, after serde_json validates the input.
/// Quoting only numbers outside strings avoids changing production dependency features or
/// rounding through serde_json's default f64 representation before comparison.
fn number_text(input: &[u8]) -> Result<Value, String> {
    let mut quoted = Vec::with_capacity(input.len());
    let mut at = 0;
    while at < input.len() {
        let start = at;
        match input[at] {
            b'"' => {
                at += 1;
                while at < input.len() {
                    let byte = input[at];
                    at += 1;
                    if byte == b'\\' {
                        at += 1;
                    } else if byte == b'"' {
                        break;
                    }
                }
                quoted.extend_from_slice(&input[start..at]);
            }
            b'-' | b'0'..=b'9' => {
                while input
                    .get(at)
                    .is_some_and(|byte| byte.is_ascii_digit() || b"-.eE+".contains(byte))
                {
                    at += 1;
                }
                quoted.push(b'"');
                quoted.extend_from_slice(&input[start..at]);
                quoted.push(b'"');
            }
            byte => {
                quoted.push(byte);
                at += 1;
            }
        }
    }
    serde_json::from_slice(&quoted).map_err(|_| "cannot preserve JSON number text".into())
}

/// Compare decimal values exactly, including integers outside the f64 exact-integer range.
fn decimal(text: &str) -> Option<(bool, String, i64)> {
    let negative = text.starts_with('-');
    let unsigned = text.trim_start_matches('-');
    let (mantissa, exponent) = unsigned.split_once(['e', 'E']).unwrap_or((unsigned, "0"));
    let fractional = mantissa
        .split_once('.')
        .map_or(0, |(_, fraction)| fraction.len());
    let mut exponent: i64 = exponent
        .parse::<i64>()
        .ok()?
        .checked_sub(fractional as i64)?;
    let mut digits = mantissa.replace('.', "").trim_start_matches('0').to_owned();
    if digits.is_empty() {
        return Some((false, String::new(), 0));
    }
    while digits.ends_with('0') {
        digits.pop();
        exponent = exponent.checked_add(1)?;
    }
    Some((negative, digits, exponent))
}

pub fn compare(
    actual: &[u8],
    expected: &[u8],
    variables: &[Variable<'_>],
    prefixes: &[Prefix<'_>],
) -> Result<(), String> {
    let actual_value: Value =
        serde_json::from_slice(actual).map_err(|_| "adapter output is not JSON")?;
    let expected_value: Value =
        serde_json::from_slice(expected).map_err(|_| "inventory expectation is not JSON")?;
    let numbers = (number_text(actual)?, number_text(expected)?);
    let (actual, expected) = (&actual_value, &expected_value);
    for (index, field) in variables.iter().enumerate() {
        if variables[..index]
            .iter()
            .any(|prior| prior.pointer == field.pointer)
            || expected.pointer(field.pointer).is_none()
            || actual.pointer(field.pointer).is_none()
            || (!field.schema["type"].is_string() && !field.schema["format"].is_string())
        {
            return Err("invalid inventory variable declaration or missing field".into());
        }
    }
    equal(
        actual,
        expected,
        "",
        variables,
        prefixes,
        (&numbers.0, &numbers.1),
    )
}

fn equal(
    actual: &Value,
    expected: &Value,
    pointer: &str,
    variables: &[Variable<'_>],
    prefixes: &[Prefix<'_>],
    numbers: (&Value, &Value),
) -> Result<(), String> {
    if let Some(field) = variables.iter().find(|field| field.pointer == pointer) {
        if valid(&field.schema, actual) {
            return Ok(());
        }
        return Err("variable field violates its declared type or format".into());
    }
    match (actual, expected) {
        (Value::Object(actual), Value::Object(expected)) if actual.len() == expected.len() => {
            for (key, value) in expected {
                let actual = actual.get(key).ok_or("missing inventory field")?;
                let key = key.replace('~', "~0").replace('/', "~1");
                equal(
                    actual,
                    value,
                    &format!("{pointer}/{key}"),
                    variables,
                    prefixes,
                    numbers,
                )?;
            }
            Ok(())
        }
        (Value::Array(actual), Value::Array(expected)) if actual.len() == expected.len() => {
            for (index, (actual, expected)) in actual.iter().zip(expected).enumerate() {
                if pointer == "/logs" {
                    let actual = actual.as_str().ok_or("non-string log")?;
                    let expected = expected.as_str().ok_or("non-string inventory log")?;
                    if message(actual, prefixes)? != message(expected, prefixes)? {
                        return Err("log message differs".into());
                    }
                } else {
                    equal(
                        actual,
                        expected,
                        &format!("{pointer}/{index}"),
                        variables,
                        prefixes,
                        numbers,
                    )?;
                }
            }
            Ok(())
        }
        (Value::Number(_), Value::Number(_)) => {
            let number = |tree: &Value| {
                tree.pointer(pointer)
                    .and_then(Value::as_str)
                    .and_then(decimal)
            };
            let actual = number(numbers.0);
            if actual.is_some() && actual == number(numbers.1) {
                Ok(())
            } else {
                Err("number differs from inventory".into())
            }
        }
        (Value::Null, Value::Null) => Ok(()),
        (Value::String(actual), Value::String(expected)) if actual == expected => Ok(()),
        (Value::Bool(actual), Value::Bool(expected)) if actual == expected => Ok(()),
        _ => Err("envelope differs from inventory".into()),
    }
}
