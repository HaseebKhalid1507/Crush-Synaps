#!/usr/bin/env bash
# Pipe real Content-Length JSON-RPC frames through the crush binary.
set -euo pipefail
BIN="${1:-./target/debug/crush}"
frame() { local b="$1"; printf 'Content-Length: %d\r\n\r\n%s' "${#b}" "$b"; }
big=$(printf 'x%.0s' {1..50000})
{
  frame '{"jsonrpc":"2.0","id":1,"method":"initialize"}'
  frame "{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"hook.handle\",\"params\":{\"kind\":\"after_tool_call\",\"tool_input\":{\"command\":\"echo\"},\"tool_output\":\"$big\"}}"
  frame '{"jsonrpc":"2.0","id":3,"method":"shutdown"}'
} | "$BIN" 2>/dev/null | tr -d '\r' | grep -o '"protocol_version":[0-9]*\|"action":"[a-z]*"'
