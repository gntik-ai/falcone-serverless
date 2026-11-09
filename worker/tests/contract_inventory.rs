//! Frozen documentation contract gate; no runtime or live deployment access.

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

const FIXTURE_DIR: &str = "docs/contracts/fixtures/legacy-envelope/";
const REFERENCE_SOURCE: &str = "packages/openapi-sdk-service/actions/openapi-spec-serve.mjs";
const REFERENCE_REVISION: &str = "10a3c6e7a23c9a29b934c35f0682bbdef232099f";
const INVOKE_SOURCE: &str =
    "tests/contracts/functions-api/fixtures/legacy/invokeFunctionAction.json";
const INVOKE_REVISION: &str = "a19f40ab9e160cd522c110efd50c30b2f5e0d708";
const VALID_PATHS: [&str; 8] = [
    "401", "400", "429", "404", "403", "304", "200-json", "200-yaml",
];
const INVALID_CASES: [&str; 3] = ["missing-path", "oversized-body", "non-JSON-body"];
// Operator-pinned sources (issue #278): path, revision, sha256 of the file at that revision.
const PROVENANCE: [(&str, &str, &str); 5] = [
    (
        REFERENCE_SOURCE,
        REFERENCE_REVISION,
        "3827f5e2c7c14cc3cf5abb4eb8272d8cb67938dc4d195723efcf695bf8924ce1",
    ),
    (
        "packages/openapi-sdk-service/src/config.mjs",
        REFERENCE_REVISION,
        "749d3fefde37945e57607ae462a6a55ee193181c898ae4bbbd641f1dae0a30e5",
    ),
    (
        "packages/openapi-sdk-service/src/spec-version-repo.mjs",
        REFERENCE_REVISION,
        "5b13e9bd885954900ddf1d4e2d7b780d5e46d73e09074b31bdf5aa013f5de437",
    ),
    (
        "packages/openapi-sdk-service/src/spec-cache.mjs",
        REFERENCE_REVISION,
        "74883a947b993384a4fe3fd697e7cf4f73bdb2f36ac688ec4303bea43d17e2dc",
    ),
    (
        INVOKE_SOURCE,
        INVOKE_REVISION,
        "46d7d7e19f10b8dac1fc561317a9b0d2e012b184647534c58246605a92505664",
    ),
];
// sha256 of each frozen v1 section body as published in the v1 inventory (c9c49215...).
const FROZEN_V1_SECTIONS: [(&str, &str); 4] = [
    (
        "Contract surfaces",
        "29579db6ef6a1654a2f608ecb8d874a1f033ba38ffb6e5166d9825e479831939",
    ),
    (
        "Knative exclusion",
        "5a464feb34738e7ce99a85a3a18986cd2f7b47f6377134390403ef6778aaf042",
    ),
    (
        "Classification vocabulary",
        "70f562062725a844ce4d11b662c2972c6fb3bb071ca34f072c9bc3c07bee3a12",
    ),
    (
        "Fail-closed rule",
        "8a70944701f69b8f5c8ccd972e6fa418a6e44e0407a6699417a152d72b1c844e",
    ),
];

struct Documents {
    inventory: String,
    design: String,
    readme: String,
    adr_names: Vec<String>,
    host_wit: String,
    /// Every file under the legacy-envelope fixture directory, keyed by repository path.
    fixtures: BTreeMap<String, Vec<u8>>,
}

fn collect_files(base: &Path, dir: &Path, files: &mut BTreeMap<String, Vec<u8>>) {
    for entry in fs::read_dir(dir).expect("list fixture directory") {
        let path = entry.expect("read fixture entry").path();
        if path.is_dir() {
            collect_files(base, &path, files);
        } else {
            let relative = path.strip_prefix(base).expect("fixture under base");
            files.insert(
                format!(
                    "{FIXTURE_DIR}{}",
                    relative.to_str().expect("UTF-8 fixture name")
                ),
                fs::read(&path).expect("read fixture"),
            );
        }
    }
}

impl Documents {
    fn load() -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let docs = root.join("docs");
        let mut fixtures = BTreeMap::new();
        let base = root.join(FIXTURE_DIR);
        collect_files(&base, &base, &mut fixtures);
        Self {
            host_wit: fs::read_to_string(root.join("worker/wit/host.wit")).expect("read host.wit"),
            fixtures,
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
        validate(&self.inventory, &self.design, &self.readme, &self.adr_names)?;
        validate_legacy(&self.inventory, &self.fixtures, &self.host_wit)
    }

    fn fixture_path(name: &str) -> String {
        format!("{FIXTURE_DIR}{name}")
    }

    /// Rewrites a fixture and its pinned sha256, so only the content checks can reject it.
    fn rewrite_fixture(&mut self, name: &str, rewrite: impl FnOnce(&str) -> String) {
        let path = Self::fixture_path(name);
        let old = self
            .fixtures
            .get(&path)
            .expect("fixture to rewrite")
            .clone();
        let new = rewrite(std::str::from_utf8(&old).expect("UTF-8 fixture")).into_bytes();
        assert_ne!(old, new, "mutation of {name} changed nothing");
        self.replace(&sha256_hex(&old), &sha256_hex(&new));
        self.fixtures.insert(path, new);
    }

    fn mutate_fixture(&mut self, name: &str, old: &str, new: &str) {
        self.rewrite_fixture(name, |text| {
            assert_eq!(text.matches(old).count(), 1, "fixture anchor: {old}");
            text.replacen(old, new, 1)
        });
    }

