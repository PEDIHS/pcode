"""Offline protocol integration tests; never use real accounts or paid quotas."""
import contextlib
import importlib.util
import json
import os
from pathlib import Path
import tempfile
import threading
import time
import tomllib
import unittest
from unittest.mock import patch
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

SPEC = importlib.util.spec_from_file_location("keyless", Path(__file__).with_name("server.py"))
keyless = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(keyless)


@contextlib.contextmanager
def running(state):
    server = keyless.Server(("127.0.0.1", 0), keyless.Handler)
    server.state = state
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}"
    finally:
        server.shutdown()
        server.server_close()
        thread.join(2)


class ConnectorTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.state = keyless.State(Path(self.tmp.name))

    def test_quota_is_model_specific_positive_unexpired_and_catalog_bound(self):
        catalog = [{"modelId": "GLM-5.3"}, {"modelId": "GLM-5.3-Flash"}]
        balance = {"data": {"balances": [
            {"available_units": "1", "capabilities": ["model:glm-5.3-flash", "model:UNKNOWN"]},
            {"available_units": 0, "remaining_units": 100, "capabilities": ["model:GLM-5.3"]},
            {"available_units": 99, "expires_at": time.time() - 20, "capabilities": ["model:GLM-5.3"]},
            {"available_units": "not-a-number", "capabilities": ["model:GLM-5.3"]},
        ]}}
        self.assertEqual(keyless.allowed_models(balance, catalog), ["GLM-5.3-Flash"])
        self.assertEqual(keyless.allowed_models({"data": {"plans": [{"status": "active"}]}}, catalog), [])

    def test_profile_install_preserves_existing_config_and_does_not_choose_it_as_default(self):
        original = '# user settings\n[provider]\ndefault_provider = "claude"\n[display]\ntheme = "glass"\n'
        config = self.state.home / "config.toml"
        config.write_text(original)
        with patch.object(keyless, "remote_json", return_value={"data": [{"id": 'local/model"name'}]}):
            result = self.state.register("ollama")
            first = config.read_text()
            self.state.register("ollama")
        self.assertEqual(config.read_text(), first)
        self.assertTrue(first.startswith(original.rstrip()))
        parsed = tomllib.loads(first)
        self.assertEqual(parsed["provider"]["default_provider"], "claude")
        provider = parsed["providers"][result["profile"]]
        self.assertEqual(provider["auth"], "bearer")
        self.assertTrue(provider["requires_api_key"])
        self.assertEqual(provider["api_key"], self.state.secret)
        self.assertEqual(provider["headers"]["X-Jcode-Connector"], self.state.secret)
        self.assertEqual(provider["models"][0]["id"], 'local/model"name')
        self.assertEqual(len(list(self.state.directory.glob("config-before-*"))), 1)

    def test_never_overwrites_an_unmanaged_profile(self):
        config = self.state.home / "config.toml"
        original = '[providers.jcode-local-ollama]\nbase_url = "http://existing"\n'
        config.write_text(original)
        with patch.object(keyless, "remote_json", return_value={"data": [{"id": "test"}]}):
            with self.assertRaises(keyless.ConnectorError):
                self.state.register("ollama")
        self.assertEqual(config.read_text(), original)

    def test_empty_allowance_never_enables_or_falls_back_to_paid(self):
        with patch.object(self.state, "check_zai", return_value={"models": []}):
            with self.assertRaises(keyless.ConnectorError):
                self.state.register("zai")
        self.assertFalse((self.state.home / "config.toml").exists())

    @unittest.skipUnless(os.name == "nt", "Windows credential protection")
    def test_dpapi_credentials_do_not_appear_in_storage_or_status(self):
        token = "test-only-account-token-do-not-use"
        encrypted = keyless.protect(json.dumps({"token": token, "user": {"name": "Tester"}}).encode())
        (self.state.directory / "zai-account.dpapi").write_bytes(encrypted)
        self.assertNotIn(token.encode(), encrypted)
        self.assertEqual(self.state.credentials()["token"], token)
        self.assertNotIn(token, json.dumps(self.state.public_status()))

    def test_untrusted_auth_url_and_expired_flow_are_rejected(self):
        data = {"code": 0, "data": {"authorize_url": "https://evil.example/", "flow_id": "x",
                                   "expires_at": time.time() + 300, "poll_interval_sec": 2}}
        with patch.object(keyless, "remote_json", return_value=data):
            with self.assertRaises(keyless.ConnectorError):
                self.state.zai_start()
        self.state.flows["expired"] = {"expires": time.time() - 1}
        with self.assertRaises(keyless.ConnectorError):
            self.state.zai_poll("expired")

    def test_http_rejects_foreign_origins_invalid_hosts_and_missing_capabilities(self):
        with running(self.state) as base:
            def code(path, headers):
                try:
                    urllib.request.urlopen(urllib.request.Request(base + path, headers=headers))
                except urllib.error.HTTPError as error:
                    result = error.code
                    error.close()
                    return result
                return 200
            self.assertEqual(code("/api/status", {}), 401)
            self.assertEqual(code("/api/status", {"X-Jcode-Connector": self.state.secret, "Origin": "https://evil.example"}), 401)
            self.assertEqual(code("/api/status", {"X-Jcode-Connector": self.state.secret, "Host": "evil.example"}), 403)
            self.assertEqual(code("/api/status", {"X-Jcode-Connector": self.state.secret}), 200)
            self.assertEqual(code("/api/status", {"Authorization": "Bearer " + self.state.secret}), 200)
            self.assertEqual(code("/health", {}), 200)

    def test_native_streaming_tool_calls_and_request_tools_pass_through_unchanged(self):
        expected = b'data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_test","type":"function","function":{"name":"read","arguments":"{\\\"path\\\":\\\"README.md\\\"}"}}]}}]}\n\ndata: [DONE]\n\n'
        captured = {}
        class Upstream(BaseHTTPRequestHandler):
            def log_message(self, *_): pass
            def do_POST(self):
                captured["body"] = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                captured["private_header"] = self.headers.get("X-Jcode-Connector")
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(expected)))
                self.end_headers()
                self.wfile.write(expected)
        upstream = ThreadingHTTPServer(("127.0.0.1", 0), Upstream)
        thread = threading.Thread(target=upstream.serve_forever, daemon=True)
        thread.start()
        self.addCleanup(upstream.server_close)
        self.addCleanup(upstream.shutdown)
        request = {"model": "fixture", "stream": True, "messages": [{"role": "user", "content": "Read the file"}],
                   "tools": [{"type": "function", "function": {"name": "read", "parameters": {"type": "object"}}}]}
        with patch.dict(keyless.LOCAL_SERVICES, {"ollama": ("Fixture", f"http://127.0.0.1:{upstream.server_port}/v1")}):
            with running(self.state) as base:
                req = urllib.request.Request(base + "/ollama/v1/chat/completions", json.dumps(request).encode(),
                    headers={"Content-Type": "application/json", "X-Jcode-Connector": self.state.secret})
                with urllib.request.urlopen(req) as response:
                    self.assertEqual(response.read(), expected)
        self.assertEqual(captured["body"], request)
        self.assertIsNone(captured["private_header"])

    def test_cancelled_login_cannot_replace_account_with_a_late_poll(self):
        flow = {"id": "test", "poll_token": "test", "expires": time.time() + 60, "interval": 2, "next": 0}
        self.state.flows["local"] = flow
        def delayed(*args, **kwargs):
            self.state.flows.clear()
            return {"code": 0, "data": {"status": "ready", "token": "test", "user": {}}}
        with patch.object(keyless, "remote_json", side_effect=delayed):
            with self.assertRaises(keyless.ConnectorError):
                self.state.zai_poll("local")
        self.assertFalse((self.state.directory / "zai-account.dpapi").exists())

    def test_native_login_uses_scriptable_json_and_sends_authorization_codes_on_stdin(self):
        from types import SimpleNamespace
        observed = []
        def run(command, **options):
            observed.append((command, options))
            return SimpleNamespace(stdout=b'{"status":"authenticated"}\n=== auth-test: saved\n', returncode=0)
        with patch.object(keyless.subprocess, "run", side_effect=run):
            result = self.state.native_operation("gemini", "fixture-flow", "--auth-code", "fixture-code")
        self.assertEqual(result["status"], "authenticated")
        self.assertNotIn("fixture-code", observed[0][0])
        self.assertEqual(observed[0][1]["input"], b"fixture-code")
        self.assertIn("--json", observed[0][0])
        self.assertIn("--no-browser", observed[0][0])


if __name__ == "__main__":
    unittest.main(verbosity=2)
