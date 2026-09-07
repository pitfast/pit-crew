import { IncomingBody, OutgoingBody, OutgoingResponse, Fields, ResponseOutparam } from 'wasi:http/types@0.2.0';

const assets = new Map([
  ["/assets/app.css", { type: "text/css; charset=utf-8", bytes: new Uint8Array([109,97,105,110,32,123,32,99,111,108,111,114,58,32,115,116,101,101,108,98,108,117,101,59,32,125,10]) }],
  ["/assets/app.js", { type: "text/javascript; charset=utf-8", bytes: new Uint8Array([100,111,99,117,109,101,110,116,46,113,117,101,114,121,83,101,108,101,99,116,111,114,40,39,109,97,105,110,39,41,46,100,97,116,97,115,101,116,46,114,101,97,100,121,32,61,32,39,116,114,117,101,39,59,10]) }],
  ["/index.html", { type: "text/html; charset=utf-8", bytes: new Uint8Array([60,33,100,111,99,116,121,112,101,32,104,116,109,108,62,60,104,116,109,108,62,60,104,101,97,100,62,60,108,105,110,107,32,114,101,108,61,34,115,116,121,108,101,115,104,101,101,116,34,32,104,114,101,102,61,34,47,97,115,115,101,116,115,47,97,112,112,46,99,115,115,34,62,60,47,104,101,97,100,62,60,98,111,100,121,62,60,109,97,105,110,62,80,105,116,70,97,115,116,32,115,116,97,116,105,99,32,119,101,98,32,118,50,60,47,109,97,105,110,62,60,115,99,114,105,112,116,32,115,114,99,61,34,47,97,115,115,101,116,115,47,97,112,112,46,106,115,34,62,60,47,115,99,114,105,112,116,62,60,47,98,111,100,121,62,60,47,104,116,109,108,62,10]) }],
]);

function response(path, status, body, type) {
  const headers = new Headers({ 'content-type': type });
  return new Response(body, { status, headers });
}

export const incomingHandler = {
  async handle(request, responseOutparam) {
    const pathWithQuery = request.pathWithQuery() ?? '/';
    const encodedPath = pathWithQuery.split('?', 1)[0];
    let path;
    try { path = decodeURIComponent(encodedPath); } catch (_) { path = encodedPath; }
    if (path.split('/').includes('..')) {
      const result = response(path, 404, 'not found', 'text/plain; charset=utf-8');
      const outgoing = new OutgoingResponse(Fields.fromList([['content-type', new TextEncoder().encode('text/plain; charset=utf-8')]]));
      outgoing.setStatusCode(result.status);
      const body = outgoing.body();
      const stream = body.write();
      stream.blockingWriteAndFlush(new TextEncoder().encode('not found'));
      stream[Symbol.dispose]();
      OutgoingBody.finish(body, undefined);
      ResponseOutparam.set(responseOutparam, { tag: 'ok', val: outgoing });
      return;
    }
    const asset = assets.get(path) ?? (path.endsWith('/') ? assets.get(path + 'index.html') : undefined);
    if (!asset) {
      const outgoing = new OutgoingResponse(Fields.fromList([['content-type', new TextEncoder().encode('text/plain; charset=utf-8')]]));
      outgoing.setStatusCode(404);
      const body = outgoing.body();
      const stream = body.write();
      stream.blockingWriteAndFlush(new TextEncoder().encode('not found'));
      stream[Symbol.dispose]();
      OutgoingBody.finish(body, undefined);
      ResponseOutparam.set(responseOutparam, { tag: 'ok', val: outgoing });
      return;
    }
    const outgoing = new OutgoingResponse(Fields.fromList([['content-type', new TextEncoder().encode(asset.type)]]));
    outgoing.setStatusCode(200);
    const body = outgoing.body();
    const stream = body.write();
    stream.blockingWriteAndFlush(asset.bytes);
    stream[Symbol.dispose]();
    OutgoingBody.finish(body, undefined);
    ResponseOutparam.set(responseOutparam, { tag: 'ok', val: outgoing });
  },
};
