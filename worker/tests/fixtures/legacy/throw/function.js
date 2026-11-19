export function main(params) {
  console.log('handler started');
  console.log('handler finished');
  throw new Error('synthetic failure');
}
