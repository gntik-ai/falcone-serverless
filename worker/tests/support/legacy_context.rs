//! Shared assertions for offline and trusted-image pre-instantiation rejection cases.
use falcone_serverless_worker::{
    adapters::AdapterError,
    api::{self, ExecuteRequest},
    host::Context,
};
use serde_json::{Value, json};

pub fn assert_rejected_contexts(
    request: &ExecuteRequest,
    reject: impl Fn(&ExecuteRequest, Option<&Context>) -> AdapterError,
) {
    let mut inventory: Value = serde_json::from_str(include_str!(
        "../fixtures/contract/result/valid/failure-invalid-request-non-json-body.json"
    ))
    .unwrap();
    inventory["result"]["result"]["detail"]["reason"] = json!("malformed_message");
    inventory["result"]["logs"] = json!([]);
    for case in ["missing", "tenant", "workspace", "revision"] {
        let mut pinned = request.clone();
        let mut context = pinned.plan().context;
        match case {
            "tenant" => context.tenant = "forged-context-tenant".into(),
            "workspace" => context.workspace = "forged-context-workspace".into(),
            "revision" => {
                // Bypass admit(): the adapter must enforce its own revision binding check.
                let mut wire = serde_json::to_value(&pinned).unwrap();
                wire["credential"]["revision"] = json!("forged-context-revision");
                pinned = serde_json::from_value(wire).unwrap();
            }
            _ => {}
        }
        let error = reject(&pinned, (case != "missing").then_some(&context));
        assert_eq!(error.refusal.code(), api::Code::InvalidRequest);
        assert_eq!(error.refusal.class(), api::Class::NonRetryable);
        assert_eq!(
            error.message,
            if case == "missing" {
                "execution context missing"
            } else {
                "execution context does not match the pinned revision"
            }
        );
        let (status, response) = error.response(&pinned);
        assert_eq!(status, 400);
        super::oracle::compare(
            &response,
            &serde_json::to_vec(&inventory["result"]).unwrap(),
            &[],
            &[],
        )
        .unwrap();
        api::parse_result(&response, &pinned).unwrap();
        let text = String::from_utf8(response).unwrap();
        // The inventoried identity retains the pinned projection revision. Neither the
        // forged credential revision nor any tenant/workspace value may be echoed.
        for private in [
            "forged-context",
            "forged-tenant",
            "forged-workspace",
            "tenant-synthetic-0001",
            "ws-synthetic-0001",
        ] {
            assert!(!text.contains(private), "{case}");
        }
    }
}
