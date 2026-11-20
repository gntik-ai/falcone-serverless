// Minimal handle() example: it reads a rate-limit bucket from storage, writes the updated
// bucket back and publishes an access event. It is a real handler.handle export; the legacy
// openapi-spec-serve function is not wrapped. Imports are falcone:host interfaces only.
import { openObject, readObject, writeObject } from 'falcone:host/storage@0.2.0';
import { publish } from 'falcone:host/events@0.2.0';

const TOPIC = 'console.openapi.spec.accessed';
const encode = (value) => new TextEncoder().encode(JSON.stringify(value));
const decode = (bytes) => new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes));

export const handler = {
  handle(bytes) {
    let params;
    try {
      params = JSON.parse(decode(bytes));
    } catch {
      throw { tag: 'invalid-input', val: 'invalid JSON input' };
    }
    try {
      const minute = Math.floor(params.now / 60000);
      const bucket = openObject(`tenants/synthetic/rate/${params.workspaceId}`);
      const prior = decode(readObject(bucket));
      const current = prior === '' ? null : JSON.parse(prior);
      const sameMinute = current !== null && current.minute === minute;
      if (sameMinute && current.count >= params.limit) {
        return encode({
          statusCode: 429,
          headers: { 'Retry-After': '60' },
          body: { code: 'RATE_LIMITED', message: 'Too many requests' }
        });
      }
      writeObject(bucket, encode({ minute, count: sameMinute ? current.count + 1 : 1 }));
      publish(TOPIC, encode({
        eventType: 'openapi.spec.accessed',
        timestamp: new Date(params.now).toISOString(),
        workspaceId: params.workspaceId,
        tenantId: params.tenantId,
        specVersion: params.specVersion,
        requesterId: params.requesterId,
        format: params.format
      }));
      return encode({ statusCode: 200, body: { ok: true } });
    } catch {
      // A denied or failed host call is a handler error; no binding detail crosses the boundary.
      throw { tag: 'failed', val: 'handler failed' };
    }
  }
};
