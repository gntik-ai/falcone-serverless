//! Frozen documentation contract gate; no runtime or live deployment access.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

struct Documents {
    inventory: String,
    design: String,
    readme: String,
    adr_names: Vec<String>,
}

impl Documents {
    fn load() -> Self {
        let docs = Path::new(env!("CARGO_MANIFEST_DIR")).join("../docs");
        Self {
            inventory: fs::read_to_string(docs.join("contracts/falcone-contract-inventory.md"))
                .expect("read inventory"),
            design: fs::read_to_string(docs.join("falcone-serverless-design.md"))
                .expect("read design"),
            readme: fs::read_to_string(docs.join("README.md")).expect("read README"),
            adr_names: fs::read_dir(docs.join("adr"))
                .expect("list ADRs")
                .map(|entry| {
                    entry
                        .expect("read ADR entry")
                        .file_name()
                        .into_string()
                        .expect("UTF-8 ADR name")
                })
                .collect(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        validate(&self.inventory, &self.design, &self.readme, &self.adr_names)
    }

    fn replace(&mut self, old: &str, new: &str) {
        assert_eq!(
            self.inventory.matches(old).count(),
            1,
            "fixture anchor: {old}"
        );
        self.inventory = self.inventory.replacen(old, new, 1);
    }

    fn row(&self, id: &str) -> String {
        self.inventory
            .lines()
            .find(|line| line.starts_with(&format!("| {id} |")))
            .expect("fixture row")
            .to_owned()
    }

    fn replace_cell(&mut self, id: &str, column: usize, value: &str) {
        let old = self.row(id);
        let mut cells: Vec<_> = old.trim_matches('|').split('|').map(str::trim).collect();
        cells[column] = value;
        let new = format!("| {} |", cells.join(" | "));
        self.replace(&old, &new);
    }
}

fn validate(
    inventory: &str,
    design: &str,
    readme: &str,
    adr_names: &[String],
) -> Result<(), String> {
    if !inventory.lines().any(|line| line == "Contract version: v1") {
        return Err("missing Contract version: v1".into());
    }
    let rows = table(
        section(inventory, "Contract surfaces")?,
        &["ID", "Contract", "Status", "Source"],
    )?;
    let required = [
        "routes",
        "api-versions",
        "schemas",
        "error-codes",
        "pagination",
        "idempotency",
        "urls",
        "data",
        "main-params-envelope",
        "functions-invoke",
        "authorization",
        "websocket",
        "mcp",
    ];
    if rows.keys().copied().collect::<BTreeSet<_>>() != BTreeSet::from(required) {
        return Err("contract row IDs must match v1".into());
    }
    let sources: BTreeSet<_> = section(design, "Sources and evidence")?
        .lines()
        .filter_map(|line| {
            line.strip_prefix("- **")
                .and_then(|line| line.split_once(":**"))
        })
        .map(|(id, _)| id)
        .collect();
    let headings = design_headings(design);
    for (id, row) in &rows {
        if !matches!(row[2], "observed" | "source-defined" | "proposed") {
            return Err(format!("{id}: expected exactly one status"));
        }
        if row[2] == "observed" || (*id == "mcp" && row[2] != "proposed") {
            return Err(format!("{id}: v1 has no live inspection evidence"));
        }
        if *id == "authorization" && !row[1].contains("A project is a Falcone workspace") {
            return Err("authorization must identify the workspace as project boundary".into());
        }
        for source in row[3].split(';').map(str::trim) {
            if !valid_source(source, &sources, &headings, adr_names) {
                return Err(format!("{id}: unresolved source {source:?}"));
            }
        }
    }
    let exclusion = section(inventory, "Knative exclusion")?;
    if !exclusion.contains("Knative APIs") || !exclusion.contains("Knative Service URLs") {
        return Err("exclusion must name Knative APIs and Knative Service URLs".into());
    }
    require_lines(
        section(inventory, "Classification vocabulary")?,
        &[
            "- language",
            "- packages/package manager",
            "- native libraries or binaries",
            "- Dockerfile/OCI image: feeds oci_verdict; not an automatic blocker.",
            "- install hooks/build scripts",
            "- process/OS interfaces",
            "- Knative API/URL use",
            "Verdicts: `candidate`, `needs-port`, `unsupported`, `unclassified`.",
            "Required fields: `function-id`, `function-revision`, `wasm_verdict`, `oci_verdict`, `target_mode`.",
            "target_mode values: `wasm|oci`.",
            "target_mode is never derived from a failed wasm verdict.",
        ],
    )?;
    let fail_closed = section(inventory, "Fail-closed rule")?;
    require_lines(
        fail_closed,
        &[
            "Evaluate all blockers before returning candidate, regardless of API parity.",
            "Select wasm_verdict for target_mode wasm and oci_verdict for target_mode oci.",
            "For target_mode oci, the workspace must be OCI-authorized at cutover.",
        ],
    )?;
    let rules = table(fail_closed, &["Condition", "Outcome"])?;
    let expected = [
        ("no-classification", "blocker"),
        ("revision-mismatch", "blocker"),
        ("verdict-not-candidate", "blocker"),
        ("oci-not-authorized", "blocker"),
        ("candidate-at-pinned-revision", "candidate"),
    ];
    if rules.len() != expected.len()
        || expected
            .iter()
            .any(|(condition, outcome)| rules.get(condition).is_none_or(|row| row[1] != *outcome))
    {
        return Err("fail-closed conditions/outcomes must match v1".into());
    }
    if !readme.contains("contracts/falcone-contract-inventory.md") {
        return Err("README must link the inventory".into());
    }
    Ok(())
}

fn require_lines(text: &str, required: &[&str]) -> Result<(), String> {
    for expected in required {
        if !text.lines().any(|line| line == *expected) {
            return Err(format!("missing contract statement: {expected}"));
        }
    }
    Ok(())
}

fn design_headings(design: &str) -> Vec<(usize, &str)> {
    let mut section = 0;
    let mut headings = Vec::new();
    for line in design.lines() {
        if let Some(heading) = line.strip_prefix("## ")
            && let Some((number, _)) = heading.split_once(". ")
            && let Ok(number) = number.parse::<usize>()
        {
            section = number;
        }
        if line.starts_with('#') {
            headings.push((section, line.trim_start_matches('#').trim()));
        }
    }
    headings
}

fn valid_source(
    source: &str,
    sources: &BTreeSet<&str>,
    headings: &[(usize, &str)],
    adr_names: &[String],
) -> bool {
    if source.starts_with('S') {
        return sources.contains(source);
    }
    if let Some(citation) = source.strip_prefix("design §") {
        let Some((number, cited_heading)) = citation.split_once(' ') else {
            return false;
        };
        let Ok(number) = number.parse::<usize>() else {
            return false;
        };
        return !cited_heading.is_empty()
            && headings.iter().any(|(section, heading)| {
                *section == number
                    && (*heading == cited_heading
                        || heading
                            .strip_prefix(cited_heading)
                            .is_some_and(|tail| tail.starts_with('.')))
            });
    }
    if let Some(number) = source.strip_prefix("ADR ") {
        return number.len() == 4
            && number.bytes().all(|byte| byte.is_ascii_digit())
            && adr_names
                .iter()
                .any(|name| name.starts_with(&format!("{number}-")) && name.ends_with(".md"));
    }
    false
}

fn section<'a>(text: &'a str, heading: &str) -> Result<&'a str, String> {
    let marker = format!("## {heading}");
    let start = text
        .split_inclusive('\n')
        .scan(0, |offset, line| {
            *offset += line.len();
            Some((*offset, line.trim_end_matches(['\r', '\n'])))
        })
        .find(|(_, line)| *line == marker)
        .map(|(offset, _)| offset)
        .ok_or_else(|| format!("missing section {heading}"))?;
    let body = &text[start..];
    Ok(body.split("\n## ").next().unwrap_or(body))
}

