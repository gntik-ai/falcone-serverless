#![deny(warnings)]

//! Pure, test-only ctr-worker-execution-api OCI profile v1 model.

use serde::Deserialize;
use serde_json::{Value, json};

pub fn evaluate(rule: &str, input: &Value) -> Value {
    match rule {
        "R1" => transport(input),
        "R2" => shim_identity(input),
        "R3" => dispatcher_identity(input),
        "R4" => identity_mounts(input),
        "R5" => container_request(input),
        "R6" => concurrency(input),
        "R7" => deadline(input),
        _ => json!({"outcome": "reject", "reason": "invalid_input"}),
    }
}

fn transport(input: &Value) -> Value {
    let accepted =
        fields(input, &["transport"]) && input["transport"].as_str() == Some("mutual_tls");
    json!({"outcome": if accepted { "accept" } else { "reject" }, "request_read": accepted})
}

fn fields(input: &Value, allowed: &[&str]) -> bool {
    input
        .as_object()
        .is_some_and(|object| object.keys().all(|key| allowed.contains(&key.as_str())))
}

fn shim_identity(input: &Value) -> Value {
    let verified = fields(input, &["identity"]) && input["identity"].as_str() == Some("verified");
    json!({"outcome": if verified { "allow" } else { "refuse" },
        "audit_events": if verified { 0 } else { 1 },
        "credential_sent": verified, "request_sent": verified})
}

fn dispatcher_identity(input: &Value) -> Value {
    let verified = fields(input, &["identity"]) && input["identity"].as_str() == Some("verified");
    json!({"outcome": if verified { "accept" } else { "reject" }, "forwarded": false})
}

fn identity_mounts(input: &Value) -> Value {
    let conformant = fields(input, &["identity_mounts", "automountServiceAccountToken"])
        && input["identity_mounts"] == json!(["shim"])
        && input["automountServiceAccountToken"].as_bool() == Some(false);
    json!({"outcome": if conformant { "conformant" } else { "non_conformant" }})
}

fn container_request(input: &Value) -> Value {
    let refused = json!({"outcome": "refuse", "request": null});
    if !fields(input, &["execution_credential", "params", "context"])
        || input.get("params").is_none()
        || input.get("context").is_none()
    {
        return refused;
    }
    let Some(credential) = input["execution_credential"]
        .as_str()
        .filter(|value| !value.is_empty())
    else {
        return refused;
    };
    let request = json!({"params": input["params"], "context": input["context"]});
    let wire = request.to_string();
    let quoted = json!(credential).to_string();
    if wire.contains("execution_credential")
        || wire.contains(credential)
        || wire.contains(&quoted[1..quoted.len() - 1])
        || contains_credential(&request, credential)
    {
        return refused;
    }
    json!({"outcome": "forward", "request": request})
}

fn contains_credential(value: &Value, credential: &str) -> bool {
    let forbidden = |text: &str| text.contains("execution_credential") || text.contains(credential);
    match value {
        Value::String(text) => forbidden(text),
        Value::Array(values) => values
            .iter()
            .any(|value| contains_credential(value, credential)),
        Value::Object(values) => values
            .iter()
            .any(|(key, value)| forbidden(key) || contains_credential(value, credential)),
        _ => false,
    }
}

fn concurrency(input: &Value) -> Value {
    let Some(limit) = input["limit"].as_u64().filter(|limit| *limit > 0) else {
        return json!({"outcome": "reject", "reason": "invalid_configuration"});
    };
    if !fields(input, &["limit", "in_flight"]) {
        return json!({"outcome": "reject", "reason": "invalid_input"});
    }
    match input["in_flight"].as_u64() {
        Some(count) if count < limit => json!({"outcome": "admit", "reason": null}),
        Some(_) => json!({"outcome": "reject", "reason": "concurrency_limit_exceeded"}),
        None => json!({"outcome": "reject", "reason": "invalid_input"}),
    }
}

