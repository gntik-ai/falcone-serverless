//! Checkpoint object v1 contract and pure metadata validation.

use serde::{Deserialize, Serialize};

/// Persisted JSON envelope; application payload bytes live behind `payload_ref`.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckpointMetadata {
    pub format_version: u32,
    pub job_id: String,
    pub logical_invocation_key: String,
    pub attempt_id: String,
    pub fencing_token: u64,
    pub sequence: u64,
    pub revision: String,
    pub profile: String,
    pub input_digest: String,
    pub checkpoint_schema: String,
    pub committed: bool,
    pub payload_ref: String,
}

/// Compatibility requirements from the job's pinned input and execution policy.
pub struct ResumeExpectations<'a> {
    pub checkpoint_schema: &'a str,
    pub input_digest: &'a str,
    pub revision: &'a str,
    pub profile: &'a str,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckpointError {
    UnsupportedFormatVersion(u32),
    StaleFence,
    FutureFence,
    NotCommitted,
    SchemaMismatch,
    InputDigestMismatch,
    RevisionMismatch,
    ProfileMismatch,
}

/// Accept only the current attempt's fence; this does not persist or commit bytes.
pub fn check_commit(meta: &CheckpointMetadata, current_fence: u64) -> Result<(), CheckpointError> {
    if meta.format_version != 1 {
        return Err(CheckpointError::UnsupportedFormatVersion(
            meta.format_version,
        ));
    }
    if meta.fencing_token < current_fence {
        return Err(CheckpointError::StaleFence);
    }
    if meta.fencing_token > current_fence {
        return Err(CheckpointError::FutureFence);
    }
    Ok(())
}