    fn remove_fixture(&mut self, name: &str) {
        let path = Self::fixture_path(name);
        let row = self
            .inventory
            .lines()
            .find(|line| line.starts_with(&format!("| {path} |")))
            .expect("fixture row")
            .to_owned();
        self.replace(&format!("{row}\n"), "");
        self.fixtures.remove(&path).expect("fixture to remove");
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

    fn row_for(&self, path: &str) -> String {
        self.inventory
            .lines()
            .find(|line| line.starts_with(&format!("| {path} |")))
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
    if !inventory
        .lines()
        .any(|line| line == "Contract version: v1.1")
    {
        return Err("missing Contract version: v1.1".into());
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

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// The frozen v1 sections must keep their published bytes; v1.1 is additive.
fn frozen_v1_unchanged(inventory: &str) -> Result<(), String> {
    for (heading, digest) in FROZEN_V1_SECTIONS {
        if sha256_hex(section(inventory, heading)?.as_bytes()) != digest {
            return Err(format!("frozen v1 section changed: {heading}"));
        }
    }
    Ok(())
}

/// Legacy-envelope gate (MIG-01.t3): every fixture is listed, present, hash-pinned and
/// consistent with the pinned reference function; nothing unlisted is present.
fn validate_legacy(
    inventory: &str,
    files: &BTreeMap<String, Vec<u8>>,
    host_wit: &str,
) -> Result<(), String> {
    for heading in [
        "Legacy request envelope",
        "Legacy result envelope",
        "Invocation identity",
    ] {
        section(inventory, heading)?;
    }
    let fixtures = section(inventory, "Legacy envelope fixtures")?;
    require_lines(
        fixtures,
        &[
            "The valid fixtures cover exactly these paths: 401, 400, 429, 404, 403, 304, 200-json and 200-yaml.",
            "The invalid fixtures are missing-path, oversized-body and non-JSON-body.",
            "Size limit source: `max-host-to-guest-bytes` in `worker/wit/host.wit`.",
        ],
    )?;
    let provenance = table(
        section(inventory, "Legacy envelope source provenance")?,
        &["Source", "Revision", "sha256", "Role"],
    )?;
    if provenance.len() != PROVENANCE.len()
        || PROVENANCE.iter().any(|(source, revision, digest)| {
            provenance
                .get(source)
                .is_none_or(|row| row[1] != *revision || row[2] != *digest)
        })
    {
        return Err("provenance must match the operator-pinned sources".into());
    }
    let listed = table(
        fixtures,
        &["Fixture", "sha256", "Falcone source", "Revision"],
    )?;
    let valid = |name: &str| Documents::fixture_path(&format!("valid/{name}.json"));
    let invalid = |name: &str| Documents::fixture_path(&format!("invalid/{name}.json"));
    let expected: BTreeSet<String> = VALID_PATHS
        .iter()
        .map(|name| valid(name))
        .chain(INVALID_CASES.iter().map(|name| invalid(name)))
        .collect();
    if listed
        .keys()
        .map(|path| path.to_string())
        .collect::<BTreeSet<_>>()
        != expected
    {
        return Err("fixture table must list exactly the legacy fixtures".into());
    }
    if let Some(path) = files.keys().find(|path| !expected.contains(*path)) {
        return Err(format!("unlisted fixture file: {path}"));
    }
    if let Some(path) = expected.iter().find(|path| !files.contains_key(*path)) {
        return Err(format!("missing fixture file: {path}"));
    }
    let limit = host_limit(host_wit)?;
    let mut covered = BTreeSet::new();
    for (path, row) in &listed {
        let bytes = &files[*path];
        let is_valid = path.starts_with(&Documents::fixture_path("valid/"));
        let (source, revision) = if is_valid {
            (REFERENCE_SOURCE, REFERENCE_REVISION)
        } else {
            (INVOKE_SOURCE, INVOKE_REVISION)
        };
        if row[1] != sha256_hex(bytes) {
            return Err(format!("{path}: sha256 does not match the file bytes"));
        }
        if row[2] != source || row[3] != revision {
            return Err(format!("{path}: wrong Falcone source or revision"));
        }
        let name = path
            .rsplit('/')
            .next()
            .and_then(|file| file.strip_suffix(".json"))
            .ok_or("fixture must be a .json file")?;
        let fixture = parse_fixture(path, bytes)?;
        if is_valid {
            covered.insert(check_valid(name, &fixture)?);
        } else {
            check_invalid(name, &fixture, limit)?;
        }
    }
    if covered != VALID_PATHS.iter().map(|path| path.to_string()).collect() {
        return Err("a response path of the reference function is uncovered".into());
    }
    Ok(())
}

/// `max-host-to-guest-bytes` as published in worker/wit/host.wit; never a copied constant.
fn host_limit(host_wit: &str) -> Result<usize, String> {
    let limits: BTreeSet<_> = host_wit
        .lines()
        .filter_map(|line| line.trim().strip_prefix("/// max-host-to-guest-bytes:"))
        .map(|value| value.trim().parse::<usize>())
        .collect::<Result<_, _>>()
        .map_err(|_| "unparsable max-host-to-guest-bytes in host.wit")?;
    match limits.into_iter().collect::<Vec<_>>()[..] {
        [limit] => Ok(limit),
        _ => Err("host.wit must state one max-host-to-guest-bytes".into()),
    }
}

fn parse_fixture(path: &str, bytes: &[u8]) -> Result<Value, String> {
    let text = std::str::from_utf8(bytes).map_err(|_| format!("{path}: not UTF-8"))?;
    if text.contains('\r') || !text.ends_with('\n') {
        return Err(format!("{path}: must use LF and end with a final newline"));
    }
    secret_scan(text).map_err(|reason| format!("{path}: secret scan: {reason}"))?;
    let fixture = serde_json::from_str(text).map_err(|_| format!("{path}: not JSON"))?;
    check_synthetic(&fixture).map_err(|reason| format!("{path}: {reason}"))?;
    check_sensitive_fields(&fixture, false)
        .map_err(|reason| format!("{path}: secret scan: {reason}"))?;
    scan_decoded_strings(&fixture).map_err(|reason| format!("{path}: secret scan: {reason}"))?;
    Ok(fixture)
}

fn keys(value: &Value, expected: &[&str]) -> Result<(), String> {
    let actual: BTreeSet<_> = value
        .as_object()
        .ok_or("expected an object")?
        .keys()
        .map(String::as_str)
        .collect();
    if actual == expected.iter().copied().collect() {
        Ok(())
    } else {
        Err(format!("keys must be exactly {expected:?}"))
    }
}

fn check_identity(fixture: &Value) -> Result<(), String> {
    let invocation = &fixture["invocation"];
    keys(invocation, &["headers", "identity"])?;
    let identity = &invocation["identity"];
    keys(
        identity,
        &["invocation-key", "attempt-id", "idempotency-key"],
    )?;
    // idempotency-key is the Idempotency-Key header of the invoke request.
    match (
        identity["idempotency-key"].as_str(),
        invocation["headers"]["Idempotency-Key"].as_str(),
    ) {
        (Some(key), Some(header)) if key == header => Ok(()),
        _ => Err("idempotency-key must equal the Idempotency-Key header".into()),
    }
}

fn check_valid(name: &str, fixture: &Value) -> Result<String, String> {
    keys(
        fixture,
        &[
            "path",
            "decidedBy",
            "description",
            "invocation",
            "params",
            "dependencyState",
            "expected",
        ],
    )?;
    check_identity(fixture)?;
    if fixture["path"] != name {
        return Err(format!("{name}: path must equal the file name"));
    }
    let (step, result) = reference(fixture)?;
    if fixture["decidedBy"] != step {
        return Err(format!(
            "{name}: decided by {step}, not {}",
            fixture["decidedBy"]
        ));
    }
    if fixture["expected"] != result {
        return Err(format!(
            "{name}: expected differs from the reference result {result}"
        ));
    }
    // Only the state needed to reach the deciding step is recorded.
    let mut state = vec![];
    if !matches!(step, "auth" | "path") {
        state.push("rateLimit");
    }
    if matches!(step, "spec-lookup" | "tenant" | "etag" | "format") {
        state.push("currentSpec");
    }
    keys(&fixture["dependencyState"], &state)?;
    let spec = &fixture["dependencyState"]["currentSpec"];
    if let Some(json) = spec["formatJson"].as_str() {
        let hash = format!("sha256:{}", sha256_hex(json.as_bytes()));
        if spec["contentHash"] != hash {
            return Err(format!("{name}: contentHash must be sha256 of formatJson"));
        }
    }
    let status = result["statusCode"].as_u64().ok_or("statusCode")?;
    Ok(match (status, result["headers"]["Content-Type"].as_str()) {
        (200, Some("application/x-yaml")) => "200-yaml".into(),
        (200, _) => "200-json".into(),
        _ => status.to_string(),
    })
}

fn check_invalid(name: &str, fixture: &Value, limit: usize) -> Result<(), String> {
    keys(
        fixture,
        &["case", "description", "invocation", "params", "expected"],
    )?;
    check_identity(fixture)?;
    if fixture["case"] != name {
        return Err(format!("{name}: case must equal the file name"));
    }
    let expected = &fixture["expected"];
    keys(expected, &["invoked", "rejection"])?;
    if expected["invoked"] != false {
        return Err(format!("{name}: must be rejected before invocation"));
    }
    let params = &fixture["params"];
    let headers = &params["__ow_headers"];
    // Authenticated, so the rejection cannot be mistaken for the 401 path.
    if headers["x-auth-tenant-id"].as_str().is_none()
        || headers["x-auth-user-id"].as_str().is_none()
    {
        return Err(format!("{name}: must carry authentication headers"));
    }
    let (rejection, path, body) = match name {
        "missing-path" => ("envelope-invalid", false, None),
        "oversized-body" => ("body-too-large", true, Some(true)),
        "non-JSON-body" => ("body-not-json", true, Some(false)),
        _ => return Err(format!("{name}: unknown invalid case")),
    };
    if expected["rejection"] != rejection {
        return Err(format!("{name}: rejection must be {rejection}"));
    }
    if params.get("__ow_path").is_some() != path {
        return Err(format!(
            "{name}: __ow_path must be {}",
            if path { "present" } else { "omitted" }
        ));
    }
    let Some(parses) = body else {
        if params.get("__ow_body").is_some() {
            return Err(format!("{name}: must not carry a body"));
        }
        return Ok(());
    };
    if headers["content-type"] != "application/json" {
        return Err(format!("{name}: must be a JSON request"));
    }
    let body = params["__ow_body"]
        .as_str()
        .ok_or("__ow_body must be text")?;
    if serde_json::from_str::<Value>(body).is_ok() != parses {
        return Err(format!(
            "{name}: __ow_body JSON-parsability must be {parses}"
        ));
    }
    // The size limit comes from host.wit: exactly one byte over it, or within it.
    if (name == "oversized-body") != (body.len() == limit + 1) || body.len() > limit + 1 {
        return Err(format!(
            "{name}: __ow_body of {} bytes against limit {limit}",
            body.len()
        ));
    }
    Ok(())
}

/// The pinned reference function (openapi-spec-serve.mjs at 10a3c6e7) as a decision model:
/// returns the deciding step and the result envelope for the recorded request and state.
fn reference(fixture: &Value) -> Result<(&'static str, Value), String> {
    let params = &fixture["params"];
    let state = &fixture["dependencyState"];
    let none = json!({});
    let headers = params
        .get("__ow_headers")
        .filter(|v| !v.is_null())
        .unwrap_or(&none);
    let pick = |first: &str, second: &str| {
        headers
            .get(first)
            .filter(|value| !value.is_null())
            .or_else(|| headers.get(second))
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let (Some(tenant), Some(_requester)) = (
        pick("x-auth-tenant-id", "x-tenant-id"),
        pick("x-auth-user-id", "x-actor-id"),
    ) else {
        return Ok((
            "auth",
            json!({"statusCode": 401, "body": {"code": "UNAUTHORIZED", "message": "Authentication required"}}),
        ));
    };
    if extract_workspace_id(params["__ow_path"].as_str().unwrap_or("")).is_none() {
        return Ok((
            "path",
            json!({"statusCode": 400, "body": {"code": "INVALID_PATH", "message": "workspaceId missing from path"}}),
        ));
    }
    let rate = state
        .get("rateLimit")
        .ok_or("rateLimit state is required")?;
    let limit = rate["limit"].as_u64().ok_or("rateLimit.limit")?;
    let minute = rate["now"].as_u64().ok_or("rateLimit.now")? / 60_000;
    let bucket = rate.get("priorBucket").filter(|bucket| !bucket.is_null());
    if let Some(bucket) = bucket
        && bucket["minute"].as_u64() == Some(minute)
        && bucket["count"].as_u64().ok_or("priorBucket.count")? >= limit
    {
        return Ok((
            "rate-limit",
            json!({"statusCode": 429, "headers": {"Retry-After": "60"}, "body": {"code": "RATE_LIMITED", "message": "Too many requests"}}),
        ));
    }
    let spec = state
        .get("currentSpec")
        .ok_or("currentSpec state is required")?;
    if spec.is_null() {
        return Ok((
            "spec-lookup",
            json!({"statusCode": 404, "body": {"code": "SPEC_NOT_FOUND", "message": "No OpenAPI spec available for workspace"}}),
        ));
    }
    if spec["tenantId"] != tenant {
        return Ok((
            "tenant",
            json!({"statusCode": 403, "body": {"code": "FORBIDDEN", "message": "Workspace tenant mismatch"}}),
        ));
    }
    let etag = format!("\"{}\"", spec["contentHash"].as_str().ok_or("contentHash")?);
    let if_none_match = headers
        .get("if-none-match")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !if_none_match.is_empty() && if_none_match != "*" && if_none_match.trim() == etag {
        return Ok((
            "etag",
            json!({"statusCode": 304, "headers": {"ETag": etag}}),
        ));
    }
    let yaml =
        params["__ow_query"]["format"] == "yaml" || headers["accept"] == "application/x-yaml";
    Ok((
        "format",
        json!({
            "statusCode": 200,
            "headers": {
                "Content-Type": if yaml { "application/x-yaml" } else { "application/json" },
                "ETag": etag,
                "X-Spec-Version": spec["specVersion"],
                "Cache-Control": "max-age=60, must-revalidate"
            },
            "body": if yaml { &spec["formatYaml"] } else { &spec["formatJson"] }
        }),
    ))
}

/// `/\/v1\/workspaces\/([^/]+)\/openapi/` without a regex engine: the leftmost match wins.
fn extract_workspace_id(path: &str) -> Option<&str> {
    const PREFIX: &str = "/v1/workspaces/";
    path.match_indices(PREFIX).find_map(|(at, _)| {
        let (id, tail) = path[at + PREFIX.len()..].split_once('/')?;
        (!id.is_empty() && tail.starts_with("openapi")).then_some(id)
    })
}

/// Synthetic identifiers look like `tenant-synthetic-0001` or `idem-legacy-0001`.
fn is_synthetic(id: &str) -> bool {
    let parts: Vec<_> = id.split('-').collect();
    matches!(parts[..], [kind, "synthetic" | "legacy", number]
        if !kind.is_empty()
            && kind.bytes().all(|byte| byte.is_ascii_lowercase())
            && number.len() == 4
            && number.bytes().all(|byte| byte.is_ascii_digit()))
}

/// A serialized `__ow_body` is JSON text: decode it so the identifier and
/// sensitive-field checks reach what the handler would parse, not an opaque string.
fn json_body(key: &str, value: &Value) -> Option<Value> {
    if key != "__ow_body" {
        return None;
    }
    serde_json::from_str(value.as_str()?).ok()
}

fn check_synthetic(value: &Value) -> Result<(), String> {
    const ID_KEYS: [&str; 13] = [
        "x-auth-tenant-id",
        "x-tenant-id",
        "x-auth-user-id",
        "x-actor-id",
        "x-correlation-id",
        "idempotency-key",
        "invocation-key",
        "attempt-id",
        "tenantid",
        "workspaceid",
        "requesterid",
        "requestid",
        "request-id",
    ];
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                let lower = key.to_lowercase();
                if (ID_KEYS.contains(&lower.as_str()) || lower == "id")
                    && !child.as_str().is_some_and(is_synthetic)
                {
                    return Err(format!("non-synthetic identifier in {key}"));
                }
                if key == "__ow_path"
                    && let Some(path) = child.as_str()
                    && path.split('/').any(|segment| {
                        !matches!(segment, "" | "v1" | "workspaces" | "projects" | "openapi")
                            && !is_synthetic(segment)
                    })
                {
                    return Err("non-synthetic identifier in __ow_path".into());
                }
                check_synthetic(child)?;
                if let Some(body) = json_body(key, child) {
                    check_synthetic(&body)?;
                }
            }
            Ok(())
        }
        Value::Array(items) => items.iter().try_for_each(check_synthetic),
        _ => Ok(()),
    }
}

const SENSITIVE_KEYWORDS: [&str; 8] = [
    "password",
    "passwd",
    "secret",
    "token",
    "apikey",
    "api_key",
    "api-key",
    "credential",
];

/// Structural half of the secret scan: whatever the formatting, every value
/// under a sensitive key (at any depth) must be a string marked synthetic.
fn check_sensitive_fields(value: &Value, sensitive: bool) -> Result<(), String> {
    match value {
        Value::Object(map) => map.iter().try_for_each(|(key, child)| {
            let lower = key.to_lowercase();
            let sensitive = sensitive
                || SENSITIVE_KEYWORDS
                    .iter()
                    .any(|keyword| lower.contains(keyword));
            check_sensitive_fields(child, sensitive)?;
            match json_body(key, child) {
                Some(body) => check_sensitive_fields(&body, sensitive),
                None => Ok(()),
            }
        }),
        Value::Array(items) => items
            .iter()
            .try_for_each(|item| check_sensitive_fields(item, sensitive)),
        Value::String(text) if sensitive && !text.contains("synthetic") => {
            Err("sensitive value not marked synthetic".into())
        }
        Value::Number(_) | Value::Bool(_) if sensitive => {
            Err("sensitive value not marked synthetic".into())
        }
        _ => Ok(()),
    }
}

/// Decoded half of the secret scan: JSON escapes (`\/`, `\u003a`) hide a
/// credential from the raw text, so every decoded key and string is scanned too.
fn scan_decoded_strings(value: &Value) -> Result<(), String> {
    match value {
        Value::Object(map) => map.iter().try_for_each(|(key, child)| {
            secret_scan(key)?;
            scan_decoded_strings(child)?;
            // A serialized body hides its own escapes one level deeper.
            match json_body(key, child) {
                Some(body) => scan_decoded_strings(&body),
                None => Ok(()),
            }
        }),
        Value::Array(items) => items.iter().try_for_each(scan_decoded_strings),
        Value::String(text) => secret_scan(text),
        _ => Ok(()),
    }
}

/// Offline secret scan for fixtures: no scanner exists in the repository.
fn secret_scan(text: &str) -> Result<(), String> {
    let lower = text.to_ascii_lowercase();
    let token_end = |rest: &str, allowed: &dyn Fn(u8) -> bool| {
        rest.bytes().take_while(|byte| allowed(*byte)).count()
    };
    for (at, _) in lower.match_indices("-----begin") {
        if lower[at..]
            .lines()
            .next()
            .is_some_and(|line| line.contains("private key"))
        {
            return Err("private key header".into());
        }
    }
    for prefix in ["AKIA", "ASIA"] {
        for (at, _) in text.match_indices(prefix) {
            let key = &text[at + 4..];
            if token_end(key, &|b| b.is_ascii_uppercase() || b.is_ascii_digit()) >= 16 {
                return Err("AWS access key".into());
            }
        }
    }
    let base64url = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'-';
    for (at, _) in text.match_indices("eyJ") {
        let rest = &text[at..];
        let first = token_end(rest, &base64url);
        let after = rest[first..].strip_prefix('.').unwrap_or("");
        let second = token_end(after, &base64url);
        if first >= 8 && second >= 8 && after[second..].starts_with('.') {
            return Err("JWT".into());
        }
    }
    for (at, _) in lower.match_indices("bearer ") {
        let rest = lower[at + 7..].trim_start();
        if token_end(rest, &|b| {
            b.is_ascii_alphanumeric() || b"._~+/=-".contains(&b)
        }) >= 8
        {
            return Err("Bearer token".into());
        }
    }
    for (at, _) in text.match_indices("://") {
        let authority = &text[at + 3..];
        let end = authority
            .find(|c: char| {
                c == '/' || c == '?' || c == '#' || c == '"' || c == '\\' || c.is_whitespace()
            })
            .unwrap_or(authority.len());
        if authority[..end].contains('@') {
            return Err("credentialed URL".into());
        }
    }
    for keyword in SENSITIVE_KEYWORDS {
        for (at, _) in lower.match_indices(keyword) {
            let skipped = |c: char| matches!(c, '"' | '\'' | '\\') || c.is_whitespace();
            let rest = lower[at + keyword.len()..].trim_start_matches(skipped);
            let Some(rest) = rest.strip_prefix([':', '=']) else {
                continue;
            };
            let rest = rest.trim_start_matches(skipped);
            let value = &rest[..rest
                .find(|c: char| {
                    c == '"' || c == '\'' || c == ',' || c == '}' || c == '\\' || c.is_whitespace()
                })
                .unwrap_or(rest.len())];
            if !value.is_empty() && !value.starts_with(['{', '[']) && !value.contains("synthetic") {
                return Err(format!("{keyword} value not marked synthetic"));
            }
        }
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

fn rejects_because(docs: &Documents, reason: &str) {
    let error = docs.validate().expect_err("accepted a broken legacy gate");
    assert!(
        error.contains(reason),
        "rejected for {error:?}, not {reason:?}"
    );
}

#[test]
fn frozen_v1_sections_keep_their_bytes() {
    frozen_v1_unchanged(&Documents::load().inventory).expect("frozen v1 sections");
}

#[test]
fn altered_frozen_v1_row_is_rejected() {
    for (id, column, value) in [
        ("routes", 2, "source-defined"),
        ("mcp", 1, "Preserve MCP."),
        ("websocket", 3, "S1"),
    ] {
        let mut docs = Documents::load();
        docs.replace_cell(id, column, value);
        assert!(frozen_v1_unchanged(&docs.inventory).is_err(), "{id}");
    }
    let mut docs = Documents::load();
    docs.replace(
        "| revision-mismatch | blocker |",
        "| revision-mismatch | candidate |",
    );
    assert!(frozen_v1_unchanged(&docs.inventory).is_err());
}

#[test]
fn v1_version_line_is_rejected() {
    rejects_replacement("Contract version: v1.1", "Contract version: v1");
    rejects_replacement("Contract version: v1.1", "Contract version: v1.2");
}

#[test]
fn missing_legacy_section_is_rejected() {
    for heading in [
        "Legacy request envelope",
        "Legacy result envelope",
        "Invocation identity",
        "Legacy envelope fixtures",
        "Legacy envelope source provenance",
    ] {
        rejects_replacement(&format!("## {heading}"), "## Removed section");
    }
}

#[test]
fn missing_fixture_file_is_rejected() {
    let mut docs = Documents::load();
    docs.fixtures
        .remove(&Documents::fixture_path("valid/429.json"));
    rejects_because(&docs, "missing fixture file");
}

#[test]
fn altered_fixture_byte_is_rejected() {
    let mut docs = Documents::load();
    docs.fixtures
        .get_mut(&Documents::fixture_path("valid/404.json"))
        .expect("fixture")[10] ^= 1;
    rejects_because(&docs, "sha256 does not match");
}

#[test]
fn unlisted_fixture_file_is_rejected() {
    let mut docs = Documents::load();
    docs.fixtures.insert(
        Documents::fixture_path("valid/extra.json"),
        b"{}\n".to_vec(),
    );
    rejects_because(&docs, "unlisted fixture file");
}

#[test]
fn removed_fixture_row_is_rejected() {
    let mut docs = Documents::load();
    let path = Documents::fixture_path("invalid/missing-path.json");
    let row = docs
        .inventory
        .lines()
        .find(|line| line.starts_with(&format!("| {path} |")))
        .expect("row")
        .to_owned();
    docs.replace(&format!("{row}\n"), "");
    rejects_because(&docs, "must list exactly");
}

#[test]
fn uppercase_or_zero_fixture_hash_is_rejected() {
    let docs = Documents::load();
    let digest = sha256_hex(&docs.fixtures[&Documents::fixture_path("valid/401.json")]);
    for bad in [digest.to_uppercase(), "0".repeat(64)] {
        let mut docs = Documents::load();
        docs.replace(&digest, &bad);
        rejects_because(&docs, "sha256 does not match");
    }
}

#[test]
fn uncovered_response_path_is_rejected() {
    let mut docs = Documents::load();
    docs.remove_fixture("valid/304.json");
    rejects_because(&docs, "must list exactly");
    // A fixture that no longer exercises its path leaves that path uncovered.
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "valid/200-yaml.json",
        "\"format\": \"yaml\"",
        "\"format\": \"json\"",
    );
    rejects_because(&docs, "differs from the reference result");
}

#[test]
fn wrong_source_or_revision_is_rejected() {
    let docs = Documents::load();
    let path = Documents::fixture_path("invalid/missing-path.json");
    let row = docs.row_for(&path);
    rejects_replacement(&row, &row.replace(INVOKE_REVISION, REFERENCE_REVISION));
    rejects_replacement(&row, &row.replace(INVOKE_SOURCE, REFERENCE_SOURCE));
}

#[test]
fn altered_provenance_is_rejected() {
    for (_, _, digest) in PROVENANCE {
        let mut docs = Documents::load();
        docs.replace(digest, &"0".repeat(64));
        rejects_because(&docs, "provenance");
    }
    let mut docs = Documents::load();
    docs.replace(
        "46d7d7e19f10b8dac1fc561317a9b0d2e012b184647534c58246605a92505664",
        &"46d7d7e1".repeat(8),
    );
    rejects_because(&docs, "provenance");
}

#[test]
fn fixture_without_final_newline_or_with_crlf_is_rejected() {
    let mut docs = Documents::load();
    docs.rewrite_fixture("valid/401.json", |text| text.trim_end().to_owned());
    rejects_because(&docs, "final newline");
    let mut docs = Documents::load();
    docs.rewrite_fixture("valid/401.json", |text| text.replace('\n', "\r\n"));
    rejects_because(&docs, "final newline");
}

#[test]
fn non_synthetic_identifiers_are_rejected() {
    for (name, old, new) in [
        (
            "valid/401.json",
            "\"user-synthetic-0001\"",
            "\"alice@example.com\"",
        ),
        ("valid/403.json", "\"tenant-synthetic-0002\"", "\"acme\""),
        ("valid/200-json.json", "ws-synthetic-0007", "ws-prod-7"),
        ("valid/404.json", "\"corr-legacy-0001\"", "\"corr-1\""),
        (
            "invalid/missing-path.json",
            "\"inv-synthetic-0009\"",
            "\"inv-live-0009\"",
        ),
    ] {
        let mut docs = Documents::load();
        docs.rewrite_fixture(name, |text| text.replace(old, new));
        rejects_because(&docs, "non-synthetic identifier");
    }
}

#[test]
fn idempotency_key_must_come_from_the_header() {
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "valid/401.json",
        "\"idempotency-key\": \"idem-legacy-0001\"",
        "\"idempotency-key\": \"idem-legacy-0002\"",
    );
    rejects_because(&docs, "Idempotency-Key header");
}

#[test]
fn secrets_in_a_fixture_are_rejected() {
    // Credential shapes are assembled at run time so this file does not itself
    // contain a string that repository secret scanners flag.
    let pem = ["-----BEG", "IN RSA PRIVATE KEY-----"].concat();
    let aws = ["AK", "IAABCDEFGHIJKLMNOP"].concat();
    for payload in [
        pem,
        "postgres://svc:hunter2@db.internal/app".to_string(),
        aws,
        "Authorization: Bearer abcdefgh12345678".to_string(),
        "password=hunter2".to_string(),
    ] {
        let mut docs = Documents::load();
        docs.mutate_fixture(
            "valid/401.json",
            "\"description\": \"No",
            &format!("\"description\": \"{payload} No"),
        );
        rejects_because(&docs, "secret scan");
    }
}

#[test]
fn escaped_secrets_in_a_fixture_are_rejected_once_decoded() {
    let escaped_aws = ["AK", "IA\\u0041BCDEFGHIJKLMNOP"].concat();
    let escaped_pem = [
        "\\u002d\\u002d\\u002d\\u002d\\u002dBEG",
        "IN PRIVATE KEY\\u002d\\u002d\\u002d\\u002d\\u002d",
    ]
    .concat();
    for payload in [
        "postgres:\\/\\/svc:example@db.internal/app".to_string(),
        "postgres\\u003a//svc:example@db.internal/app".to_string(),
        escaped_pem,
        escaped_aws,
        "password\\u003dhunter2".to_string(),
    ] {
        let new = &format!("\"description\": \"{payload} No");
        let mut docs = Documents::load();
        docs.mutate_fixture("valid/401.json", "\"description\": \"No", new);
        rejects_because(&docs, "secret scan");
    }
}

#[test]
fn sensitive_action_params_must_be_synthetic_whatever_their_shape() {
    for new in [
        "\"__ow_query\": {},\n    \"password\": \"!hunter2\"",
        "\"__ow_query\": {},\n    \"password\": \"\\nhunter2\"",
        "\"__ow_query\": {},\n    \"password\":\n      \"hunter2\"",
        "\"__ow_query\": {},\n    \"apiKey\": 12345",
        "\"__ow_query\": {},\n    \"client_secret\": {\"value\": \"hunter2\"}",
    ] {
        let mut docs = Documents::load();
        docs.mutate_fixture("valid/401.json", "\"__ow_query\": {}", new);
        rejects_because(&docs, "secret scan");
    }
}

#[test]
fn secret_scan_recognises_credential_shapes() {
    // Assembled at run time: see secrets_in_a_fixture_are_rejected.
    let jwt = [
        "ey",
        "JhbGciOiJIUzI1NiJ9.",
        "ey",
        "JzdWIiOiIxMjM0NTY3ODkwIn0.c2lnbmF0dXJl",
    ]
    .concat();
    let pem = ["-----BEG", "IN PRIVATE KEY-----"].concat();
    let ssh_pem = ["-----BEG", "IN OPENSSH PRIVATE KEY-----"].concat();
    let sts = ["AS", "IAABCDEFGHIJKLMNOP"].concat();
    for text in [
        pem.as_str(),
        ssh_pem.as_str(),
        sts.as_str(),
        jwt.as_str(),
        "bearer abcdefgh12345678",
        "mongodb://user:pw@host/db",
        "https://token@example.com/x",
        "{\"password\": \"hunter2\"}",
        "{\"apiKey\": \"abc123\"}",
        "client_secret=abc123",
        "{\"password\": \"!hunter2\"}",
        "{\"password\":\n\"hunter2\"}",
        "password:\nhunter2",
        "{\\\"token\\\":\\\"abc123\\\"}",
    ] {
        assert!(secret_scan(text).is_err(), "accepted {text}");
    }
    for text in [
        "{\"password\": \"synthetic-password\"}",
        "{\"tokens\": 5}",
        "https://example.com/v1/workspaces/ws-synthetic-0001/openapi",
        "Bearer short",
        "{\"x-auth-user-id\": \"user-synthetic-0001\"}",
    ] {
        secret_scan(text).expect(text);
    }
}

#[test]
fn oversized_body_must_exceed_the_host_limit_by_one_byte() {
    for (old, new) in [("AAAA", "AAA"), ("AAAA", "AAAAA")] {
        let mut docs = Documents::load();
        docs.rewrite_fixture("invalid/oversized-body.json", |text| {
            text.replacen(old, new, 1)
        });
        rejects_because(&docs, "bytes against limit 1048576");
    }
}

#[test]
fn identifiers_and_secrets_inside_a_serialized_body_are_checked() {
    for (extra, reason) in [
        (
            r#","tenantId":"tenant-production""#,
            "non-synthetic identifier",
        ),
        (r#","password":"hunter2""#, "not marked synthetic"),
    ] {
        let mut docs = Documents::load();
        docs.rewrite_fixture("invalid/oversized-body.json", |text| {
            // Same body length: trim the padding by what the member adds.
            let escaped = extra.replace('"', "\\\"");
            text.replacen(&"A".repeat(extra.len()), "", 1).replacen(
                "\\\"}\"",
                &format!("\\\"{escaped}}}\""),
                1,
            )
        });
        rejects_because(&docs, reason);
    }
}

#[test]
fn escaped_secrets_inside_a_serialized_body_are_rejected_once_decoded() {
    // The URL's slashes are JSON-escaped inside the serialized body (and the
    // body is itself a JSON string), so neither the raw text nor the outer
    // decoded strings contain it; only the decoded body does.
    let extra = r#","note":"postgres:\/\/svc:example@db.internal/app""#;
    let mut docs = Documents::load();
    docs.rewrite_fixture("invalid/oversized-body.json", |text| {
        // Same body length: trim the padding by what the member adds.
        let escaped = extra.replace('\\', "\\\\").replace('"', "\\\"");
        let padded = text.replacen(&"A".repeat(extra.len()), "", 1);
        assert_ne!(padded, text, "padding not found");
        padded.replacen("\\\"}\"", &format!("\\\"{escaped}}}\""), 1)
    });
    rejects_because(&docs, "secret scan");
}

#[test]
fn oversized_limit_is_read_from_host_wit() {
    let mut docs = Documents::load();
    assert_eq!(host_limit(&docs.host_wit), Ok(1_048_576));
    docs.host_wit = docs.host_wit.replace("1048576", "2097152");
    rejects_because(&docs, "against limit 2097152");
    docs.host_wit = docs
        .host_wit
        .replace("max-host-to-guest-bytes", "max-bytes");
    rejects_because(&docs, "max-host-to-guest-bytes");
    docs.host_wit = "/// max-host-to-guest-bytes: 1\n/// max-host-to-guest-bytes: 2\n".into();
    rejects_because(&docs, "one max-host-to-guest-bytes");
}

#[test]
fn inventory_must_cite_the_limit_source() {
    rejects_replacement(
        "Size limit source: `max-host-to-guest-bytes` in `worker/wit/host.wit`.",
        "Size limit source: a constant.",
    );
}

#[test]
fn invalid_fixtures_must_be_rejected_before_invocation() {
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "invalid/missing-path.json",
        "\"invoked\": false",
        "\"invoked\": true",
    );
    rejects_because(&docs, "before invocation");
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "invalid/missing-path.json",
        "\"__ow_query\": {}",
        "\"__ow_path\": \"/v1/workspaces/ws-synthetic-0009/openapi\"",
    );
    rejects_because(&docs, "__ow_path must be omitted");
    let mut docs = Documents::load();
    docs.mutate_fixture("invalid/non-JSON-body.json", ": not-json\"", ": 1}\"");
    rejects_because(&docs, "parsability");
    let mut docs = Documents::load();
    docs.rewrite_fixture("invalid/oversized-body.json", |text| {
        text.replacen("{\\\"data", "{\\\"data\\\"", 1)
    });
    rejects_because(&docs, "parsability");
}

#[test]
fn valid_400_and_missing_path_stay_distinct() {
    let docs = Documents::load();
    let valid = &docs.fixtures[&Documents::fixture_path("valid/400.json")];
    let valid: Value = serde_json::from_slice(valid).expect("400 fixture");
    assert!(valid["params"].get("__ow_path").is_some());
    assert_eq!(valid["expected"]["statusCode"], 400);
    let invalid = &docs.fixtures[&Documents::fixture_path("invalid/missing-path.json")];
    let invalid: Value = serde_json::from_slice(invalid).expect("missing-path fixture");
    assert!(invalid["params"].get("__ow_path").is_none());
    assert_eq!(invalid["expected"]["invoked"], false);
}

#[test]
fn reference_decisions_follow_the_pinned_order() {
    // 429 needs the bucket at the limit within the same minute.
    for (old, new) in [("\"count\": 60", "\"count\": 59"), ("29453760", "29453761")] {
        let mut docs = Documents::load();
        docs.mutate_fixture("valid/429.json", old, new);
        rejects_because(&docs, "currentSpec state is required");
    }
    // A tenant header makes the request pass auth, so the 401 fixture no longer decides.
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "valid/401.json",
        "\"x-auth-user-id\"",
        "\"x-auth-tenant-id\": \"tenant-synthetic-0001\",\n      \"x-auth-user-id\"",
    );
    rejects_because(&docs, "rateLimit state is required");
    // The ETag must be the quoted hash; the unquoted hash does not match.
    let mut docs = Documents::load();
    docs.rewrite_fixture("valid/304.json", |text| {
        text.replacen("\"if-none-match\": \"\\\"", "\"if-none-match\": \"", 1)
    });
    rejects_because(&docs, "decided by format");
    // contentHash is the sha256 of formatJson.
    let mut docs = Documents::load();
    docs.rewrite_fixture("valid/200-json.json", |text| {
        text.replace("sha256:0", "sha256:1")
    });
    rejects_because(&docs, "contentHash must be sha256");
    // State the deciding step does not need must not be recorded.
    let mut docs = Documents::load();
    docs.mutate_fixture(
        "valid/401.json",
        "\"dependencyState\": {}",
        "\"dependencyState\": {\"currentSpec\": null}",
    );
    rejects_because(&docs, "keys must be exactly");
}

#[test]
fn readme_describes_inventory_as_v1_1() {
    let docs = Documents::load();
    assert!(
        docs.readme
            .contains("frozen Falcone contract inventory v1.1")
    );
}
