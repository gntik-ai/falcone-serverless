import {openObject} from 'falcone:host/storage@0.2.0';
export function main(params) {
  openObject('workspaces/forged-workspace/object');
  return {statusCode: 200};
}
