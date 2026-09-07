#!/usr/bin/env python3
"""Credential-blind counting relay for approved Hya live validation."""

from __future__ import annotations

import argparse
import datetime as dt
import json
import os
import shutil
import threading
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

UPSTREAM_ORIGIN = ""
METADATA_PATH = Path()
REQUEST_CAP = 0
STATE_LOCK = threading.Lock()
NEXT_ORDINAL = 0
FORWARDED_COUNT = 0

HOP_BY_HOP_HEADERS = {
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
}


def utc_timestamp() -> str:
    """Return one UTC timestamp for relay metadata."""
    return dt.datetime.now(dt.timezone.utc).isoformat()


def bounded_name(value: Any) -> str | None:
    """Return a bounded Tool schema name or None for an invalid value."""
    if not isinstance(value, str):
        return None
    return value[:128]


def summarize_request(body: bytes) -> tuple[list[str], str | None]:
    """Extract only Tool schema names and reasoning effort from request JSON."""
    try:
        payload = json.loads(body)
    except (UnicodeDecodeError, json.JSONDecodeError):
        return [], None
    if not isinstance(payload, dict):
        return [], None

    names: list[str] = []
    tools = payload.get("tools")
    if isinstance(tools, list):
        for tool in tools:
            if not isinstance(tool, dict):
                continue
            name = bounded_name(tool.get("name"))
            if name is None:
                function = tool.get("function")
                if isinstance(function, dict):
                    name = bounded_name(function.get("name"))
            if name is not None and name not in names:
                names.append(name)

    effort: str | None = None
    reasoning = payload.get("reasoning")
    if isinstance(reasoning, dict):
        effort = bounded_name(reasoning.get("effort"))
    if effort is None:
        effort = bounded_name(payload.get("reasoning_effort"))
    return names, effort


def load_existing_state() -> tuple[int, int]:
    """Recover the highest ordinal and durable forwarded reservations."""
    if not METADATA_PATH.exists():
        return 0, 0
    highest = 0
    forwarded: set[int] = set()
    for line in METADATA_PATH.read_text(encoding="utf-8").splitlines():
        if not line:
            continue
        try:
            record = json.loads(line)
        except json.JSONDecodeError:
            continue
        ordinal = record.get("ordinal")
        if not isinstance(ordinal, int) or ordinal < 1:
            continue
        highest = max(highest, ordinal)
        if record.get("status") == "forwarding":
            forwarded.add(ordinal)
    return highest, len(forwarded)


def append_record_locked(record: dict[str, Any]) -> None:
    """Durably append one metadata-only record while STATE_LOCK is held."""
    with METADATA_PATH.open("a", encoding="utf-8") as stream:
        stream.write(json.dumps(record, separators=(",", ":"), sort_keys=True))
        stream.write("\n")
        stream.flush()
        os.fsync(stream.fileno())


def reserve_request(path: str, names: list[str], effort: str | None) -> tuple[int, bool]:
    """Persist an ordinal before forwarding and enforce the global request cap."""
    global FORWARDED_COUNT, NEXT_ORDINAL
    with STATE_LOCK:
        NEXT_ORDINAL += 1
        ordinal = NEXT_ORDINAL
        record = {
            "ordinal": ordinal,
            "time": utc_timestamp(),
            "path": path,
            "status": "forwarding",
            "tool_schema_names": names,
            "reasoning_effort": effort,
        }
        if FORWARDED_COUNT >= REQUEST_CAP:
            record["status"] = 429
            append_record_locked(record)
            return ordinal, False
        FORWARDED_COUNT += 1
        append_record_locked(record)
        return ordinal, True


def finish_request(
    ordinal: int,
    path: str,
    names: list[str],
    effort: str | None,
    status: int,
) -> None:
    """Append the terminal HTTP status for one reserved request."""
    record = {
        "ordinal": ordinal,
        "time": utc_timestamp(),
        "path": path,
        "status": status,
        "tool_schema_names": names,
        "reasoning_effort": effort,
    }
    with STATE_LOCK:
        append_record_locked(record)


def safe_path(raw_path: str) -> str:
    """Return only the URL path, excluding query and fragment data."""
    return urllib.parse.urlsplit(raw_path).path


