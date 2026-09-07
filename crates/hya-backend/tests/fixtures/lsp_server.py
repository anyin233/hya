"""Deterministic Content-Length LSP peer for runtime transport contracts."""
import json
import pathlib
import re
import sys
import urllib.parse

root = None

def send(value):
    data = json.dumps(value).encode()
    sys.stdout.buffer.write(f"Content-Length: {len(data)}\r\n\r\n".encode() + data)
    sys.stdout.buffer.flush()

while True:
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            sys.exit(0)
        if line in (b"\r\n", b"\n"):
            break
        key, value = line.decode().split(":", 1)
        headers[key.lower()] = value.strip()
    request = json.loads(sys.stdin.buffer.read(int(headers["content-length"])))
    method = request.get("method")
    params = request.get("params", {})
    if method == "initialize":
        root = pathlib.Path(urllib.parse.unquote(urllib.parse.urlparse(params["rootUri"]).path))
        result = {"capabilities": {"workspaceSymbolProvider": True, "textDocumentSync": 1}}
    elif method == "workspace/symbol":
        result = []
        for source in root.glob("*.ts"):
            for number, line in enumerate(source.read_text().splitlines()):
                match = re.search(r"export function (\w+)", line)
                if match and params.get("query", "") in match[1]:
                    result.append({"name": match[1], "kind": 12, "location": {"uri": source.as_uri(), "range": {"start": {"line": number, "character": match.start(1)}, "end": {"line": number, "character": match.end(1)}}}})
    elif method == "shutdown":
        result = None
    elif method == "exit":
        sys.exit(0)
    elif "id" not in request:
        continue
    else:
        send({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "unsupported method"}})
        continue
    if "id" in request:
        send({"jsonrpc": "2.0", "id": request["id"], "result": result})
