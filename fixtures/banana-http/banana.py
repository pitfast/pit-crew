def handle(method, path, body):
    if path == "/language":
        return 200, [("content-type", b"text/plain")], b"banana"
    if path == "/echo":
        return 200, [("content-type", b"text/plain")], body
    return 200, [("content-type", b"text/plain")], b"hello"