fn deadline(input: &Value) -> Value {
    let invalid = json!({"outcome": "reject", "reason": "invalid_input", "result_discarded": true});
    if !fields(input, &["phase", "now_ms", "deadline_ms"]) {
        return invalid;
    }
    let (Some(now), Some(deadline)) = (input["now_ms"].as_u64(), input["deadline_ms"].as_u64())
    else {
        return invalid;
    };
    let phase = input["phase"].as_str();
    if !matches!(phase, Some("admission" | "response")) {
        return invalid;
    }
    let expired = now >= deadline;
    let outcome = match (phase, expired) {
        (Some("admission"), false) => "admit",
        (Some("admission"), true) => "reject",
        (_, false) => "completed",
        (_, true) => "deadline_exceeded",
    };
    json!({"outcome": outcome, "reason": if expired { Some("deadline_exceeded") } else { None },
        "result_discarded": expired})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    rule: String,
    input: Value,
    expected: Value,
}

fn fixtures() -> Vec<Case> {
    parse_fixture_set(include_str!(
        "../../docs/contracts/worker-execution-api/fixtures-v1.json"
    ))
    .expect("well-formed fixture cases")
}

fn check_rule(rule: &str) {
    let cases = fixtures();
    assert!(cases.iter().any(|case| case.rule == rule));
    for case in cases.iter().filter(|case| case.rule == rule) {
        assert_eq!(evaluate(rule, &case.input), case.expected, "{}", case.id);
    }
}

const RULES: [&str; 7] = ["R1", "R2", "R3", "R4", "R5", "R6", "R7"];

fn parse_fixture_set(source: &str) -> Result<Vec<Case>, String> {
    let cases: Vec<Case> = serde_json::from_str(source).map_err(|error| error.to_string())?;
    let mut ids = std::collections::BTreeSet::new();
    for case in &cases {
        if case.id.trim().is_empty()
            || !ids.insert(&case.id)
            || !RULES.contains(&case.rule.as_str())
            || !valid_expected(case)
        {
            return Err(format!("invalid or duplicate fixture: {}", case.id));
        }
    }
    for rule in RULES {
        for polarity in [false, true] {
            if !cases
                .iter()
                .any(|case| case.rule == rule && positive(&case.expected) == polarity)
            {
                return Err(format!("{rule}: missing positive={polarity}"));
            }
        }
    }
    Ok(cases)
}

fn positive(expected: &Value) -> bool {
    matches!(
        expected["outcome"].as_str(),
        Some("accept" | "allow" | "conformant" | "forward" | "admit" | "completed")
    )
}

fn valid_expected(case: &Case) -> bool {
    let (outcomes, keys): (&[&str], &[&str]) = match case.rule.as_str() {
        "R1" => (&["accept", "reject"], &["outcome", "request_read"]),
        "R2" => (
            &["allow", "refuse"],
            &["outcome", "audit_events", "credential_sent", "request_sent"],
        ),
        "R3" => (&["accept", "reject"], &["outcome", "forwarded"]),
        "R4" => (&["conformant", "non_conformant"], &["outcome"]),
        "R5" => (&["forward", "refuse"], &["outcome", "request"]),
        "R6" => (&["admit", "reject"], &["outcome", "reason"]),
        "R7" => (
            &["admit", "reject", "completed", "deadline_exceeded"],
            &["outcome", "reason", "result_discarded"],
        ),
        _ => return false,
    };
    let Some(expected) = case.expected.as_object() else {
        return false;
    };
    expected.len() == keys.len()
        && expected.iter().all(|(key, value)| {
            keys.contains(&key.as_str())
                && match key.as_str() {
                    "outcome" => value
                        .as_str()
                        .is_some_and(|outcome| outcomes.contains(&outcome)),
                    "audit_events" => value.as_u64().is_some(),
                    "request" => {
                        value.is_null()
                            || value.as_object().is_some_and(|request| {
                                request.len() == 2
                                    && request.contains_key("params")
                                    && request.contains_key("context")
                            })
                    }
                    "reason" => {
                        value.is_null()
                            || value
                                .as_str()
                                .is_some_and(|reason| match case.rule.as_str() {
                                    "R6" => [
                                        "concurrency_limit_exceeded",
                                        "invalid_configuration",
                                        "invalid_input",
                                    ]
                                    .contains(&reason),
                                    "R7" => {
                                        ["deadline_exceeded", "invalid_input"].contains(&reason)
                                    }
                                    _ => false,
                                })
                    }
                    _ => value.is_boolean(),
                }
        })
}

