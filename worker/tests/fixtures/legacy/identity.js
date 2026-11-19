function main(params, context) {
  return {tenant: context.tenant, workspace: context.workspace,
    invocation_key: context.invocation_key, attempt_id: context.attempt_id,
    idempotency_key: context.idempotency_key};
}
