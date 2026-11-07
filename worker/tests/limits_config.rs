//! WAS-02 ac3: the versioned limits contract validates fail-closed and names the field.
use falcone_serverless_worker::limits::{Field, Limits, LimitsError};
use serde_json::{Value, json};
use std::time::Duration;

fn valid() -> Value {
    json!({
        "version": 1,
        "compiler_cpu_seconds": 60,
        "compiler_memory_bytes": 2_147_483_648u64,
        "guest_memory_bytes": 134_217_728,
        "guest_to_host_buffer_bytes": 65_536,
        "host_to_guest_buffer_bytes": 32_768,
        "guest_deadline_ms": 5_000,
        "host_call_deadline_ms": 2_000,
        "termination_budget_ms": 3_000,
        "interruption_tolerance_ms": 1_500
    })
}

fn parse(value: &Value) -> Result<Limits, LimitsError> {
    Limits::parse(&value.to_string())
}

fn with(field: &str, value: Value) -> Value {
    let mut config = valid();
    config[field] = value;
    config
}

#[test]
fn a_fully_specified_config_parses_to_the_exact_values() {
    let limits = parse(&valid()).unwrap();
    assert_eq!(limits.compiler_cpu_seconds(), 60);
    assert_eq!(limits.compiler_memory_bytes(), 2_147_483_648);
    assert_eq!(limits.guest_memory_bytes(), 134_217_728);
    assert_eq!(limits.guest_to_host_buffer_bytes(), 65_536);
    assert_eq!(limits.host_to_guest_buffer_bytes(), 32_768);
    assert_eq!(limits.guest_deadline(), Duration::from_secs(5));
    assert_eq!(limits.host_call_deadline(), Duration::from_secs(2));
    assert_eq!(limits.termination_budget(), Duration::from_secs(3));
    assert_eq!(
        limits.interruption_tolerance(),
        Duration::from_millis(1_500)
    );
}

#[test]
fn every_missing_field_is_rejected_by_name() {
    for field in Field::ALL {
        let mut config = valid();
        config.as_object_mut().unwrap().remove(field.name());
        assert_eq!(
            parse(&config),
            Err(LimitsError::Missing {
                field: field.name()
            }),
            "{}",
            field.name()
        );
    }
}

#[test]
fn zero_unbounded_and_over_ceiling_values_are_rejected_by_name() {
    for field in Field::ALL {
        let name = field.name();
        assert_eq!(
            parse(&with(name, json!(0))),
            Err(LimitsError::Zero { field: name }),
            "{name} zero"
        );
        assert_eq!(
            parse(&with(name, json!(u64::MAX))),
            Err(LimitsError::Unbounded { field: name }),
            "{name} max"
        );
        assert_eq!(
            parse(&with(name, json!(field.ceiling() + 1))),
            Err(LimitsError::AboveCeiling {
                field: name,
                ceiling: field.ceiling()
            }),
            "{name} above ceiling"
        );
        // The ceiling itself is a legal, bounded value.
        let at_ceiling = parse(&with(name, json!(field.ceiling()))).unwrap();
        assert_eq!(at_ceiling.get(field), field.ceiling(), "{name} at ceiling");
    }
}

#[test]
fn non_integer_values_are_rejected_by_name() {
    for bad in [json!("60"), json!(-1), json!(1.5), json!(null), json!(true)] {
        assert_eq!(
            parse(&with("compiler_cpu_seconds", bad.clone())),
            Err(LimitsError::NotAnInteger {
                field: "compiler_cpu_seconds"
            }),
            "{bad}"
        );
    }
}

#[test]
fn unknown_and_missing_versions_are_rejected_naming_version() {
    for version in [json!(0), json!(2), json!(u64::MAX)] {
        let error = parse(&with("version", version.clone())).unwrap_err();
        assert!(
            matches!(error, LimitsError::UnknownVersion { .. }),
            "{version}: {error:?}"
        );
        assert_eq!(error.field(), Some("version"));
    }
    assert_eq!(
        parse(&with("version", json!("1"))).unwrap_err().field(),
        Some("version")
    );
    let mut config = valid();
    config.as_object_mut().unwrap().remove("version");
    assert_eq!(
        parse(&config),
        Err(LimitsError::Missing { field: "version" })
    );
}

#[test]
fn unknown_fields_and_malformed_documents_fail_closed() {
    let mut config = valid();
    config["guest_deadline_seconds"] = json!(1);
    assert_eq!(
        parse(&config),
        Err(LimitsError::UnknownField {
            name: "guest_deadline_seconds".into()
        })
    );
    for text in ["", "[]", "{", "null", "7"] {
        assert_eq!(Limits::parse(text), Err(LimitsError::Malformed), "{text}");
    }
}

#[test]
fn qualification_defaults_validate_and_do_not_weaken_the_prior_constants() {
    let defaults = Limits::qualification();
    assert_eq!(
        Limits::parse(&defaults.to_json().to_string()).unwrap(),
        defaults
    );
    // Values the worker enforced before the contract existed (independent literals).
    assert_eq!(defaults.guest_to_host_buffer_bytes(), 1_048_576);
    assert_eq!(defaults.host_to_guest_buffer_bytes(), 1_048_576);
    assert_eq!(defaults.guest_memory_bytes(), 256 * 1024 * 1024);
    assert_eq!(defaults.host_call_deadline(), Duration::from_secs(30));
    assert_eq!(defaults.compiler_cpu_seconds(), 120);
    assert_eq!(defaults.compiler_memory_bytes(), 4 * 1024 * 1024 * 1024);
}

#[test]
fn replacing_a_field_revalidates_it() {
    let limits = Limits::qualification();
    let tighter = limits.replace(Field::GuestDeadlineMs, 250).unwrap();
    assert_eq!(tighter.guest_deadline(), Duration::from_millis(250));
    assert_eq!(
        limits.replace(Field::GuestDeadlineMs, 0),
        Err(LimitsError::Zero {
            field: "guest_deadline_ms"
        })
    );
    assert_eq!(
        limits.replace(Field::GuestMemoryBytes, u64::MAX),
        Err(LimitsError::Unbounded {
            field: "guest_memory_bytes"
        })
    );
}
