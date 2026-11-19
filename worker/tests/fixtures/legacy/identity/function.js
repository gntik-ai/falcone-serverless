import {invocationKey, attemptId, idempotencyKey} from 'falcone:host/invocation-context@0.2.0';
import {publish} from 'falcone:host/events@0.2.0';
export function main(params) {
  publish('orders', new Uint8Array(0));
  return {statusCode: 200, body: {invocation_key: invocationKey(), attempt_id: attemptId(), idempotency_key: idempotencyKey()}};
}
