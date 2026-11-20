// Representative workload: regular expressions over text. It imports nothing from the host.
export const handler = {
  handle(bytes) {
    let input;
    try {
      input = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(new Uint8Array(bytes)));
    } catch {
      throw { tag: 'invalid-input', val: 'invalid JSON input' };
    }
    const orders = [...input.text.matchAll(/order (\d+)/g)].map((match) => match[1]);
    const found = /(\d{4})-(\d{2})-(\d{2})/.exec(input.text);
    const date = found === null ? null : { year: found[1], month: found[2], day: found[3] };
    return new TextEncoder().encode(JSON.stringify({ orders, date }));
  }
};
