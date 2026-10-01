#!/usr/bin/env python3
"""A stand-in for the installed runner exe, for test_clean_room.py's end-to-end
test only: serves test_clean_room.FakeRunner over real HTTP on the port named
by CLEAN_ROOM_FAKE_PORT, for the fixture named by CLEAN_ROOM_FAKE_FIXTURE, so
the CLI's launch, urllib transport, HTTP-error path and stop run for real.
The page text carries CLEAN_ROOM_FAKE_PAGE_TEXT when set."""

import os
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
os.environ.setdefault(
    "CLEAN_ROOM_VOCABULARY", os.environ.get("CLEAN_ROOM_FAKE_VOCABULARY", "")
)
from test_clean_room import FakeRunner

PORT = int(os.environ["CLEAN_ROOM_FAKE_PORT"])
fake = FakeRunner(
    Path(os.environ["CLEAN_ROOM_FAKE_FIXTURE"]),
    page_text=os.environ.get("CLEAN_ROOM_FAKE_PAGE_TEXT", "Projects"),
)


class _ThisProcess:
    """This server IS the runner process: alive until a window close."""

    _alive = True

    def alive(self):
        return self._alive


fake.process = _ThisProcess()


class Handler(BaseHTTPRequestHandler):
    def _serve(self, method):
        n = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(n) if n else None
        status, text = fake(method, f"http://127.0.0.1:9876{self.path}", body, 10)
        data = text.encode("utf-8")
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)
        self.wfile.flush()
        if not fake.process.alive():
            # The window close tears the app down: exit on our own, like the product.
            threading.Timer(0.2, lambda: os._exit(0)).start()

    def do_GET(self):
        self._serve("GET")

    def do_POST(self):
        self._serve("POST")

    def do_DELETE(self):
        self._serve("DELETE")

    def log_message(self, *a):
        pass


ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
