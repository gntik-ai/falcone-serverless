use falcone_serverless_worker::limits::{Field, Limits, ValidationKind};
use serde_json::{Value, json};

fn config() -> Value {
    json!({"version":1,"compiler_cpu_seconds":120,"compiler_memory_bytes":4294967296u64,
        "guest_memory_bytes":268435456,"guest_to_host_bytes":1048576,"host_to_guest_bytes":1048576,
        "guest_deadline_ms":100,"host_call_deadline_ms":50,"termination_budget_ms":500})
}

#[test]
fn explicit_contract_parses_and_invalid_fields_fail_closed() {
    let parsed = Limits::parse(&config().to_string()).unwrap();
    assert_eq!(parsed.value(Field::GuestMemoryBytes), 268435456);
    assert_eq!(parsed.value(Field::GuestDeadlineMs), 100);
    for field in Field::ALL {
        let mut missing = config();
        missing.as_object_mut().unwrap().remove(field.name());
        let error = Limits::parse(&missing.to_string()).unwrap_err();
        assert_eq!(
            (error.field.as_str(), error.kind),
            (field.name(), ValidationKind::Missing)
        );
        for (value, kind) in [
            (json!(0), ValidationKind::Zero),
            (json!(u64::MAX), ValidationKind::Unbounded),
            (json!("unbounded"), ValidationKind::Invalid),
            (Value::Null, ValidationKind::Invalid),
            (json!(-1), ValidationKind::Invalid),
        ] {
            let mut invalid = config();
            invalid[field.name()] = value;
            let error = Limits::parse(&invalid.to_string()).unwrap_err();
            assert_eq!((error.field.as_str(), error.kind), (field.name(), kind));
        }
    }
    let mut unknown = config();
    unknown["version"] = json!(2);
    let error = Limits::parse(&unknown.to_string()).unwrap_err();
    assert_eq!(
        (error.field.as_str(), error.kind),
        ("version", ValidationKind::UnknownVersion)
    );
    for (field, value) in [
        ("guest_memory_bytes", 268435457),
        ("guest_to_host_bytes", 1048577),
        ("host_to_guest_bytes", 1048577),
        ("host_call_deadline_ms", 30001),
    ] {
        let mut widened = config();
        widened[field] = json!(value);
        let error = Limits::parse(&widened.to_string()).unwrap_err();
        assert_eq!(
            (error.field.as_str(), error.kind),
            (field, ValidationKind::AboveCeiling)
        );
    }
}
