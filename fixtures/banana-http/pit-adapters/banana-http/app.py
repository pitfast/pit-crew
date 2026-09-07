import asyncio
import importlib

from componentize_py_types import Ok
from poll_loop import PollLoop, Sink, Stream
from wit.exports.wasi.http_v0_2 import IncomingHandler, incoming_handler
from wit.imports.wasi.http_v0_2.types import Fields, IncomingRequest, OutgoingResponse, ResponseOutparam

_handler = importlib.import_module("banana").handle


async def _run(request, response_out):
    path = request.path_with_query() or "/"
    method = request.method().__class__.__name__.removeprefix("Method_").upper()
    chunks = []
    if method not in ("GET", "HEAD"):
        reader = Stream(request.consume())
        while True:
            chunk = await reader.next()
            if chunk is None:
                break
            chunks.append(chunk)
    status, headers, body = _handler(method, path.split("?", 1)[0], b"".join(chunks))
    response = OutgoingResponse(Fields.from_list(headers))
    response.set_status_code(status)
    response_body = response.body()
    ResponseOutparam.set(response_out, Ok(response))
    sink = Sink(response_body)
    await sink.send(body)
    sink.close()


@incoming_handler.guest
class Handler(IncomingHandler):
    def handle(self, request: IncomingRequest, response_out: ResponseOutparam) -> None:
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(_run(request, response_out))

