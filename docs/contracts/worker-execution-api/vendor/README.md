# Vendored falcone contract schemas

Both files are copied byte for byte from gntik-ai/falcone and must not be edited or reformatted.
The execution API schemas reference them by `$id`; conformance tests register them with the
validator so every `$ref` resolves offline. A test fails if either sha256 changes.

| File | Source repository | Source path | Revision | sha256 |
| --- | --- | --- | --- | --- |
| capability-grants.v1.schema.json | gntik-ai/falcone | docs/contracts/serverless/capability-grants/capability-grants.v1.schema.json | a19f40ab (unchanged at falcone main on 2026-11-11) | 7f93939d0cc4b87d0bb6ef9af145a8b68a060ab918dc96ffc6247086a2b25dd2 |
| execution-credential.v1.claims.schema.json | gntik-ai/falcone | docs/contracts/serverless/execution-credential/execution-credential.v1.claims.schema.json | a19f40ab9e160cd522c110efd50c30b2f5e0d708 | 9c98ba16548e724004fbf17c38d3d689fd7183d25b386c83e61b13ff8b31ecf0 |

Revision `a19f40ab` is falcone main `a19f40ab9e160cd522c110efd50c30b2f5e0d708`.

The capability grants file is the canonical grants contract (`falcone:host/*@0.1.0`). The claims schema
keeps its `allowed_capabilities` reference to it unchanged. The worker request grants are a different
list at `@0.2.0` and validate against `urn:falcone:contracts:revision-runtime-manifest:1`, not this file.