fn table<'a>(text: &'a str, header: &[&str]) -> Result<BTreeMap<&'a str, Vec<&'a str>>, String> {
    let mut lines = text.lines().filter(|line| line.starts_with('|'));
    let cells = |line: &'a str| -> Vec<&'a str> {
        line.trim_matches('|').split('|').map(str::trim).collect()
    };
    if lines.next().map(cells).as_deref() != Some(header) {
        return Err(format!("missing table header {header:?}"));
    }
    let separator = lines.next().map(cells).ok_or("missing table separator")?;
    if separator.len() != header.len() || separator.iter().any(|cell| *cell != "---") {
        return Err("invalid table separator".into());
    }
    let mut rows = BTreeMap::new();
    for line in lines {
        let row = cells(line);
        if row.len() != header.len() || row[0].is_empty() {
            return Err(format!("invalid table row: {line}"));
        }
        if rows.insert(row[0], row).is_some() {
            return Err("duplicate table row".into());
        }
    }
    Ok(rows)
}

#[test]
fn committed_documents_pass() {
    Documents::load()
        .validate()
        .expect("valid frozen inventory");
}

#[test]
fn sources_after_blank_line_and_heading_forms_resolve() {
    let mut docs = Documents::load();
    for source in [
        "S25; S26; S30",
        "design §0 Evidence and boundaries",
        "design §2 API. API and developer experience",
        "design §4 MIG",
        "ADR 0001",
    ] {
        docs.replace_cell("routes", 3, source);
        docs.validate().expect("valid source form");
    }
}

#[test]
fn missing_version_is_rejected() {
    let mut docs = Documents::load();
    docs.replace("Contract version: v1", "");
    assert!(docs.validate().is_err());
}

#[test]
fn missing_websocket_is_rejected() {
    let mut docs = Documents::load();
    let row = docs.row("websocket");
    docs.replace(&row, "");
    assert!(docs.validate().is_err());
}

#[test]
fn missing_status_is_rejected() {
    rejects_cell("routes", 2, "");
}

#[test]
fn two_statuses_are_rejected() {
    rejects_cell("routes", 2, "source-defined proposed");
}

#[test]
fn verified_status_is_rejected() {
    rejects_cell("routes", 2, "verified");
}

