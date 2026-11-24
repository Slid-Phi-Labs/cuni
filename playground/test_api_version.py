"""GET /api/version: CLI version, target count, and build commit.

Starts the real Studio handler on a free port with a stand-in `cuni` binary,
so it needs no Rust build. When a real build is present it also checks that
the reported version matches Cargo.toml.

Run: python3 -m unittest playground.test_api_version -v
"""

from __future__ import annotations

import json
import os
import re
import sys
import tempfile
import threading
import unittest
import urllib.request
from http.server import ThreadingHTTPServer
from pathlib import Path

PLAY = Path(__file__).resolve().parent
ROOT = PLAY.parent
if str(PLAY) not in sys.path:
    sys.path.insert(0, str(PLAY))

import server  # noqa: E402

FAKE_CUNI = """#!/bin/sh
if [ "$1" = "version" ]; then echo "cuni 9.9.9"; exit 0; fi
if [ "$1" = "--list-langs" ]; then printf 'py\\tPython\\t.py\\ngo\\tGo\\t.go\\njs\\tJavaScript\\t.js\\n'; exit 0; fi
exit 2
"""


def _cargo_version() -> str:
    m = re.search(r'^version\s*=\s*"([^"]+)"', (ROOT / "Cargo.toml").read_text(), re.M)
    assert m, "no version in Cargo.toml"
    return m.group(1)


class ApiVersionTests(unittest.TestCase):
    def setUp(self):
        self._env = dict(os.environ)
        self.tmp = tempfile.TemporaryDirectory()
        # Studio children run as the sandbox uid (sandbox.py), so the fake
        # binary's directory must be traversable by it, like /usr/local/bin.
        os.chmod(self.tmp.name, 0o755)
        fake = Path(self.tmp.name) / "cuni"
        fake.write_text(FAKE_CUNI)
        fake.chmod(0o755)  # executable by the sandbox uid too
        os.environ["CUNI_BIN"] = str(fake)
        os.environ["CUNI_BUILD_SHA"] = "0123456789abcdef0123456789abcdef01234567"
        self.httpd = ThreadingHTTPServer(("127.0.0.1", 0), server.Handler)
        threading.Thread(target=self.httpd.serve_forever, daemon=True).start()

    def tearDown(self):
        self.httpd.shutdown()
        self.httpd.server_close()
        os.environ.clear()
        os.environ.update(self._env)
        self.tmp.cleanup()

    def _get(self, path: str) -> tuple[int, dict]:
        url = f"http://127.0.0.1:{self.httpd.server_address[1]}{path}"
        with urllib.request.urlopen(url, timeout=10) as r:
            return r.status, json.loads(r.read().decode())

    def test_reports_version_targets_and_commit(self):
        code, body = self._get("/api/version")
        self.assertEqual(code, 200)
        self.assertTrue(body["ok"])
        self.assertEqual(body["version"], "9.9.9")
        self.assertEqual(body["targets"], 3)
        self.assertEqual(body["commit"], "0123456789abcdef0123456789abcdef01234567")

    def test_unknown_build_arg_is_not_reported_as_a_commit(self):
        os.environ["CUNI_BUILD_SHA"] = "unknown"
        os.environ.pop("GIT_SHA", None)
        sha = server.build_sha()
        self.assertNotEqual(sha, "unknown")

    def test_missing_binary_says_so(self):
        os.environ["CUNI_BIN"] = "/nonexistent/cuni"
        info = server.version_info()
        if info["ok"]:
            self.skipTest("a real cuni binary is on this machine")
        self.assertIsNone(info["version"])
        self.assertTrue(info["error"])


class RealBinaryTests(unittest.TestCase):
    def test_real_build_matches_cargo_toml(self):
        for rel in ("target/release/cuni", "target/debug/cuni"):
            if (ROOT / rel).is_file():
                break
        else:
            self.skipTest("no cuni build in target/")
        old = os.environ.pop("CUNI_BIN", None)
        try:
            info = server.version_info()
        finally:
            if old is not None:
                os.environ["CUNI_BIN"] = old
        self.assertEqual(info["version"], _cargo_version())
        self.assertEqual(info["targets"], 53)


if __name__ == "__main__":
    unittest.main()
