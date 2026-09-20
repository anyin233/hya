#!/usr/bin/env python3
"""Real MCP server speaking Streamable HTTP (2025-03-26/2025-06-18 spec).

Single-file, stdlib-only fixture used by hya-mcp integration tests. Serves:

- POST /mcp with JSON-RPC messages; replies ``application/json`` by default and
  ``text/event-stream`` for tools/call of the ``stream_add`` tool (both response
  modes are legal per spec, so the client must accept either).
- ``Mcp-Session-Id`` response header on initialize; subsequent requests must
  echo it or get HTTP 404 (session not found). ``DELETE /mcp`` terminates.
- ``--stateless`` runs the 2026-07-28 stateless paradigm: no session ids, and
  the initialize handshake is answered but never required.

Tools: ping, add, stream_add, slow, fail_tool, rpc_error, tasks_run (tasks
extension semantics), tasks_result.
"""
import argparse
import json
import sys
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PROTOCOL_VERSION = "2025-06-18"

TOOLS = [
    {
        "name": "ping",
        "description": "Echo msg back as pong",
        "inputSchema": {"type": "object", "properties": {"msg": {"type": "string"}}},
    },
    {
        "name": "add",
        "description": "Add two numbers (JSON response)",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
        },
    },
    {
        "name": "stream_add",
        "description": "Add two numbers (SSE-streamed response)",
        "inputSchema": {
            "type": "object",
            "properties": {"a": {"type": "number"}, "b": {"type": "number"}},
        },
    },
    {
        "name": "slow",
        "description": "Sleep for N seconds then reply (background tests)",
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


def tool_result(text, is_error=False):
    return {"content": [{"type": "text", "text": text}], "isError": is_error}


def dispatch_tool(name, args):
    if name == "ping":
        return tool_result("pong:{}".format(args.get("msg", "")))
    if name == "add":
        return tool_result("sum:{}".format(args.get("a", 0) + args.get("b", 0)))
    if name == "stream_add":
        return tool_result("stream_sum:{}".format(args.get("a", 0) + args.get("b", 0)))
    if name == "slow":
        seconds = float(args.get("seconds", 1))
        time.sleep(seconds)
        return tool_result("slept:{}".format(args.get("seconds", 1)))
    if name == "fail_tool":
        return tool_result("boom: deliberate tool failure", is_error=True)
    if name == "rpc_error":
        return None  # caller turns this into a JSON-RPC error
    return tool_result("unknown tool: {}".format(name), is_error=True)


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    stateless = False
    sessions = set()
    lock = threading.Lock()

    def log_message(self, fmt, *args):  # quiet
        pass

    def _session_ok(self):
        if self.stateless:
            return True
        sid = self.headers.get("Mcp-Session-Id")
        if sid is None:
            return False
        with self.lock:
            return sid in self.sessions

    def _send_json(self, code, payload, session=None):
        body = json.dumps(payload).encode()
        self.send_response(code)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        if session:
            self.send_header("Mcp-Session-Id", session)
        self.end_headers()
        self.wfile.write(body)

    def _send_sse(self, payload):
        data = json.dumps(payload)
        chunk = "event: message\ndata: {}\n\n".format(data).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(chunk)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        self.wfile.write(chunk)

    def _respond(self, req_id, result):
        return {"jsonrpc": "2.0", "id": req_id, "result": result}

    def _respond_error(self, req_id, code, message):
        return {
            "jsonrpc": "2.0",
            "id": req_id,
            "error": {"code": code, "message": message},
        }

    def do_DELETE(self):
        if self.stateless:
            self._send_json(200, {"jsonrpc": "2.0", "result": {}})
            return
        sid = self.headers.get("Mcp-Session-Id")
        with self.lock:
            self.sessions.discard(sid)
        self.send_response(200)
        self.send_header("Content-Length", "0")
        self.end_headers()

    def do_GET(self):
        # No server-initiated stream on this fixture.
        self._send_json(
            405, {"jsonrpc": "2.0", "error": {"code": -32000, "message": "GET not supported"}}
        )

    def do_POST(self):
        if self.path.split("?")[0] != "/mcp":
            self._send_json(404, {"error": "not found"})
            return
        length = int(self.headers.get("Content-Length", "0"))
        try:
            req = json.loads(self.rfile.read(length).decode() or "{}")
        except (ValueError, UnicodeDecodeError):
            self._send_json(
                400, {"jsonrpc": "2.0", "id": None, "error": {"code": -32700, "message": "parse error"}}
            )
            return

        method = req.get("method")
        req_id = req.get("id")

        # Notifications never carry an id: 202 Accepted, no body.
        if req_id is None:
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.end_headers()
            return

        if method == "initialize":
            if self.stateless:
                self._send_json(
                    200,
                    self._respond(
                        req_id,
                        {
                            "protocolVersion": PROTOCOL_VERSION,
                            "capabilities": {"tools": {}},
                            "serverInfo": {"name": "streamable-stateless", "version": "0.1.0"},
                        },
                    ),
                )
                return
            sid = uuid.uuid4().hex
            with self.lock:
                self.sessions.add(sid)
            self._send_json(
                200,
                self._respond(
                    req_id,
                    {
                        "protocolVersion": PROTOCOL_VERSION,
                        "capabilities": {"tools": {}},
                        "serverInfo": {"name": "streamable", "version": "0.1.0"},
                    },
                ),
                session=sid,
            )
            return

        if not self._session_ok():
            # Spec: unknown/expired session id -> 404, client must reinitialize.
            self._send_json(404, {"error": "session not found"})
            return

        if method == "tools/list":
            self._send_json(200, self._respond(req_id, {"tools": TOOLS}))
            return

        if method == "tools/call":
            params = req.get("params") or {}
            name = params.get("name")
            args = params.get("arguments") or {}
            if name == "rpc_error":
                self._send_json(
                    200, self._respond_error(req_id, -32000, "rpc_error: deliberate failure")
                )
                return
            result = dispatch_tool(name, args)
            if name == "stream_add":
                # Exercise the SSE response mode of Streamable HTTP.
                self._send_sse(self._respond(req_id, result))
                return
            self._send_json(200, self._respond(req_id, result))
            return

        self._send_json(200, self._respond(req_id, {}))


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("port", type=int)
    parser.add_argument("--stateless", action="store_true")
    args = parser.parse_args()

    handler = Handler
    handler.stateless = args.stateless
    server = ThreadingHTTPServer(("127.0.0.1", args.port), handler)
    server.daemon_threads = True
    print("ready", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
