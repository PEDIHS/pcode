"""Jcode account connectors. No user-supplied API keys, no browser cookie access.

Z.ai uses the public CLI OAuth protocol implemented by zai-org/ZCode. Only its
Start Plan route is exposed; an empty allowance never falls back to paid APIs.
The local HTTP interface requires a random per-install capability, including
inference. Windows account tokens are protected with current-user DPAPI.
"""
from __future__ import annotations

import argparse
import ctypes
import hmac
import json
import math
import os
from pathlib import Path
import secrets
import socket
import subprocess
import threading
import time
import tomllib
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

VERSION = "1.0.0"
PORT = 42187
ORIGIN = f"http://127.0.0.1:{PORT}"
ZCODE = "https://zcode.z.ai/api/v1"
MAX_BODY = 24 * 1024 * 1024
LOCAL_SERVICES = {
    "ollama": ("Ollama", "http://127.0.0.1:11434/v1"),
    "lmstudio": ("LM Studio", "http://127.0.0.1:1234/v1"),
}


class ConnectorError(Exception):
    def __init__(self, message, status=400):
        super().__init__(message)
        self.status = status


def atomic_write(path: Path, data: bytes):
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.with_name(path.name + "." + secrets.token_hex(6) + ".tmp")
    try:
        with open(tmp, "xb") as handle:
            if os.name != "nt":
                os.chmod(tmp, 0o600)
            handle.write(data)
            handle.flush()
            os.fsync(handle.fileno())
        os.replace(tmp, path)
    finally:
        tmp.unlink(missing_ok=True)


def protect(data: bytes, decrypt=False) -> bytes:
    """Never fall back to plaintext when OS credential protection fails."""
    if os.name != "nt":
        raise ConnectorError("Secure account storage requires Windows DPAPI on this build.")
    from ctypes import wintypes

    class Blob(ctypes.Structure):
        _fields_ = [("size", wintypes.DWORD), ("data", ctypes.POINTER(ctypes.c_ubyte))]

    buffer = ctypes.create_string_buffer(data)
    source = Blob(len(data), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_ubyte)))
    target = Blob()
    crypt = ctypes.WinDLL("crypt32", use_last_error=True)
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.LocalFree.argtypes = [ctypes.c_void_p]
    kernel.LocalFree.restype = ctypes.c_void_p
    if decrypt:
        fn = crypt.CryptUnprotectData
        args = (ctypes.byref(source), None, None, None, None, 1, ctypes.byref(target))
    else:
        fn = crypt.CryptProtectData
        args = (ctypes.byref(source), "Jcode account connectors", None, None, None, 1, ctypes.byref(target))
    fn.restype = wintypes.BOOL
    if not fn(*args):
        raise ConnectorError("Windows could not unlock this account. Sign in again.", 401)
    try:
        return ctypes.string_at(target.data, target.size)
    finally:
        kernel.LocalFree(ctypes.cast(target.data, ctypes.c_void_p))


def remote_json(url, *, method="GET", body=None, headers=None, timeout=20):
    raw = None if body is None else json.dumps(body).encode()
    request = urllib.request.Request(url, raw, method=method, headers={
        "Accept": "application/json", "User-Agent": "Jcode-Account-Connectors/" + VERSION,
        **({"Content-Type": "application/json"} if raw else {}), **(headers or {}),
    })
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            data = response.read(2 * 1024 * 1024 + 1)
        if len(data) > 2 * 1024 * 1024:
            raise ConnectorError("The provider returned an oversized response.", 502)
        return json.loads(data)
    except urllib.error.HTTPError as error:
        # Provider bodies may contain sensitive tokens. Return a fixed diagnosis.
        status = error.code
        error.close()
        text = {
            401: "This sign-in expired or was rejected. Sign in again.",
            403: "The provider has not granted access to this account or client.",
            429: "The provider's limit has been reached. Wait before trying again.",
        }.get(status, f"The provider returned HTTP {status}.")
        raise ConnectorError(text, status if status in (401, 403, 429) else 502) from None
    except (OSError, ValueError):
        raise ConnectorError("Could not reach the provider. Check the connection and try again.", 502) from None


