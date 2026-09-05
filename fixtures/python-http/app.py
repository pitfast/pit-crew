"""Minimal Python wasi:http/proxy fixture for PitFast."""

import asyncio

import poll_loop
from componentize_py_types import Ok
from poll_loop import PollLoop, Sink, Stream
from wit.exports.wasi.http_v0_2 import IncomingHandler, incoming_handler
from wit.imports.wasi.http_v0_2.types import (
    Fields,
    IncomingRequest,
    OutgoingBody,
    OutgoingResponse,
    ResponseOutparam,
)


@incoming_handler.guest
class Handler(IncomingHandler):
    def handle(self, request: IncomingRequest, response_out: ResponseOutparam) -> None:
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(handle_async(request, response_out))


async def handle_async(
    request: IncomingRequest, response_out: ResponseOutparam
) -> None:
    path = request.path_with_query() or "/"
    route = path.split("?", 1)[0]

    if route == "/echo":
        response = OutgoingResponse(Fields.from_list([]))
        response_body = response.body()
        ResponseOutparam.set(response_out, Ok(response))
        source = Stream(request.consume())
        sink = Sink(response_body)
        while True:
            chunk = await source.next()
            if chunk is None:
                break
            await sink.send(chunk)
        sink.close()
        return

    value = "python"
    if route == "/hello":
        value = "hello"
    elif route == "/language":
        value = "python"
    elif route == "/cpu":
        total = 0
        for i in range(500_000):
            total = (total + i * 31) & 0xFFFFFFFF
        value = f"python:{total}"

    response = OutgoingResponse(Fields.from_list([]))
    body = response.body()
    ResponseOutparam.set(response_out, Ok(response))
    sink = Sink(body)
    await sink.send(value.encode())
    sink.close()

