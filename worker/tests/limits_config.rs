use falcone_serverless_worker::limits::{Error, Field, Invalid, Limits};
use serde_json::{Value, json};

fn config() -> Value {
    json!({"version":1, "compile_cpu_seconds":120, "compile_memory_bytes":4294967296u64,
        "compile_wall_clock_ms":300000, "compile_input_bytes":67108864,
        "guest_memory_bytes":268435456, "guest_to_host_bytes":1048576,
        "host_to_guest_bytes":1048576, "guest_deadline_ms":1000,
        "host_call_deadline_ms":30000, "termination_budget_ms":5000})
}

#[test]
fn complete_contract_parses_without_defaults() {
    let limits = Limits::parse(&config().to_string()).unwrap();
    assert_eq!(limits.get(Field::GuestDeadlineMs), 1000);
    for field in Field::ALL {
        assert_eq!(limits.get(field), config()[field.name()].as_u64().unwrap());
        let mut value = config();
        value.as_object_mut().unwrap().remove(field.name());
        assert_eq!(
            Limits::parse(&value.to_string()),
            Err(Error::Field {
                field: field.name().into(),
                kind: Invalid::Missing
            })
        );
    }
}

#[test]
fn invalid_values_name_each_field_and_reason() {
    for (field, ceiling) in [
        (Field::CompileCpuSeconds, 3600u64),
        (Field::CompileMemoryBytes, 68719476736),
        (Field::CompileWallClockMs, 3600000),
        (Field::CompileInputBytes, 67108864),
        (Field::GuestMemoryBytes, 536870912),
        (Field::GuestToHostBytes, 1048576),
        (Field::HostToGuestBytes, 1048576),
        (Field::GuestDeadlineMs, 300000),
        (Field::HostCallDeadlineMs, 30000),
        (Field::TerminationBudgetMs, 30000),
    ] {
        let mut at_ceiling = config();
        at_ceiling[field.name()] = json!(ceiling);
        let limits = Limits::parse(&at_ceiling.to_string()).unwrap();
        assert_eq!(limits.get(field), ceiling);
        for (value, kind) in [
            (json!(0), Invalid::Zero),
            (json!(u64::MAX), Invalid::Unbounded),
            (json!(ceiling + 1), Invalid::AboveCeiling),
            (json!("1"), Invalid::WrongType),
            (Value::Null, Invalid::WrongType),
            (json!(-1), Invalid::WrongType),
            (json!(1.5), Invalid::WrongType),
        ] {
            let mut config = config();
            config[field.name()] = value;
            assert_eq!(
                Limits::parse(&config.to_string()),
                Err(Error::Field {
                    field: field.name().into(),
                    kind
                })
            );
        }
    }
    let mut config = config();
    config["surprise"] = json!(1);
    assert_eq!(
        Limits::parse(&config.to_string()),
        Err(Error::Field {
            field: "surprise".into(),
            kind: Invalid::Unknown
        })
    );
}

#[test]
fn version_and_document_errors_are_typed() {
    for version in [json!(2), json!(0), json!(u64::MAX), json!("1"), Value::Null] {
        let mut config = config();
        config["version"] = version;
        assert!(matches!(
            Limits::parse(&config.to_string()),
            Err(Error::Version)
        ));
    }
    let mut config = config();
    config.as_object_mut().unwrap().remove("version");
    assert_eq!(Limits::parse(&config.to_string()), Err(Error::Version));
    assert_eq!(Limits::parse("[]"), Err(Error::Document));
    assert_eq!(Limits::parse("{"), Err(Error::Document));
}

#[test]
fn duplicate_fields_are_rejected_instead_of_overwritten() {
    for field in Field::ALL {
        for value in [0, 1] {
            let source =
                config()
                    .to_string()
                    .replacen('{', &format!("{{\"{}\":{value},", field.name()), 1);
            assert_eq!(
                Limits::parse(&source),
                Err(Error::Field {
                    field: field.name().into(),
                    kind: Invalid::Duplicate,
                })
            );
        }
    }
    let source = config().to_string().replacen('{', "{\"version\":1,", 1);
    assert_eq!(Limits::parse(&source), Err(Error::Version));
}

#[test]
fn positional_documents_are_not_a_closed_named_contract() {
    assert_eq!(
        Limits::parse(
            "[1,120,4294967296,300000,67108864,268435456,1048576,1048576,1000,30000,5000]"
        ),
        Err(Error::Document)
    );
}