def envelope(value):
    if not isinstance(value, dict) or value.get("code") != 0:
        raise ConnectorError("The provider did not accept this operation. No connection was enabled.", 502)
    return value.get("data") or {}


def allowed_models(balance, catalog):
    """Require positive, unexpired model-specific allowance, never a paid fallback."""
    advertised = {m["modelId"] for m in catalog if isinstance(m, dict) and isinstance(m.get("modelId"), str)}
    available = set()
    now = time.time()
    data = balance.get("data") or {}
    for bucket in data.get("balances", []):
        if not isinstance(bucket, dict):
            continue
        try:
            units = float(bucket.get("available_units", bucket.get("remaining_units", 0)) or 0)
            if not math.isfinite(units) or units <= 0:
                continue
            owners = [p for p in data.get("plans", []) if isinstance(p, dict) and (
                (bucket.get("user_plan_id") and p.get("user_plan_id") == bucket["user_plan_id"]) or
                (not bucket.get("user_plan_id") and bucket.get("plan_id") and p.get("plan_id") == bucket["plan_id"]))]
            if owners and not any(p.get("status", "").lower() == "active" and
                                  (not p.get("ends_at") or float(p["ends_at"]) > now) for p in owners):
                continue
            expiry = bucket.get("expires_at")
            if expiry:
                if isinstance(expiry, str) and not expiry.replace(".", "", 1).isdigit():
                    from datetime import datetime
                    expires = datetime.fromisoformat(expiry.replace("Z", "+00:00")).timestamp()
                else:
                    expires = float(expiry)
                    if expires > 100_000_000_000:
                        expires /= 1000
                if expires <= now:
                    continue
        except (ValueError, TypeError):
            continue
        candidates = []
        for capability in bucket.get("capabilities") or []:
            if isinstance(capability, str) and capability.lower().startswith("model:"):
                candidates.append(capability[6:].strip())
        if not candidates and isinstance(bucket.get("show_name"), str):
            candidates.append(bucket["show_name"].strip())
        for model in candidates:
            match = next((m for m in advertised if m.casefold() == model.casefold()), None)
            if match:
                available.add(match)
    return sorted(available, key=lambda m: ("flash" not in m.lower(), m))


def profile_text(name, base_url, models, secret):
    if not models or any(not isinstance(m, str) or not m.strip() for m in models):
        raise ConnectorError("No usable models were reported. Start the service or check your allowance.")
    quote = lambda s: json.dumps(s, ensure_ascii=False)
    entries = ", ".join("{ id = " + quote(m) + ", context_window = 131072 }" for m in models)
    return (f"[providers.{name}]\n"
            'type = "open-ai-compatible"\n'
            f"base_url = {quote(base_url)}\n"
            # The key is an automatically generated loopback capability, not
            # an API key obtained from the cloud provider or entered by a user.
            f'auth = "bearer"\napi_key = {quote(secret)}\nrequires_api_key = true\nmodel_catalog = true\n'
            f"default_model = {quote(models[0])}\nmodels = [{entries}]\n"
            f"headers = {{ X-Jcode-Connector = {quote(secret)} }}\n")


