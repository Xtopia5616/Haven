#!/usr/bin/env python3
"""
Minimal MCP echo server for integration testing.
Implements the MCP stdio protocol (JSON-RPC 2.0, NDJSON).
Supports: initialize, tools/list, tools/call (echo).
"""

import json
import sys


def send(msg):
    sys.stdout.write(json.dumps(msg) + "\n")
    sys.stdout.flush()


def recv():
    line = sys.stdin.readline()
    if not line:
        return None
    return json.loads(line)


def main():
    while True:
        msg = recv()
        if msg is None:
            break

        method = msg.get("method")
        msg_id = msg.get("id")
        params = msg.get("params", {})

        if method == "initialize":
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "protocolVersion": "2024-11-05",
                    "serverInfo": {
                        "name": "echo-server",
                        "version": "1.0.0",
                    },
                },
            })
        elif method == "notifications/initialized":
            pass
        elif method == "tools/list":
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": {
                    "tools": [
                        {
                            "name": "echo",
                            "description": "Echo the input text back",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "text": {"type": "string"},
                                },
                                "required": ["text"],
                            },
                        },
                        {
                            "name": "reverse",
                            "description": "Reverse the input text",
                            "inputSchema": {
                                "type": "object",
                                "properties": {
                                    "text": {"type": "string"},
                                },
                                "required": ["text"],
                            },
                        },
                    ],
                },
            })
        elif method == "tools/call":
            tool_name = params.get("name", "")
            arguments = params.get("arguments", {})
            text = arguments.get("text", "")

            if tool_name == "echo":
                send({
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "content": [{"type": "text", "text": text}],
                    },
                })
            elif tool_name == "reverse":
                send({
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "result": {
                        "content": [{"type": "text", "text": text[::-1]}],
                    },
                })
            else:
                send({
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {
                        "code": -32601,
                        "message": f"Tool not found: {tool_name}",
                    },
                })
        elif method == "shutdown":
            send({
                "jsonrpc": "2.0",
                "id": msg_id,
                "result": None,
            })
        elif method == "exit":
            break
        else:
            if msg_id is not None:
                send({
                    "jsonrpc": "2.0",
                    "id": msg_id,
                    "error": {
                        "code": -32601,
                        "message": f"Method not found: {method}",
                    },
                })


if __name__ == "__main__":
    main()