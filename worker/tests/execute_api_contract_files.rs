//! Static checks of the execution API's repository files: the failure code table, the schema,
//! the spec tables and the `Code` enum agree, and the images ship what the API needs. These
//! read the repository, so they run in CI and never in the packaged qualification suites.
#![cfg(target_os = "linux")]

use serde_json::{Value, json};

fn root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .to_path_buf()
}

fn read_json(path: &str) -> Value {
    serde_json::from_slice(&std::fs::read(root().join(path)).unwrap()).unwrap()
}

#[test]
fn the_code_table_the_schema_the_spec_and_the_code_enum_agree_on_every_code() {
    use falcone_serverless_worker::api::{Class, Code, Reason, Refusal};
    let table = read_json("docs/contracts/worker-execution-api/failure-codes.v1.json");
    let table = table.as_array().unwrap();
    let common = read_json("docs/contracts/worker-execution-api/common.v1.schema.json");
    let failure = &common["$defs"]["failure"];
    let spec = std::fs::read_to_string(
        root().join("docs/contracts/worker-execution-api/execution-api-v1.md"),
    )
    .unwrap();
    // Every code of the enum is a row, and every row a code.
    let names: Vec<String> = Code::ALL
        .iter()
        .map(|c| {
            serde_json::to_value(c)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    let rows: Vec<String> = table
        .iter()
        .map(|r| r["code"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(names, rows);
    let schema_codes: Vec<String> = failure["properties"]["code"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(schema_codes, names);
    let mut every_reason = std::collections::BTreeSet::new();
    for (row, code) in table.iter().zip(Code::ALL) {
        let name = row["code"].as_str().unwrap();
        // The enum's reasons are the row's reasons.
        let reasons: Vec<String> = code
            .reasons()
            .iter()
            .map(|r| {
                serde_json::to_value(r)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        let listed: Vec<String> = row["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
        assert_eq!(reasons, listed, "{name}");
        every_reason.extend(listed.iter().cloned());
        assert_eq!(
            u64::from(code.status()),
            row["status"].as_u64().unwrap(),
            "{name}"
        );
        for reason in &listed {
            let reason_value: Reason = serde_json::from_value(json!(reason)).unwrap();
            let refusal = Refusal::new(code, reason_value);
            let wire = serde_json::to_value(&refusal).unwrap();
            assert_eq!(wire["status"], row["status"], "{name}");
            assert_eq!(wire["code"], name);
            assert_eq!(wire["detail"]["reason"], json!(reason));
            // The class is the row's, or the reason's own override.
            let expected = row["class_by_reason"][reason.as_str()]
                .as_str()
                .unwrap_or(row["class"].as_str().unwrap());
            assert_eq!(
                serde_json::to_value(refusal.class()).unwrap(),
                json!(expected),
                "{name}/{reason}"
            );
            let retryable = refusal.class() == Class::Retryable;
            assert_eq!(wire["retryable"], json!(retryable), "{name}/{reason}");
            assert_eq!(wire["retryable"], row["retryable"], "{name}/{reason}");
        }
        // The schema: a status, a retryable value and the reasons, for the code.
        let block = failure["allOf"]
            .as_array()
            .unwrap()
            .iter()
            .find(|b| b["if"]["properties"]["code"]["const"] == json!(name))
            .unwrap_or_else(|| panic!("{name} has no schema block"));
        assert_eq!(
            block["then"]["properties"]["status"]["const"], row["status"],
            "{name}"
        );
        assert_eq!(
            block["then"]["properties"]["retryable"]["const"], row["retryable"],
            "{name}"
        );
        assert_eq!(
            block["then"]["properties"]["detail"]["properties"]["reason"]["enum"], row["reasons"],
            "{name}"
        );
        // The spec: one table row with the status, every reason and the class.
        let line = spec
            .lines()
            .find(|l| l.starts_with(&format!("| `{name}` |")))
            .unwrap_or_else(|| panic!("{name} is not in the spec table"));
        assert!(line.contains(&format!("| {} |", row["status"])), "{name}");
        for reason in &listed {
            assert!(line.contains(reason.as_str()), "{name}/{reason}");
        }
        assert!(line.contains(row["class"].as_str().unwrap()), "{name}");
    }
    let schema_reasons: std::collections::BTreeSet<String> =
        failure["properties"]["detail"]["properties"]["reason"]["enum"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r.as_str().unwrap().to_owned())
            .collect();
    assert_eq!(schema_reasons, every_reason);
}

#[test]
fn the_production_image_ships_the_compile_child_beside_the_worker() {
    let production = std::fs::read_to_string(root().join("worker/Containerfile")).unwrap();
    let copies: Vec<&str> = production
        .lines()
        .filter(|l| l.starts_with("COPY --from=build"))
        .collect();
    let target = |binary: &str| {
        copies
            .iter()
            .find(|line| line.contains(&format!("/release/{binary} ")))
            .and_then(|line| line.split_whitespace().last())
            .unwrap_or_else(|| panic!("{binary} is not copied into the production image"))
            .to_owned()
    };
    let (worker, child) = (
        target("falcone-serverless-worker"),
        target("artifact-compile-child"),
    );
    assert_eq!(
        std::path::Path::new(&worker).parent(),
        std::path::Path::new(&child).parent()
    );
    assert!(child.ends_with("/artifact-compile-child"));
    // The qualification image ships the suites that run in the restricted pod: the hook-enabled
    // one beside the runner, and the black-box one that drives the production image.
    let qualification =
        std::fs::read_to_string(root().join("worker/qualification/Containerfile")).unwrap();
    for suite in ["execute_api_local", "execute_api_production"] {
        assert!(
            qualification.contains(&format!("/usr/local/bin/{suite}")),
            "{suite}"
        );
        assert!(
            qualification.contains(&format!("--test {suite}")),
            "{suite}"
        );
    }
}
