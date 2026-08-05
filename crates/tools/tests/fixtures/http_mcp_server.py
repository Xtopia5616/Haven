#!/usr/bin/env python3
"""
Minimal MCP Streamable HTTP echo server for integration testing.

Implements the MCP 2025-03-26 Streamable HTTP transport with a single
endpoint (`/mcp`):

* POST  -> JSON-RPC messages; responses are delivered as SSE streams
          (Content-Type: text/event-stream) so the client's SSE parsing path
          is exercised. Notifications get an empty 202.
* GET   -> a long-lived SSE stream that pushes a tools/list_changed
          notification, exercising the client's notification listener.

Usage: python http_mcp_server.py <port>
"""

import base64
import json
import sys
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = 0
SESSION_ID = "echo-http-session-1"

TOOLS = [
    {
        "name": "echo",
        "description": "Echo the input text back",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "reverse",
        "description": "Reverse the input text",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
    {
        "name": "image",
        "description": "Return a tiny PNG image block",
        "inputSchema": {"type": "object", "properties": {}},
    },
    {
        "name": "resource",
        "description": "Return an embedded text resource",
        "inputSchema": {
            "type": "object",
            "properties": {"text": {"type": "string"}},
            "required": ["text"],
        },
    },
]


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.0"

    def log_message(self, *args):
        pass

    def _read_body(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        return self.rfile.read(length) if length else b""

    def _write_json(self, payload, status=200, extra_headers=None):
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        # Close after every response: with HTTP/1.0 and no keep-alive the
        # client (reqwest) would otherwise pool the connection and reuse a
        # socket the server already closed, producing flaky
        # "error reading a body from connection" failures.
        self.send_header("Connection", "close")
        if extra_headers:
            for k, v in extra_headers:
                self.send_header(k, v)
        self.end_headers()
        self.wfile.write(body)

    def _write_sse(self, payload, status=200):
        data = json.dumps(payload)
        body = ("event: message\ndata: " + data + "\n\n").encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        # A POST response carries exactly one event, so a Content-Length makes
        # the body deterministic — the client reads exactly these bytes instead
        # of depending on connection-close timing (which intermittently
        # produced WSAECONNRESET on Windows).
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.wfile.flush()

    def do_POST(self):
        if self.path != "/mcp":
            self.send_error(404)
            return
        body = self._read_body()
        try:
            msg = json.loads(body)
        except Exception:
            self._write_json(
                {
                    "jsonrpc": "2.0",
                    "id": None,
                    "error": {"code": -32700, "message": "Parse error"},
                },
                400,
            )
            return

        method = msg.get("method")
        msg_id = msg.get("id")

        if method == "initialize":
            self._write_json(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "protocolVersion": "2024-11-05",
                        "serverInfo": {"name": "echo-http", "version": "1.0.0"},
                    },
                },
                extra_headers=[("Mcp-Session-Id", SESSION_ID)],
            )
            return

        if method.startswith("notifications/"):
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            return

        if msg_id is None:
            self.send_response(202)
            self.send_header("Content-Length", "0")
            self.send_header("Connection", "close")
            self.end_headers()
            return

        if method == "ping":
            self._write_sse({"jsonrpc": "2.0", "id": msg_id, "result": {}})
        elif method == "tools/list":
            self._write_sse(
                {"jsonrpc": "2.0", "id": msg_id, "result": {"tools": TOOLS}}
            )
        elif method == "tools/call":
            params = msg.get("params", {})
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})
            text = arguments.get("text", "")
            if tool_name == "echo":
                content = [{"type": "text", "text": text}]
            elif tool_name == "reverse":
                content = [{"type": "text", "text": text[::-1]}]
            elif tool_name == "image":
                png = base64.b64decode(
                    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg=="
                )
                content = [
                    {
                        "type": "image",
                        "mimeType": "image/png",
                        "data": base64.b64encode(png).decode(),
                    }
                ]
            elif tool_name == "resource":
                content = [
                    {
                        "type": "resource",
                        "resource": {
                            "uri": "memory://note",
                            "mimeType": "text/plain",
                            "text": text,
                        },
                    }
                ]
            else:
                self._write_sse(
                    {
                        "jsonrpc": "2.0",
                        "id": msg_id,
                        "error": {"code": -32601, "message": f"Tool not found: {tool_name}"},
                    }
                )
                return
            self._write_sse(
                {"jsonrpc": "2.0", "id": msg_id, "result": {"content": content}}
            )
        elif method == "shutdown":
            self._write_sse({"jsonrpc": "2.0", "id": msg_id, "result": None})
        else:
            self._write_sse(
                {
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {"code": -32601, "message": f"Method not found: {method}"},
                }
            )

    def do_GET(self):
        if self.path != "/mcp":
            self.send_error(404)
            return
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        try:
            self.wfile.write(
                b'event: message\ndata: {"jsonrpc":"2.0","method":"notifications/tools/list_changed"}\n\n'
            )
            self.wfile.flush()
            while True:
                time.sleep(1)
                self.wfile.write(b": keepalive\n\n")
                self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, OSError):
            pass


if __name__ == "__main__":
    # Bind to port 0 so the OS assigns a unique free port atomically — this
    # eliminates the bind-then-release race of picking a port from the test
    # harness. The actual port is reported in the READY line; the test parses
    # it and connects to this process and this process alone.
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    print(f"READY {server.server_address[1]}", flush=True)
    server.serve_forever()
