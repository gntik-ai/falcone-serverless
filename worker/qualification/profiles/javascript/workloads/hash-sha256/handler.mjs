// Representative workload: SHA-256 through Web Crypto (crypto.subtle.digest), with no polyfill
// and no Node crypto module. It imports nothing from the host.
export const handler = {
  async handle(bytes) {
    let input;
    try {
      input = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes)));
    } catch {
      throw { tag: 'invalid-input', val: 'invalid JSON input' };
    }
    const digest = await crypto.subtle.digest('SHA-256', new TextEncoder().encode(input.message));
    const hex = [...new Uint8Array(digest)].map((byte) => byte.toString(16).padStart(2, '0')).join('');
    return new TextEncoder().encode(JSON.stringify({ sha256: hex }));
  }
};