#[test]
fn observed_row_is_rejected() {
    rejects_cell("routes", 2, "observed");
}

#[test]
fn source_defined_mcp_is_rejected() {
    rejects_cell("mcp", 2, "source-defined");
}

#[test]
fn authorization_without_workspace_is_rejected() {
    rejects_cell("authorization", 1, "A project is a tenant resource.");
}

#[test]
fn empty_source_is_rejected() {
    rejects_cell("routes", 3, "");
}

#[test]
fn free_text_source_is_rejected() {
    rejects_cell("routes", 3, "repository inspection");
}

#[test]
fn unknown_s_reference_is_rejected() {
    rejects_cell("routes", 3, "S99");
}

#[test]
fn nonexistent_design_heading_is_rejected() {
    rejects_cell("routes", 3, "design §9 Nonexistent");
}

#[test]
fn heading_in_wrong_section_is_rejected() {
    rejects_cell("routes", 3, "design §3 API");
}

#[test]
fn nonexistent_adr_is_rejected() {
    rejects_cell("routes", 3, "ADR 0099");
}

#[test]
fn invalid_token_after_valid_source_is_rejected() {
    rejects_cell("routes", 3, "S1; repository inspection");
}

#[test]
fn missing_knative_section_is_rejected() {
    rejects_replacement("## Knative exclusion", "## Removed exclusion");
}

#[test]
fn knative_without_urls_is_rejected() {
    rejects_replacement(
        "Raw Knative APIs and Knative Service URLs are outside the replacement scope.",
        "Raw Knative APIs are outside the replacement scope.",
    );
}

#[test]
fn missing_unclassified_is_rejected() {
    rejects_replacement(
        "Verdicts: `candidate`, `needs-port`, `unsupported`, `unclassified`.",
        "Verdicts: `candidate`, `needs-port`, `unsupported`.",
    );
}

#[test]
fn missing_function_revision_is_rejected() {
    rejects_replacement("`function-revision`, ", "");
}

#[test]
fn missing_oci_verdict_is_rejected() {
    rejects_replacement("`oci_verdict`, ", "");
}

#[test]
fn missing_target_mode_is_rejected() {
    rejects_replacement("`oci_verdict`, `target_mode`.", "`oci_verdict`.");
}

#[test]
fn missing_process_dimension_is_rejected() {
    rejects_replacement("- process/OS interfaces", "");
}

#[test]
fn dockerfile_as_blocker_is_rejected() {
    rejects_replacement(
        "- Dockerfile/OCI image: feeds oci_verdict; not an automatic blocker.",
        "- Dockerfile/OCI image: blocker.",
    );
}

#[test]
fn implicit_mode_selection_is_rejected() {
    rejects_replacement(
        "target_mode is never derived from a failed wasm verdict.",
        "target_mode is derived from a failed wasm verdict.",
    );
}

#[test]
fn missing_fail_closed_table_is_rejected() {
    rejects_replacement("| Condition | Outcome |", "");
}

#[test]
fn missing_no_classification_is_rejected() {
    rejects_replacement("| no-classification | blocker |", "");
}

#[test]
fn revision_mismatch_as_candidate_is_rejected() {
    rejects_replacement(
        "| revision-mismatch | blocker |",
        "| revision-mismatch | candidate |",
    );
}

#[test]
fn missing_oci_authorization_is_rejected() {
    rejects_replacement("| oci-not-authorized | blocker |", "");
}

#[test]
fn oci_outcome_is_rejected() {
    rejects_replacement(
        "| verdict-not-candidate | blocker |",
        "| verdict-not-candidate | oci |",
    );
}

#[test]
fn fallback_outcome_is_rejected() {
    rejects_replacement(
        "| verdict-not-candidate | blocker |",
        "| verdict-not-candidate | fallback |",
    );
}

#[test]
fn wrong_selected_verdict_is_rejected() {
    rejects_replacement(
        "Select wasm_verdict for target_mode wasm and oci_verdict for target_mode oci.",
        "Select wasm_verdict for every target_mode.",
    );
}

#[test]
fn authorization_without_cutover_check_is_rejected() {
    rejects_replacement(
        "For target_mode oci, the workspace must be OCI-authorized at cutover.",
        "For target_mode oci, the workspace was OCI-authorized at classification.",
    );
}

#[test]
fn readme_without_inventory_link_is_rejected() {
    let mut docs = Documents::load();
    let path = "contracts/falcone-contract-inventory.md";
    assert!(docs.readme.contains(path), "README fixture anchor");
    docs.readme = docs.readme.replace(path, "removed-inventory.md");
    assert!(docs.validate().is_err());
}

fn rejects_replacement(old: &str, new: &str) {
    let mut docs = Documents::load();
    docs.replace(old, new);
    assert!(docs.validate().is_err(), "accepted replacement of {old}");
}

fn rejects_cell(id: &str, column: usize, value: &str) {
    let mut docs = Documents::load();
    docs.replace_cell(id, column, value);
    assert!(
        docs.validate().is_err(),
        "accepted {id} cell {column}: {value}"
    );
}