/// Check committed state and compatibility; job identity matching belongs to JOB-02.
pub fn check_resume(
    meta: &CheckpointMetadata,
    expected: &ResumeExpectations<'_>,
) -> Result<(), CheckpointError> {
    if meta.format_version != 1 {
        return Err(CheckpointError::UnsupportedFormatVersion(
            meta.format_version,
        ));
    }
    if !meta.committed {
        return Err(CheckpointError::NotCommitted);
    }
    if meta.checkpoint_schema != expected.checkpoint_schema {
        return Err(CheckpointError::SchemaMismatch);
    }
    if meta.input_digest != expected.input_digest {
        return Err(CheckpointError::InputDigestMismatch);
    }
    if meta.revision != expected.revision {
        return Err(CheckpointError::RevisionMismatch);
    }
    if meta.profile != expected.profile {
        return Err(CheckpointError::ProfileMismatch);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        CheckpointError, CheckpointMetadata, ResumeExpectations, check_commit, check_resume,
    };

    const DOC: &str = include_str!("../../docs/contracts/checkpoint-object.md");
    const RULES: [&str; 4] = [
        "A commit whose `fencing_token` differs from the current attempt fence is rejected (stale-fence rejection).",
        "Resume accepts only committed checkpoints whose `checkpoint_schema`, `input_digest`, `revision` and `profile` match the job.",
        "Guest memory is never transparently restored.",
        "The payload is opaque application bytes with no content type.",
    ];

    fn validate_rules(doc: &str) -> Result<(), String> {
        let rules = section(doc, "Rules")?;
        for required in RULES {
            if !rules.lines().any(|line| line == required) {
                return Err(format!("missing contract statement: {required}"));
            }
        }
        Ok(())
    }

    #[test]
    fn contract_doc_states_checkpoint_rules() {
        validate_rules(DOC).expect("required checkpoint rules");
        for required in RULES {
            assert_eq!(DOC.matches(required).count(), 1, "fixture anchor");
            assert!(validate_rules(&DOC.replace(required, "")).is_err());
        }
    }

    fn canonical_json(doc: &str) -> Result<&str, String> {
        if doc.matches("```json\n").count() != 1 || doc.matches("```").count() != 2 {
            return Err("expected exactly one fenced json example".into());
        }
        doc.split_once("```json\n")
            .and_then(|(_, body)| body.split_once("\n```"))
            .map(|(json, _)| json)
            .ok_or_else(|| "missing closing json fence".into())
    }

    fn metadata() -> CheckpointMetadata {
        serde_json::from_str(canonical_json(DOC).expect("canonical json")).expect("v1 metadata")
    }

    fn expectations(meta: &CheckpointMetadata) -> ResumeExpectations<'_> {
        ResumeExpectations {
            checkpoint_schema: &meta.checkpoint_schema,
            input_digest: &meta.input_digest,
            revision: &meta.revision,
            profile: &meta.profile,
        }
    }

    fn rejects_resume_change(change: impl FnOnce(&mut CheckpointMetadata), error: CheckpointError) {
        let original = metadata();
        let expected = expectations(&original);
        assert_eq!(check_resume(&original, &expected), Ok(()));
        let mut changed = original.clone();
        change(&mut changed);
        assert_eq!(check_resume(&changed, &expected), Err(error));
    }

    #[test]
    fn resume_rejects_checkpoint_schema_mismatch() {
        rejects_resume_change(
            |meta| meta.checkpoint_schema = "different".into(),
            CheckpointError::SchemaMismatch,
        );
    }

    #[test]
    fn resume_rejects_input_digest_mismatch() {
        rejects_resume_change(
            |meta| meta.input_digest = "different".into(),
            CheckpointError::InputDigestMismatch,
        );
    }

    #[test]
    fn resume_rejects_revision_mismatch() {
        rejects_resume_change(
            |meta| meta.revision = "different".into(),
            CheckpointError::RevisionMismatch,
        );
    }

    #[test]
    fn resume_rejects_profile_mismatch() {
        rejects_resume_change(
            |meta| meta.profile = "different".into(),
            CheckpointError::ProfileMismatch,
        );
    }

    #[test]
    fn canonical_example_parses() {
        let meta = metadata();
        assert_eq!(check_resume(&meta, &expectations(&meta)), Ok(()));
        let serialized = serde_json::to_string(&meta).expect("serialize metadata");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&serialized).expect("serialized json"),
            serde_json::from_str::<serde_json::Value>(canonical_json(DOC).expect("example"))
                .expect("example json")
        );
    }

    #[test]
    fn resume_rejects_uncommitted_checkpoint() {
        rejects_resume_change(|meta| meta.committed = false, CheckpointError::NotCommitted);
    }

    #[test]
    fn commit_rejects_stale_fence() {
        assert_eq!(
            check_commit(&metadata(), 8),
            Err(CheckpointError::StaleFence)
        );
    }

    #[test]
    fn commit_rejects_future_fence() {
        assert_eq!(
            check_commit(&metadata(), 6),
            Err(CheckpointError::FutureFence)
        );
    }

    #[test]
    fn commit_accepts_current_fence() {
        let mut meta = metadata();
        meta.committed = false;
        assert_eq!(check_commit(&meta, 7), Ok(()));
    }

    #[test]
    fn missing_fencing_token_is_rejected() {
        let json = canonical_json(DOC).expect("example");
        let missing = json.replace("  \"fencing_token\": 7,\n", "");
        assert_ne!(missing, json, "fixture anchor");
        assert!(serde_json::from_str::<CheckpointMetadata>(&missing).is_err());
    }

    #[test]
    fn unknown_field_is_rejected() {
        let json = canonical_json(DOC).expect("example");
        let extra = json.replacen('{', "{\n  \"unknown\": true,", 1);
        assert_ne!(extra, json, "fixture anchor");
        assert!(serde_json::from_str::<CheckpointMetadata>(&extra).is_err());
    }

    #[test]
    fn unsupported_format_version_is_rejected() {
        let json = canonical_json(DOC).expect("example");
        let future = json.replace("\"format_version\": 1", "\"format_version\": 2");
        assert_ne!(future, json, "fixture anchor");
        let meta: CheckpointMetadata = serde_json::from_str(&future).expect("typed version");
        assert_eq!(
            check_commit(&meta, meta.fencing_token),
            Err(CheckpointError::UnsupportedFormatVersion(2))
        );
        assert_eq!(
            check_resume(&meta, &expectations(&meta)),
            Err(CheckpointError::UnsupportedFormatVersion(2))
        );
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

    fn validate_required_fields(doc: &str) -> Result<(), String> {
        if !doc.lines().any(|line| line == "Contract version: v1") {
            return Err("missing Contract version: v1".into());
        }
        let fields = section(doc, "Required fields")?;
        for name in [
            "format_version",
            "job_id",
            "logical_invocation_key",
            "attempt_id",
            "fencing_token",
            "sequence",
            "revision",
            "profile",
            "input_digest",
            "checkpoint_schema",
            "committed",
            "payload_ref",
        ] {
            if !fields
                .lines()
                .any(|line| line.starts_with(&format!("- `{name}`: ")))
            {
                return Err(format!("missing required field {name}"));
            }
        }
        Ok(())
    }

    #[test]
    fn contract_doc_lists_required_fields() {
        validate_required_fields(DOC).expect("valid v1 fields");
        assert!(validate_required_fields(&DOC.replace("input_digest", "removed_digest")).is_err());
        let field = section(DOC, "Required fields")
            .expect("required fields")
            .lines()
            .find(|line| line.starts_with("- `input_digest`: "))
            .expect("input digest field");
        assert!(validate_required_fields(&DOC.replace(field, "")).is_err());
    }
}
