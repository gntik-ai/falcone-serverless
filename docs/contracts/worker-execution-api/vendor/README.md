# Vendored schemas

Byte-exact copies of published Falcone contracts, so the worker tests resolve their `$ref`s
offline. Never reformat or edit them; `worker/tests/execution_api_contract.rs` fails if a
sha256 changes.

| File | Source repository | Source path | Revision | sha256 |
| --- | --- | --- | --- | --- |
| capability-grants.v1.schema.json | gntik-ai/falcone | docs/contracts/serverless/capability-grants/capability-grants.v1.schema.json | a19f40ab (unchanged at falcone main on 2026-11-11) | 7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2 |
| execution-credential.v1.claims.schema.json | gntik-ai/falcone | docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0 |

`capability-grants.v1.schema.json` has `$id` `https://falcone.dev/contracts/serverless/capability-grants/v1`
and 6679 bytes. The claims schema keeps its `allowed_capabilities` `$ref` to that `$id` exactly as
published; the tests register both files under their `$id` in the validator's resource registry.
No schema owned by this contract defines its own copy of the grant shape.
