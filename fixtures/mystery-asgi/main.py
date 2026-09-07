"""An intentionally unknown framework: only the ASGI interface matters."""


async def app(scope, receive, send):
    body = b"mystery-asgi" if scope["path"] == "/language" else b"hello"
    if scope["path"] == "/echo":
        body = (await receive()).get("body", b"")
    await send({"type": "http.response.start", "status": 200, "headers": []})
    await send({"type": "http.response.body", "body": body})

