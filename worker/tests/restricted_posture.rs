//! Static qualification contract gate; no runtime or live deployment access.

use falcone_serverless_worker::{Role, check};
use serde::Deserialize;
use serde_json::Value;
use std::fs;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Case {
    id: String,
    role: String,
    expect: String,
    object: Value,
}

fn cases() -> Vec<Case> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../docs/contracts/restricted-posture/fixtures-v1.json");
    serde_json::from_str(&fs::read_to_string(path).expect("read v1 corpus"))
        .expect("parse v1 corpus")
}

#[test]
fn shared_fixture_corpus_matches_v1() {
    for case in cases() {
        let role = match case.role.as_str() {
            "guest-worker" => Role::GuestWorker,
            "controller" => Role::Controller,
            other => panic!("{}: unknown role {other}", case.id),
        };
        let violations = check(&case.object, role);
        let ids: Vec<_> = violations
            .iter()
            .map(|violation| violation.rule_id)
            .collect();
        let expected = if case.expect == "pass" {
            vec![]
        } else {
            vec![case.expect.as_str()]
        };
        assert_eq!(ids, expected, "{}", case.id);
    }
}

#[test]
fn corpus_and_spec_cover_exactly_the_v1_rules_and_required_cases() {
    use std::collections::BTreeSet;

    let cases = cases();
    let expected_ids = BTreeSet::from([
        "RP-01", "RP-02", "RP-03", "RP-04", "RP-05", "RP-06", "RP-07", "RP-08", "RP-09", "RP-10",
        "RP-11", "RP-12",
    ]);
    let actual_ids: BTreeSet<_> = cases
        .iter()
        .filter(|case| case.expect != "pass")
        .map(|case| case.expect.as_str())
        .collect();
    assert_eq!(actual_ids, expected_ids);
    let mut case_ids = BTreeSet::new();
    for case in &cases {
        assert!(
            !case.id.is_empty() && case_ids.insert(&case.id),
            "duplicate or empty case ID"
        );
    }
    let readme = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../docs/contracts/restricted-posture/README.md"),
    )
    .expect("read v1 specification");
    for header in ["Contract: ctr-restricted-posture-suite", "Version: 1"] {
        assert!(
            readme.lines().any(|line| line == header),
            "missing {header}"
        );
    }
    for id in expected_ids {
        assert!(readme.contains(id), "undocumented rule {id}");
    }
    for (id, role, expect) in [
        ("c-worker", "guest-worker", "pass"),
        ("c-controller-token", "controller", "pass"),
        ("v-rp01-container-override", "guest-worker", "RP-01"),
        ("v-rp02-init-container", "guest-worker", "RP-02"),
        ("v-rp10-token-absent", "guest-worker", "RP-10"),
        ("v-rp12-clusterrole", "guest-worker", "RP-12"),
        ("v-rp12-rolebinding-clusterrole", "guest-worker", "RP-12"),
        ("v-rp12-no-kind", "guest-worker", "RP-12"),
    ] {
        let case = cases.iter().find(|case| case.id == id).expect(id);
        assert_eq!(
            (case.role.as_str(), case.expect.as_str()),
            (role, expect),
            "{id}"
        );
        if matches!(id, "c-controller-token" | "v-rp10-token-absent") {
            assert!(
                case.object["spec"]["template"]["spec"]
                    .get("automountServiceAccountToken")
                    .is_none(),
                "{id}"
            );
        }
    }
}