class State:
    def __init__(self, home: Path):
        self.home = home
        self.directory = home / "connectors"
        self.directory.mkdir(parents=True, exist_ok=True)
        self.lock = threading.RLock()
        self.flows = {}
        self.jobs = {}
        self.native_flows = {}
        self.models = []
        self.balance = None
        self.device = self._persistent("device-id", lambda: secrets.token_hex(16))
        self.secret = self._persistent("local-capability", lambda: secrets.token_urlsafe(48))

    def _persistent(self, name, create):
        path = self.directory / name
        if path.exists():
            return path.read_text().strip()
        value = create()
        atomic_write(path, value.encode())
        return value

    def credentials(self):
        path = self.directory / "zai-account.dpapi"
        if not path.exists():
            raise ConnectorError("Sign in to Z.ai first.", 401)
        try:
            return json.loads(protect(path.read_bytes(), decrypt=True))
        except (ValueError, OSError):
            raise ConnectorError("The saved account could not be opened. Sign in again.", 401) from None

    def zai_headers(self):
        return {"Authorization": "Bearer " + self.credentials()["token"],
                "X-Device-Mid": self.device, "X-Title": "Jcode Desktop"}

    def zai_start(self):
        poll_token = secrets.token_hex(32)
        data = envelope(remote_json(ZCODE + "/oauth/cli/init", method="POST", body={"provider": "zai"},
                                    headers={"Authorization": "Bearer " + poll_token}))
        parsed = urllib.parse.urlparse(data.get("authorize_url", ""))
        if parsed.scheme != "https" or parsed.hostname not in ("chat.z.ai", "zcode.z.ai", "z.ai"):
            raise ConnectorError("Z.ai returned an unexpected sign-in destination.", 502)
        try:
            expiry = float(data["expires_at"])
            if expiry > 100_000_000_000:
                expiry /= 1000
            interval = max(2, float(data["poll_interval_sec"]))
            flow_id = str(data["flow_id"])
        except (KeyError, ValueError, TypeError):
            raise ConnectorError("Z.ai returned an invalid sign-in response.", 502) from None
        if not flow_id or expiry <= time.time():
            raise ConnectorError("This sign-in has already expired. Start again.", 401)
        local_id = secrets.token_urlsafe(24)
        with self.lock:
            # One outstanding login. Cancelling prevents a late result replacing
            # the account after the user starts another login.
            self.flows.clear()
            self.flows[local_id] = {"id": flow_id, "poll_token": poll_token,
                                    "expires": expiry, "interval": interval, "next": 0}
        return {"flow": local_id, "url": data["authorize_url"], "interval": interval, "expires": expiry}

    def zai_poll(self, local_id):
        with self.lock:
            flow = self.flows.get(local_id)
            if flow is None or flow["expires"] < time.time():
                self.flows.pop(local_id, None)
                raise ConnectorError("This sign-in expired or was cancelled. Start again.", 401)
            if time.time() < flow["next"]:
                return {"status": "pending"}
            flow["next"] = time.time() + flow["interval"]
        data = envelope(remote_json(ZCODE + "/oauth/cli/poll/" + urllib.parse.quote(flow["id"], safe=""),
                                    headers={"Authorization": "Bearer " + flow["poll_token"]}))
        if data.get("status") == "pending":
            return {"status": "pending"}
        if data.get("status") != "ready" or not isinstance(data.get("token"), str):
            raise ConnectorError("Z.ai did not complete the sign-in. Try again.", 401)
        with self.lock:
            if self.flows.get(local_id) is not flow:
                raise ConnectorError("This sign-in was cancelled.", 409)
            # Store only the backend JWT needed for Start Plan. Do not store
            # browser cookies, passwords, or paid Coding Plan access tokens.
            account = {"token": data["token"], "user": {
                "name": (data.get("user") or {}).get("name", "Z.ai account")}}
            atomic_write(self.directory / "zai-account.dpapi", protect(json.dumps(account).encode()))
            self.flows.pop(local_id, None)
            self.models = []
        return {"status": "signed_in", "message": "Signed in. Check your Start Plan allowance to enable models."}

    def check_zai(self):
        balance = remote_json(ZCODE + "/zcode-plan/billing/balance?app_version=" + VERSION, headers=self.zai_headers())
        envelope(balance)
        config = envelope(remote_json(ZCODE + "/client/configs?platform=win32-x64"))
        models = allowed_models(balance, config.get("builtinModels") or [])
        with self.lock:
            self.models = models
            self.balance = balance
        return {"models": models, "message": "Start Plan allowance available." if models else
                "Signed in, but no usable Start Plan model allowance was reported. No paid fallback is used."}

    def register(self, provider):
        if provider == "zai":
            models = self.check_zai()["models"]
            base = ORIGIN + "/zai/v1"
            name = "jcode-zai-account"
        elif provider in LOCAL_SERVICES:
            label, _ = LOCAL_SERVICES[provider]
            data = remote_json(LOCAL_SERVICES[provider][1] + "/models", timeout=5)
            models = [m["id"] for m in data.get("data", []) if isinstance(m, dict) and isinstance(m.get("id"), str)]
            base = ORIGIN + "/" + provider + "/v1"
            name = "jcode-local-" + provider
        else:
            raise ConnectorError("Unknown connector.")
        block = profile_text(name, base, models, self.secret)
        with self.lock:
            config_path = self.home / "config.toml"
            original = config_path.read_text(encoding="utf-8-sig") if config_path.exists() else ""
            parsed = tomllib.loads(original)
            start = f"# BEGIN JCODE KEYLESS {name}"
            end = f"# END JCODE KEYLESS {name}"
            if start in original:
                before, rest = original.split(start, 1)
                if end not in rest:
                    raise ConnectorError("An incomplete managed profile was found. Configuration was not changed.")
                _, after = rest.split(end, 1)
                updated = before + start + "\n" + block + end + after
            else:
                if name in parsed.get("providers", {}):
                    raise ConnectorError("A profile with this name already exists. It was not overwritten.", 409)
                updated = original.rstrip() + "\n\n" + start + "\n" + block + end + "\n"
            tomllib.loads(updated)
            if updated != original:
                if original:
                    backup = self.directory / ("config-before-" + time.strftime("%Y%m%d-%H%M%S") + "-" + secrets.token_hex(3) + ".toml")
                    atomic_write(backup, original.encode())
                atomic_write(config_path, updated.encode())
        return {"models": models, "profile": name,
                "message": "Enabled. In Jcode, refresh Choose model. If it is not listed, reopen Jcode to reload provider profiles."}

    def public_status(self):
        name = None
        saved = (self.directory / "zai-account.dpapi").exists()
        if saved:
            try:
                name = self.credentials()["user"]["name"]
            except ConnectorError:
                pass
        return {"version": VERSION, "zai": {"saved": saved, "name": name, "models": self.models},
                "jobs": dict(self.jobs)}

    def disconnect_zai(self):
        with self.lock:
            self.flows.clear()
            self.models = []
            self.balance = None
            config = self.home / "config.toml"
            if config.exists():
                original = config.read_text(encoding="utf-8-sig")
                start = "# BEGIN JCODE KEYLESS jcode-zai-account"
                end = "# END JCODE KEYLESS jcode-zai-account"
                if start in original:
                    before, rest = original.split(start, 1)
                    if end not in rest:
                        raise ConnectorError("The managed profile is incomplete. No settings were changed.")
                    _, after = rest.split(end, 1)
                    updated = before + after
                    tomllib.loads(updated)
                    atomic_write(self.directory / ("config-before-disconnect-" + secrets.token_hex(6) + ".toml"), original.encode())
                    atomic_write(config, updated.encode())
            (self.directory / "zai-account.dpapi").unlink(missing_ok=True)
        return {"message": "Local Z.ai sign-in removed. You can sign in again at any time."}

    def launch_login(self, provider):
        if provider not in ("gemini", "copilot"):
            raise ConnectorError("Unsupported account sign-in.")
        with self.lock:
            if self.jobs.get(provider) == "waiting" and provider in self.native_flows:
                raise ConnectorError("A sign-in is already pending. Complete it below or reopen Accounts in Jcode.", 409)
            flow = secrets.token_hex(16)
            data = self.native_operation(provider, flow, "--print-auth-url")
            url = urllib.parse.urlparse(data.get("auth_url", ""))
            hosts = {"gemini": {"accounts.google.com"}, "copilot": {"github.com"}}[provider]
            if data.get("status") != "pending" or url.scheme != "https" or url.hostname not in hosts:
                raise ConnectorError("Jcode did not return a valid provider sign-in link.", 502)
            kind = data.get("input_kind")
            if kind not in ("auth_code", "complete"):
                raise ConnectorError("Use the native Accounts sign-in for this authentication method.")
            self.native_flows[provider] = flow
            self.jobs[provider] = "waiting"
        if kind == "complete":
            self.complete_native(provider, None)
        return {"url": data["auth_url"], "user_code": data.get("user_code"), "input_kind": kind,
                "message": "Complete the provider sign-in in your browser. Never enter your password here."}

    def native_operation(self, provider, flow, operation, value=None):
        binary = os.environ.get("JCODE_BIN", "jcode")
        command = [binary, "--no-update", "--no-selfdev", "login", "--provider", provider,
                   "--no-browser", "--json", "--flow-id", flow, operation]
        if value is not None:
            command.append("-")  # Authorization codes travel on stdin, never the command line.
        try:
            result = subprocess.run(command, input=(value or "").encode(), capture_output=True,
                                    creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0), timeout=900)
            if len(result.stdout) > 65536:
                raise ConnectorError("Jcode returned an oversized sign-in response.", 502)
            # Some supported versions append a human validation report after
            # this first JSON object. Never show raw stdout/stderr to the UI.
            return json.loads(result.stdout.splitlines()[0])
        except (OSError, subprocess.TimeoutExpired, ValueError, IndexError):
            raise ConnectorError("Jcode could not complete this sign-in. Retry from the native Accounts panel.", 502) from None

    def complete_native(self, provider, code):
        with self.lock:
            flow = self.native_flows.get(provider)
            if not flow or provider not in ("gemini", "copilot"):
                raise ConnectorError("No pending sign-in was found.")
            if provider == "gemini" and (not isinstance(code, str) or not code.strip() or len(code) > 16384):
                raise ConnectorError("Paste the authorization code shown by Google, not a password.")
            self.jobs[provider] = "waiting"
        def finish():
            try:
                result = self.native_operation(provider, flow, "--auth-code" if code is not None else "--complete", code)
                self.jobs[provider] = "completed" if result.get("status") == "authenticated" else "needs_attention"
            except ConnectorError:
                self.jobs[provider] = "needs_attention"
            finally:
                with self.lock:
                    if self.native_flows.get(provider) == flow:
                        self.native_flows.pop(provider, None)
        threading.Thread(target=finish, daemon=True).start()
        return {"message": "Completing the account connection…"}