#[test]
fn r1_transport_fixtures() {
    check_rule("R1");
}

#[test]
fn module_denies_warnings() {
    assert!(include_str!("oci_execution_profile.rs").starts_with("#![deny(warnings)]\n"));
}

#[test]
fn r2_shim_identity_fixtures() {
    check_rule("R2");
}

#[test]
fn r3_dispatcher_identity_fixtures() {
    check_rule("R3");
}

#[test]
fn r4_identity_mount_fixtures() {
    check_rule("R4");
}

#[test]
fn r5_container_request_fixtures() {
    check_rule("R5");
    for case in fixtures().iter().filter(|case| case.rule == "R5") {
        let result = evaluate("R5", &case.input);
        if result["outcome"] == "forward" {
            let wire = serde_json::to_string(&result["request"]).unwrap();
            assert!(!wire.contains("execution_credential"), "{}", case.id);
            assert!(
                !wire.contains(case.input["execution_credential"].as_str().unwrap()),
                "{}",
                case.id
            );
        }
    }
}

#[test]
fn r6_concurrency_fixtures() {
    check_rule("R6");
}

#[test]
fn r7_deadline_fixtures() {
    check_rule("R7");
}

#[test]
fn coverage_requires_both_polarities_for_every_rule() {
    let source = include_str!("../../docs/contracts/worker-execution-api/fixtures-v1.json");
    assert!(parse_fixture_set(source).is_ok());
    let cases: Value = serde_json::from_str(source).unwrap();
    let cases = cases.as_array().unwrap();
    for rule in RULES {
        for polarity in [false, true] {
            let mut changed = cases.clone();
            changed.retain(|case| case["rule"] != rule || positive(&case["expected"]) != polarity);
            assert!(
                parse_fixture_set(&json!(changed).to_string()).is_err(),
                "{rule}: missing positive={polarity}"
            );
        }
    }
}

#[test]
fn coverage_rejects_unknown_duplicate_and_malformed_cases() {
    let source = include_str!("../../docs/contracts/worker-execution-api/fixtures-v1.json");
    let baseline: Value = serde_json::from_str(source).unwrap();
    let baseline = baseline.as_array().unwrap();
    let mut additions = vec![baseline[0].clone()];
    for (key, value) in [
        ("id", json!("")),
        ("rule", json!("R8")),
        (
            "expected",
            json!({"outcome": "unknown", "request_read": false}),
        ),
        ("expected", json!({"outcome": "reject"})),
        (
            "expected",
            json!({"outcome": "reject", "request_read": "false"}),
        ),
        (
            "expected",
            json!({"outcome": "reject", "request_read": false, "extra": true}),
        ),
        ("input", json!({})),
    ] {
        let mut case = baseline[0].clone();
        case["id"] = json!("mutated-extra-case");
        case[key] = value;
        if key == "input" {
            case.as_object_mut().unwrap().remove("input");
        }
        additions.push(case);
    }
    for extra in additions {
        let mut changed = baseline.clone();
        changed.push(extra.clone());
        assert!(
            parse_fixture_set(&json!(changed).to_string()).is_err(),
            "accepted invalid case: {extra}"
        );
    }
    for malformed in ["not JSON", "{}", "[{}]", "[]"] {
        assert!(parse_fixture_set(malformed).is_err());
    }
}

#[test]
fn unknown_rules_and_malformed_inputs_never_grant_access() {
    for rule in RULES.into_iter().chain(["unknown"]) {
        for input in [
            json!(null),
            json!([]),
            json!(true),
            json!(42),
            json!("verified"),
            json!({}),
        ] {
            let result = evaluate(rule, &input);
            assert!(!positive(&result), "{rule}: {input}");
        }
    }
    // A known positive input with extra ambiguous data must also fail closed.
    for case in fixtures().iter().filter(|case| positive(&case.expected)) {
        let mut input = case.input.clone();
        input["unexpected"] = json!(true);
        assert!(!positive(&evaluate(&case.rule, &input)), "{}", case.id);
    }
}
