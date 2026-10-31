//! ADR 0001 documentation contract; no runtime or live deployment access.

use std::fs;
use std::path::Path;

struct Documents {
    adr: String,
    adr_0010_exists: bool,
}

impl Documents {
    fn load() -> Self {
        let adrs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs/adr");
        Self {
            adr: fs::read_to_string(adrs.join("0001-execution-model-option-a.md"))
                .expect("read ADR 0001"),
            adr_0010_exists: adrs
                .join("0010-oci-execution-mode-and-base-catalog.md")
                .is_file(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        check_facts(&self.adr)?;
        check_status(&self.adr)?;
        check_migration_scope(&self.adr)?;
        check_amendment(&self.adr, self.adr_0010_exists)
    }

    fn replace(&mut self, old: &str, new: &str) {
        self.adr = replace_once(&self.adr, old, new);
    }

    fn replace_in_amendment(&mut self, old: &str, new: &str) {
        let section = amendment(&self.adr).expect("amendment fixture");
        let changed = replace_once(section, old, new);
        self.adr = replace_once(&self.adr, section, &changed);
    }
}

fn replace_once(text: &str, old: &str, new: &str) -> String {
    assert_eq!(text.matches(old).count(), 1, "fixture anchor: {old}");
    text.replacen(old, new, 1)
}

fn check_facts(adr: &str) -> Result<(), String> {
    for fact in [
        "20 tenants",
        "10,000 deployed functions",
        "100,000 mostly WebSocket/event messages per second",
        "no admin sandbox",
        "Falcone owns quotas",
    ] {
        if !adr.contains(fact) {
            return Err(format!("missing fact: {fact}"));
        }
    }
    Ok(())
}

fn check_status(adr: &str) -> Result<(), String> {
    let status = "Accepted — operator, 2026-10-21. This records the existing direction, not a runtime qualification.";
    if !adr.lines().any(|line| line == status) {
        return Err("ADR 0001 Status must remain Accepted — operator, 2026-10-21".into());
    }
    Ok(())
}

fn check_migration_scope(adr: &str) -> Result<(), String> {
    if adr.contains("satisfies only MIG-01 criterion (2)") {
        return Err("outdated MIG-01 criterion (2)-only scope".into());
    }
    for statement in [
        "This ADR satisfies MIG-01 criterion (2) and the scope-note half of criterion (4).",
        "Contract inventory (1), dependency classification (3) and the classification half of criterion (4) remain open and gate MIG-01 and, through it, WAS-01.",
    ] {
        if !adr.contains(statement) {
            return Err(format!("missing MIG-01 scope statement: {statement}"));
        }
    }
    Ok(())
}

fn amendment(adr: &str) -> Result<&str, String> {
    let start = adr
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            *offset += line.len();
            Some((*offset, line.trim_end_matches(['\r', '\n'])))
        })
        .find(|(_, line)| *line == "## Amended by ADR 0010")
        .map(|(offset, _)| offset)
        .ok_or("missing section Amended by ADR 0010")?;
    let body = &adr[start..];
    if body.starts_with("## ") {
        return Ok("");
    }
    Ok(body.split("\n## ").next().unwrap_or(body))
}

fn check_amendment(adr: &str, adr_0010_exists: bool) -> Result<(), String> {
    let section = amendment(adr)?;
    if !section.contains("[ADR 0010](0010-oci-execution-mode-and-base-catalog.md)")
        || !adr_0010_exists
    {
        return Err(
            "amendment must link an existing 0010-oci-execution-mode-and-base-catalog.md".into(),
        );
    }
    for term in [
        "silent native-fallback",
        "CRD-controller",
        "administrator-installed sandbox",
    ] {
        if !section.contains(term) {
            return Err(format!("amendment must supersede {term}"));
        }
    }
    for statement in [
        "Approving the amended design is not implementation or security qualification.",
        "Those remain gated on WAS-01 (#51) and later qualification gates.",
    ] {
        if !section.contains(statement) {
            return Err(format!(
                "amendment missing qualification statement: {statement}"
            ));
        }
    }
    Ok(())
}

#[test]
fn committed_adr_passes() {
    Documents::load().validate().expect("valid ADR 0001 scope");
}

