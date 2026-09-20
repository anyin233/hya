#!/usr/bin/env python3
"""Real MCP server speaking the classic HTTP+SSE transport (2024-11-05 spec).

Single-file, stdlib-only fixture used by hya-mcp integration tests:

- GET /sse opens the ``text/event-stream`` channel; the first SSE event is
  ``event: endpoint`` whose data is the POST endpoint URI (possibly relative)
  bound to this connection's session.
- JSON-RPC requests are POSTed to that endpoint (202 Accepted); responses are
  written back onto the SSE stream as ``event: message`` frames.

Tools: ping, slow, fail_tool, rpc_error.
"""
import argparse
import json
import queue
import threading
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROTOCOL_VERSION = "2024-11-05"

TOOLS = [
    {
        "name": "ping",
        "description": "Echo msg back as pong",
        "inputSchema": {"type": "object", "properties": {"msg": {"type": "string"}}},
    },
    {
        "name": "slow",
        "description": "Sleep for N seconds then reply",
        "inputSchema": {"type": "object", "properties": {"seconds": {"type": "number"}}},
    },
    {
        "name": "fail_tool",
        "description": "Return isError=true tool result",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "rpc_error",
        "description": "Return a JSON-RPC error for the tools/call",
        "inputSchema": {"type": "object", "properties": {}},
    },
]


def dispatch_tool(name, args):
    if name == "ping":
        return {"content": [{"type": "text", "text": "pong:{}".format(args.get("msg", ""))}], "isError": False}
    if name == "slow":
        import time

        time.sleep(float(args.get("seconds", 1)))
        return {"content": [{"type": "text", "text": "slept:{}".format(args.get("seconds", 1))}], "isError": False}
    if name == "fail_tool":
        return {"content": [{"type": "text", "text": "boom: deliberate tool failure"}], "isError": True}
    return None


class Session:
    def __init__(self):
        self.id = uuid.uuid4().hex
        self.outbox = queue.Queue()

    def reply(self, payload):
        self.outbox.put(json.dumps(payload))


SESSIONS = {}
LOCK = threading.Lock()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):  # quiet
        pass

    def do_GET(self):
        if self.path.split("?")[0] != "/sse":
            self._json(404, {"error": "not found"})
            return
        session = Session()
        with LOCK:
            SESSIONS[session.id] = session
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.send_header("Connection", "close")
        self.end_headers()
        try:
            endpoint = "/messages?session_id={}".format(session.id)
            self.wfile.write("event: endpoint\ndata: {}\n\n".format(endpoint).encode())
            self.wfile.flush()
            while True:
                try:
                    data = session.outbox.get(timeout=1.0)
                except queue.Empty:
                    # Comment keepalive keeps the stream alive through proxies.
                    self.wfile.write(b": keepalive\n\n")
                    self.wfile.flush()
                    continue
                self.wfile.write("event: message\ndata: {}\n\n".format(data).encode())
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            with LOCK:
                SESSIONS.pop(session.id, None)

    def do_POST(self):
        if self.path.split("?")[0] != "/messages":
            self._json(404, {"error": "not found"})
            return
        query = self.path.split("?", 1)[1] if "?" in self.path else ""
        sid = dict(part.split("=", 1) for part in query.split("&") if "=" in part).get("session_id")
        with LOCK:
            session = SESSIONS.get(sid or "")
        if session is None:
            self._json(404, {"error": "unknown session"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(length).decode() or "{}")
        except (ValueError, UnicodeDecodeError):
            self._json(400, {"error": "parse error"})
            return

        # 202 Accepted first; the response travels on the SSE stream.
        self.send_response(202)
        self.send_header("Content-Length", "0")
        self.end_headers()

        req_id = req.get("id")
        if req_id is None:
            return
        method = req.get("method")
        if method == "initialize":
            session.reply(
                {
                    "jsonrpc": "2.0",
                    "id": req_id,
                    "result": {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "http-sse", "version": "0.1.0"},
                    },
                }
            )
        elif method == "tools/list":
            session.reply({"jsonrpc": "2.0", "id": req_id, "result": {"tools": TOOLS}})
        elif method == "tools/call":
            params = req.get("params") or {}
            name = params.get("name")
            if name == "rpc_error":
                session.reply(
                    {
                        "jsonrpc": "2.0",
                        "id": req_id,
                        "error": {"code": -32000, "message": "rpc_error: deliberate failure"},
                    }
                )
                return
            session.reply(
                {"jsonrpc": "2.0", "id": req_id, "result": dispatch_tool(name, params.get("arguments") or {})}
            )
        else:
            session.reply({"jsonrpc": "2.0", "id": req_id, "result": {}})

    def _json(self, code, payload):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("port", type=int)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    server.daemon_threads = True
    print("ready", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
