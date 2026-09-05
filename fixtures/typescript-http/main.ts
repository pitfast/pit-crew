import {
  OutgoingBody,
  OutgoingResponse,
  IncomingBody,
  Fields,
  ResponseOutparam,
} from 'wasi:http/types@0.2.0';

export const incomingHandler = {
  handle(request: any, responseOutparam: any): void {
    const path = request.pathWithQuery() ?? '/';
    const route = path.split('?', 1)[0];
    let value = 'typescript';
    if (route === '/hello') value = 'hello';
    if (route === '/language') value = 'typescript';
    if (route === '/cpu') {
      let total = 0;
      for (let i = 0; i < 500000; i++) total = (total + i * 31) >>> 0;
      value = `typescript:${total}`;
    }
    if (route === '/echo') {
      const body = request.consume();
      const input = body.stream();
      const chunks: Uint8Array[] = [];
      const ready = input.subscribe();
      ready.block();
      ready[Symbol.dispose]?.();
      const chunk = input.read(65536n);
      if (chunk.length > 0) chunks.push(chunk);
      const total = chunks.reduce((size, chunk) => size + chunk.length, 0);
      const echoed = new Uint8Array(total);
      let offset = 0;
      for (const chunk of chunks) { echoed.set(chunk, offset); offset += chunk.length; }
      value = new TextDecoder().decode(echoed);
      input[Symbol.dispose]?.();
      const trailers = IncomingBody.finish(body);
      const trailersReady = trailers.subscribe();
      trailersReady.block();
      trailersReady[Symbol.dispose]?.();
      trailers.get();
    }
    const response = new OutgoingResponse(new Fields());
    const body = response.body();
    const stream = body.write();
    stream.blockingWriteAndFlush(new TextEncoder().encode(value));
    stream[Symbol.dispose]();
    OutgoingBody.finish(body, undefined);
    ResponseOutparam.set(responseOutparam, { tag: 'ok', val: response });
  },
};
