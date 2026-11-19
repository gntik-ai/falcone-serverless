// Trusted adapter glue compiled into synthesized fixtures, never tenant source.
import {log} from 'falcone:host/logging@0.2.0';
import {main} from './function.js';

console.log = (...values) => log('info', values.map(value =>
  typeof value === 'string' ? value : JSON.stringify(value)).join(' '), []);

export const handler = {
  async handle(params) {
    try {
      const result = await main(JSON.parse(new TextDecoder().decode(params)));
      return new TextEncoder().encode(JSON.stringify(result));
    } catch {
      // No exception text, params or identity enters the failure diagnostic.
      throw {tag: 'failed', val: 'handler failed'};
    }
  }
};
