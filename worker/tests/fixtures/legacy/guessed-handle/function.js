import {readObject} from 'falcone:host/storage@0.2.0';
export function main(params) {
  readObject(42);
  return {statusCode: 200};
}
