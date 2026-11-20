// Representative workload: a pure JSON transformation. It imports nothing from the host.
export const handler = {
  handle(bytes) {
    let input;
    try {
      input = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes)));
    } catch {
      throw { tag: 'invalid-input', val: 'invalid JSON input' };
    }
    const items = Array.isArray(input.items) ? input.items : [];
    const total = items.reduce((sum, item) => sum + item.price * item.quantity, 0);
    const names = items.map((item) => item.name.toUpperCase()).sort();
    return new TextEncoder().encode(JSON.stringify({ count: items.length, total, names }));
  }
};