class RelayHandler(BaseHTTPRequestHandler):
    """Proxy provider requests without retaining headers or bodies."""

    protocol_version = "HTTP/1.1"

    def log_message(self, _format: str, *_args: Any) -> None:
        """Disable the default request logger."""

    def do_GET(self) -> None:  # noqa: N802
        """Serve relay status or proxy one GET request."""
        if safe_path(self.path) == "/__hya_relay/status":
            self._serve_status()
            return
        self._proxy_request(b"")

    def do_POST(self) -> None:  # noqa: N802
        """Proxy one POST request after reading its in-memory body."""
        length_header = self.headers.get("Content-Length", "0")
        try:
            length = int(length_header)
        except ValueError:
            self._send_json(400, {"error": "invalid content length"})
            return
        if length < 0:
            self._send_json(400, {"error": "invalid content length"})
            return
        body = self.rfile.read(length)
        self._proxy_request(body)

    def do_HEAD(self) -> None:  # noqa: N802
        """Proxy one HEAD request without a body."""
        self._proxy_request(b"")

    def _serve_status(self) -> None:
        """Return safe in-memory cap counters for readiness checks."""
        with STATE_LOCK:
            payload = {
                "ready": True,
                "attempts": NEXT_ORDINAL,
                "forwarded": FORWARDED_COUNT,
                "cap": REQUEST_CAP,
            }
        self._send_json(200, payload)

    def _send_json(self, status: int, payload: dict[str, Any]) -> None:
        """Send one small local JSON response and close the connection."""
        data = json.dumps(payload, separators=(",", ":")).encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Connection", "close")
        self.end_headers()
        if self.command != "HEAD":
            self.wfile.write(data)
        self.close_connection = True

    def _forward_headers(self) -> dict[str, str]:
        """Copy end-to-end request headers in memory without logging them."""
        headers: dict[str, str] = {}
        for name, value in self.headers.items():
            lowered = name.lower()
            if lowered in HOP_BY_HOP_HEADERS or lowered in {"host", "content-length"}:
                continue
            headers[name] = value
        return headers

    def _proxy_request(self, body: bytes) -> None:
        """Reserve, forward, and stream one provider request."""
        path = safe_path(self.path)
        names, effort = summarize_request(body)
        ordinal, permitted = reserve_request(path, names, effort)
        if not permitted:
            self._send_json(429, {"error": "relay request cap reached"})
            return

        target = f"{UPSTREAM_ORIGIN}{self.path}"
        request_body = body if self.command in {"POST", "PUT", "PATCH"} else None
        request = urllib.request.Request(
            target,
            data=request_body,
            headers=self._forward_headers(),
            method=self.command,
        )
        try:
            response = urllib.request.urlopen(request, timeout=300)
        except urllib.error.HTTPError as error:
            response = error
        except Exception:
            finish_request(ordinal, path, names, effort, 502)
            self._send_json(502, {"error": "upstream transport failure"})
            return

        status = int(response.status)
        finish_request(ordinal, path, names, effort, status)
        try:
            self.send_response(status)
            for name, value in response.headers.items():
                lowered = name.lower()
                if lowered in HOP_BY_HOP_HEADERS or lowered == "content-length":
                    continue
                self.send_header(name, value)
            self.send_header("Connection", "close")
            self.end_headers()
            if self.command != "HEAD":
                shutil.copyfileobj(response, self.wfile, length=64 * 1024)
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            pass
        finally:
            response.close()
            self.close_connection = True


def parse_args() -> argparse.Namespace:
    """Parse relay startup arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument("--metadata", required=True)
    parser.add_argument("--upstream-origin", default="https://api.12th.day")
    parser.add_argument("--cap", type=int, default=2000)
    parser.add_argument("--port", type=int, default=0)
    return parser.parse_args()


def main() -> None:
    """Start the local counting relay and serve until terminated."""
    global FORWARDED_COUNT, METADATA_PATH, NEXT_ORDINAL, REQUEST_CAP, UPSTREAM_ORIGIN
    args = parse_args()
    if args.cap < 1:
        raise SystemExit("cap must be positive")
    parsed_origin = urllib.parse.urlsplit(args.upstream_origin)
    if parsed_origin.scheme != "https" or not parsed_origin.netloc:
        raise SystemExit("upstream origin must be https")

    UPSTREAM_ORIGIN = args.upstream_origin.rstrip("/")
    METADATA_PATH = Path(args.metadata)
    REQUEST_CAP = args.cap
    METADATA_PATH.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
    if not METADATA_PATH.exists():
        file_descriptor = os.open(METADATA_PATH, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
        os.close(file_descriptor)
    os.chmod(METADATA_PATH, 0o600)
    NEXT_ORDINAL, FORWARDED_COUNT = load_existing_state()

    server = ThreadingHTTPServer(("127.0.0.1", args.port), RelayHandler)
    server.daemon_threads = True
    port = int(server.server_address[1])
    print(f"READY port={port} cap={REQUEST_CAP}", flush=True)
    try:
        server.serve_forever(poll_interval=0.25)
    finally:
        server.server_close()


if __name__ == "__main__":
    main()