class Server(ThreadingHTTPServer):
    daemon_threads = True
    allow_reuse_address = False


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server_version = "JcodeConnectors/" + VERSION

    def log_message(self, *_):
        pass  # Never log auth URLs, account identity, prompts, or provider data.

    @property
    def state(self):
        return self.server.state

    def valid_host(self):
        return self.headers.get("Host") == f"127.0.0.1:{self.server.server_port}"

    def authorized(self):
        origin = self.headers.get("Origin")
        if origin and origin != f"http://127.0.0.1:{self.server.server_port}":
            return False
        supplied = self.headers.get("X-Jcode-Connector", "")
        authorization = self.headers.get("Authorization", "")
        if not supplied and authorization.startswith("Bearer "):
            supplied = authorization[7:]
        return self.valid_host() and hmac.compare_digest(supplied, self.state.secret)

    def reply(self, value, status=200, content_type="application/json; charset=utf-8"):
        data = value.encode() if isinstance(value, str) else json.dumps(value, ensure_ascii=False).encode()
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("X-Content-Type-Options", "nosniff")
        self.send_header("Referrer-Policy", "no-referrer")
        self.send_header("Content-Security-Policy", "default-src 'self'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'")
        self.end_headers()
        self.wfile.write(data)

    def do_GET(self):
        if not self.valid_host():
            return self.reply({"error": "Invalid host."}, 403)
        path = urllib.parse.urlparse(self.path).path
        if path == "/health":
            return self.reply({"service": "jcode-keyless-connectors", "version": VERSION})
        if path == "/":
            return self.reply((Path(__file__).parent / "index.html").read_text(encoding="utf-8"),
                              content_type="text/html; charset=utf-8")
        if not self.authorized():
            return self.reply({"error": "Open this page from Jcode Accounts."}, 401)
        if path == "/api/status":
            return self.reply(self.state.public_status())
        if path == "/zai/v1/models":
            return self.reply({"object": "list", "data": [{"id": m, "object": "model", "owned_by": "zai-account"} for m in self.state.models]})
        for provider, (_, base) in LOCAL_SERVICES.items():
            if path == f"/{provider}/v1/models":
                try:
                    return self.reply(remote_json(base + "/models", timeout=5))
                except ConnectorError as error:
                    return self.reply({"error": str(error)}, error.status)
        return self.reply({"error": "Not found."}, 404)

    def do_POST(self):
        if not self.authorized():
            return self.reply({"error": "Unauthorized local request."}, 401)
        try:
            if self.headers.get_content_type() != "application/json":
                raise ConnectorError("Expected application/json.", 415)
            length = int(self.headers.get("Content-Length", "0"))
            if not 0 < length <= MAX_BODY:
                raise ConnectorError("Request body is empty or too large.", 413)
            body = json.loads(self.rfile.read(length))
            if not isinstance(body, dict):
                raise ConnectorError("Expected a JSON object.")
            path = urllib.parse.urlparse(self.path).path
            if path == "/api/zai/start":
                return self.reply(self.state.zai_start())
            if path == "/api/zai/poll":
                return self.reply(self.state.zai_poll(body.get("flow")))
            if path == "/api/zai/cancel":
                with self.state.lock:
                    self.state.flows.pop(body.get("flow"), None)
                return self.reply({"message": "Sign-in cancelled."})
            if path == "/api/zai/check":
                return self.reply(self.state.check_zai())
            if path == "/api/zai/disconnect":
                return self.reply(self.state.disconnect_zai())
            if path == "/api/enable":
                return self.reply(self.state.register(body.get("provider")))
            if path == "/api/login":
                return self.reply(self.state.launch_login(body.get("provider")))
            if path == "/api/login/complete":
                return self.reply(self.state.complete_native(body.get("provider"), body.get("code")))
            if path.endswith("/v1/chat/completions"):
                provider = path.split("/")[1]
                if provider == "zai":
                    # Recheck entitlement at request time. Never use stale quotas
                    # to select a different paid route or model.
                    allowed = self.state.check_zai()["models"]
                    if body.get("model") not in allowed:
                        raise ConnectorError("This model has no active Start Plan allowance. Check your account.", 403)
                    return self.proxy(ZCODE + "/zcode-plan/chat/completions", body, self.state.zai_headers())
                if provider in LOCAL_SERVICES:
                    return self.proxy(LOCAL_SERVICES[provider][1] + "/chat/completions", body, {})
            return self.reply({"error": "Not found."}, 404)
        except ConnectorError as error:
            return self.reply({"error": {"message": str(error), "type": "connector_error"}}, error.status)
        except (ValueError, TypeError):
            return self.reply({"error": {"message": "Invalid request body."}}, 400)
        except (BrokenPipeError, ConnectionResetError):
            return
        except Exception:
            return self.reply({"error": {"message": "Connector operation failed. Retry or reopen from Jcode Accounts."}}, 500)

    def proxy(self, url, body, headers):
        request = urllib.request.Request(url, json.dumps(body).encode(), headers={
            "Content-Type": "application/json", "Accept": "text/event-stream" if body.get("stream") else "application/json",
            "User-Agent": "Jcode-Account-Connectors/" + VERSION, **headers})
        try:
            remote = urllib.request.urlopen(request, timeout=180)
        except urllib.error.HTTPError as error:
            status = error.code
            error.close()
            raise ConnectorError(f"The model request was rejected (HTTP {status}). Check sign-in and allowance.", status) from None
        except OSError:
            raise ConnectorError("The model service could not be reached.", 502) from None
        # Preserve native tool-call frames exactly. No text parsing or synthetic
        # tool execution, no guessed token usage and no response replay.
        with remote:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream" if body.get("stream") else "application/json")
            self.send_header("Cache-Control", "no-store")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            try:
                if body.get("stream"):
                    while line := remote.readline():
                        self.wfile.write(line)
                        self.wfile.flush()
                else:
                    while chunk := remote.read(64 * 1024):
                        self.wfile.write(chunk)
            except (OSError, TimeoutError):
                # Headers already sent: close the stream, don't append an error
                # JSON document that would corrupt it or claim completion.
                self.close_connection = True


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--home", type=Path, default=Path(os.environ.get("JCODE_HOME", str(Path.home() / ".jcode"))))
    parser.add_argument("--port", type=int, default=PORT)
    args = parser.parse_args()
    server = Server(("127.0.0.1", args.port), Handler)
    server.state = State(args.home)
    server.serve_forever(poll_interval=0.5)


if __name__ == "__main__":
    main()
