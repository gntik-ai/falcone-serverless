// Trusted compiler input. The statically checked main definition is appended to this module.
// No ambient filesystem, environment, process or HTTP API is provided to main.
import { log } from 'falcone:host/logging@0.2.0';
import { invocationKey, attemptId, idempotencyKey } from 'falcone:host/invocation-context@0.2.0';
import { openObject, readObject } from 'falcone:host/storage@0.2.0';

const storage = Object.freeze({ openObject, readObject });
const console = Object.freeze({
  log(...values) {
    log('info', values.map(value => typeof value === 'string' ? value : JSON.stringify(value)).join(' '), []);
  }
});

export const handler = {
  async handle(bytes) {
    let input;
    try {
      input = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes)));
    } catch {
      throw { tag: 'invalid-input', val: 'invalid JSON input' };
    }
    try {
      // Tenant/workspace are supplied only in the host's private adapter packet. Invocation
      // identifiers are read again through the WIT host, never through params or headers.
      const context = Object.freeze({
        tenant: input.context.tenant,
        workspace: input.context.workspace,
        invocation_key: invocationKey(),
        attempt_id: attemptId(),
        idempotency_key: idempotencyKey()
      });
      const value = await main(input.params, context);
      const encoded = JSON.stringify(value);
      if (encoded === undefined) throw new Error('invalid handler result');
      return new TextEncoder().encode(encoded);
    } catch {
      // A throw, rejected Promise or denied capability is always a WIT handler error.
      // Neither exception text nor binding/credential values cross this boundary.
      throw { tag: 'failed', val: 'legacy handler failed' };
    }
  }
};
