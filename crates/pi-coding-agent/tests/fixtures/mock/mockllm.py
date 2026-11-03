#!/usr/bin/env python3
"""Scripted OpenAI chat-completions server (SSE) for deterministic harness runs.

usage: mockllm.py <port> <script.json> <log.jsonl>

Port 0 picks a free port. Once listening, the server prints "listening <port>" on stdout.

script.json: list of turns. Each turn is {"text": "..."} or
{"tool_calls": [{"name": "...", "arguments": {...}}], "text": optional}.
The n-th chat request gets the n-th turn; later requests get {"text": "done"}.
A turn may set "match": "<substring>" to require the last message to contain it
(otherwise the server answers "MOCK-MISMATCH" so a broken flow is visible).
"""
import http.server, json, sys, threading, time

PORT = int(sys.argv[1])
SCRIPT = json.load(open(sys.argv[2]))
LOG = open(sys.argv[3], "a", buffering=1)
lock = threading.Lock()
state = {"n": 0}

def sse(handler, obj):
    data = ("data: " + json.dumps(obj) + "\n\n").encode()
    handler.wfile.write(b"%x\r\n%s\r\n" % (len(data), data))
    handler.wfile.flush()

class H(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    def log_message(self, *a):
        pass
    def do_GET(self):
        body = json.dumps({"object": "list", "data": [{"id": "mock-model", "object": "model"}]}).encode()
        self.send_response(200)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        req = json.loads(self.rfile.read(length) or b"{}")
        with lock:
            i = state["n"]
            state["n"] += 1
        LOG.write(json.dumps({"i": i, "t": time.time(), "req": req}) + "\n")
        turn = SCRIPT[i] if i < len(SCRIPT) else {"text": "done"}
        last = json.dumps(req.get("messages", [])[-1:])
        if turn.get("match") and turn["match"] not in last:
            turn = {"text": "MOCK-MISMATCH expected " + turn["match"]}
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Transfer-Encoding", "chunked")
        self.send_header("Connection", "close")
        self.end_headers()
        base = {"id": f"chatcmpl-{i}", "object": "chat.completion.chunk", "created": int(time.time()), "model": req.get("model", "mock-model")}
        sse(self, {**base, "choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}, "finish_reason": None}]})
        if turn.get("text"):
            sse(self, {**base, "choices": [{"index": 0, "delta": {"content": turn["text"]}, "finish_reason": None}]})
        calls = turn.get("tool_calls") or []
        for k, c in enumerate(calls):
            sse(self, {**base, "choices": [{"index": 0, "delta": {"tool_calls": [{"index": k, "id": f"call_{i}_{k}", "type": "function", "function": {"name": c["name"], "arguments": json.dumps(c.get("arguments", {}))}}]}, "finish_reason": None}]})
        sse(self, {**base, "choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls" if calls else "stop"}]})
        sse(self, {**base, "choices": [], "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})
        done = b"data: [DONE]\n\n"
        self.wfile.write(b"%x\r\n%s\r\n" % (len(done), done))
        self.wfile.write(b"0\r\n\r\n")
        self.wfile.flush()
        self.close_connection = True

server = http.server.ThreadingHTTPServer(("127.0.0.1", PORT), H)
print("listening", server.server_address[1], flush=True)
server.serve_forever()
