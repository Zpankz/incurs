"""Webhook receiver for the events-worker smoke test.

Checks every request's Standard Webhooks signature with its own HMAC-SHA256,
independent of the crate under test, and appends one JSON line per request to
the log file. Behavior by path:

  /ok        echo verification challenges; answer events 200
  /flaky     echo challenges; answer an event 500 the first time, then 200
  /gone      echo challenges; answer events 410
  /redirect  answer 302 to /ok
  /slow      echo challenges after 4 seconds
"""

import base64
import hashlib
import hmac
import json
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

PORT = int(sys.argv[1])
LOG = sys.argv[2]
SECRET = sys.argv[3]
KEY = base64.b64decode(SECRET.removeprefix("whsec_"))
LOCK = threading.Lock()
FLAKY_SEEN = set()


def signature_valid(headers, body):
    message_id = headers.get("webhook-id", "")
    timestamp = headers.get("webhook-timestamp", "")
    signed = f"{message_id}.{timestamp}.".encode() + body
    expected = base64.b64encode(hmac.new(KEY, signed, hashlib.sha256).digest()).decode()
    entries = headers.get("webhook-signature", "").split(" ")
    fresh = timestamp.isdigit() and abs(time.time() - int(timestamp)) <= 300
    return fresh and any(
        entry.startswith("v1,") and hmac.compare_digest(entry[3:], expected) for entry in entries
    )


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        body = self.rfile.read(int(self.headers.get("content-length", "0")))
        try:
            payload = json.loads(body)
        except ValueError:
            payload = None
        kind = "verification" if isinstance(payload, dict) and payload.get("type") == "verification" else "event"
        record = {
            "path": self.path,
            "kind": kind,
            "at": time.time(),
            "valid": signature_valid(self.headers, body),
            "webhook_id": self.headers.get("webhook-id"),
            "signature": self.headers.get("webhook-signature"),
            "subscription": self.headers.get("x-mcp-subscription-id"),
            "event_id": payload.get("eventId") if isinstance(payload, dict) else None,
        }
        with LOCK:
            with open(LOG, "a") as log:
                log.write(json.dumps(record) + "\n")
        if self.path == "/redirect":
            self.send_response(302)
            self.send_header("Location", "/ok")
            self.send_header("content-length", "0")
            self.end_headers()
            return
        if kind == "verification":
            if self.path == "/slow":
                time.sleep(4)
            self.reply(200, {"challenge": payload.get("challenge")})
            return
        if self.path == "/gone":
            self.reply(410, {})
            return
        if self.path == "/flaky":
            with LOCK:
                first = record["event_id"] not in FLAKY_SEEN
                FLAKY_SEEN.add(record["event_id"])
            if first:
                self.reply(500, {})
                return
        self.reply(200, {})

    def reply(self, status, value):
        data = json.dumps(value).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(data)))
        self.end_headers()
        try:
            self.wfile.write(data)
        except BrokenPipeError:
            pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