#[test]
fn each_missing_fact_is_named_and_rejected() {
    for fact in [
        "20 tenants",
        "10,000 deployed functions",
        "100,000 mostly WebSocket/event messages per second",
        "no admin sandbox",
        "Falcone owns quotas",
    ] {
        let mut docs = Documents::load();
        docs.replace(fact, "");
        let error = check_facts(&docs.adr).expect_err("missing fact must fail");
        assert!(error.contains(fact), "{error}");
    }
}

#[test]
fn changed_status_is_rejected() {
    let mut docs = Documents::load();
    docs.replace("Accepted — operator, 2026-10-21", "Proposed");
    assert!(check_status(&docs.adr).is_err());
}

#[test]
fn missing_amendment_link_is_rejected() {
    let mut docs = Documents::load();
    docs.replace_in_amendment(
        "[ADR 0010](0010-oci-execution-mode-and-base-catalog.md)",
        "ADR 0010",
    );
    assert!(check_amendment(&docs.adr, docs.adr_0010_exists).is_err());
}

#[test]
fn missing_link_target_is_rejected() {
    let docs = Documents::load();
    assert!(check_amendment(&docs.adr, false).is_err());
}

#[test]
fn each_superseded_term_is_required_inside_amendment() {
    for term in [
        "silent native-fallback",
        "CRD-controller",
        "administrator-installed sandbox",
    ] {
        let mut docs = Documents::load();
        docs.replace_in_amendment(term, "");
        if term == "administrator-installed sandbox" {
            assert!(
                docs.adr.contains(term),
                "Alternatives still contains the term"
            );
        }
        docs.adr
            .push_str(&format!("\n## Later section\n\n{term}\n"));
        let error = check_amendment(&docs.adr, docs.adr_0010_exists)
            .expect_err("term outside amendment must not satisfy scope");
        assert!(error.contains(term), "{error}");
    }
}

#[test]
fn link_in_later_section_does_not_satisfy_amendment() {
    let mut docs = Documents::load();
    let link = "[ADR 0010](0010-oci-execution-mode-and-base-catalog.md)";
    docs.replace_in_amendment(link, "ADR 0010");
    docs.adr
        .push_str(&format!("\n## Later section\n\n{link}\n"));
    assert!(check_amendment(&docs.adr, docs.adr_0010_exists).is_err());
}

#[test]
fn qualification_statement_is_required_inside_amendment() {
    let mut docs = Documents::load();
    let statement = "Approving the amended design is not implementation or security qualification.";
    docs.replace_in_amendment(statement, "");
    docs.adr
        .push_str(&format!("\n## Later section\n\n{statement}\n"));
    let error = check_amendment(&docs.adr, docs.adr_0010_exists)
        .expect_err("qualification outside amendment must not satisfy scope");
    assert!(error.contains("qualification statement"), "{error}");
}

#[test]
fn was_01_gate_is_required_inside_amendment() {
    let mut docs = Documents::load();
    docs.replace_in_amendment("WAS-01 (#51)", "");
    assert!(docs.adr.contains("WAS-01 (#51)"), "earlier gates remain");
    let error = check_amendment(&docs.adr, docs.adr_0010_exists)
        .expect_err("WAS-01 outside amendment must not satisfy scope");
    assert!(error.contains("WAS-01"), "{error}");
}

#[test]
fn original_criterion_two_only_line_is_rejected() {
    let mut docs = Documents::load();
    let current = docs
        .adr
        .lines()
        .find(|line| line.starts_with("Implementation, restricted-cluster feasibility"))
        .expect("migration scope fixture")
        .to_owned();
    docs.replace(
        &current,
        "Implementation, restricted-cluster feasibility and security qualification remain **unverified until WAS-01 (#51)** supplies the relevant evidence; later host/profile security gates still apply. This ADR satisfies only MIG-01 criterion (2). Contract inventory (1) and dependency classification (3) remain open and gate MIG-01 and, through it, WAS-01. Recording the decision does not authorize implementation past those gates.",
    );
    let error = check_migration_scope(&docs.adr).expect_err("original line 21 must fail");
    assert!(error.contains("criterion (2)-only"), "{error}");
}

#[test]
fn missing_scope_credit_or_remaining_gates_is_rejected() {
    for statement in [
        "This ADR satisfies MIG-01 criterion (2) and the scope-note half of criterion (4).",
        "Contract inventory (1), dependency classification (3) and the classification half of criterion (4) remain open and gate MIG-01 and, through it, WAS-01.",
    ] {
        let mut docs = Documents::load();
        docs.replace(statement, "");
        assert!(check_migration_scope(&docs.adr).is_err());
    }
}
