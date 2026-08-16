#!/usr/bin/env python3
"""Run the two-browser product acceptance flow against a local application server.

The harness uses Chrome's DevTools protocol directly so the repository does not
own application JavaScript or require a browser-automation package at runtime.
"""

from __future__ import annotations

import base64
import hashlib
import http.server
import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import threading
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
HOST = "127.0.0.1"
OIDC_HOST = "localhost"
PORT = 18343
OIDC_PORT = 18344
AVATAR_PORT = 18345
BASE_URL = f"http://{HOST}:{PORT}"
# Keep the fake provider on a different browser site so callback cookie behavior
# matches the production cross-site OIDC redirect chain.
OIDC_ISSUER = f"http://{OIDC_HOST}:{OIDC_PORT}"
AVATAR_URL = f"{OIDC_ISSUER}/avatar.png"
TIMEOUT = 20.0


BUNDLED_WORDS = frozenset(
    (ROOT / "packages/game_domain/data/enable1.txt").read_text().splitlines()
)


def bundled_dictionary_contains(word: str) -> bool:
    return word.lower() in BUNDLED_WORDS


class AcceptanceError(RuntimeError):
    pass


def base64url(value: bytes) -> str:
    return base64.urlsafe_b64encode(value).rstrip(b"=").decode()


class FakeOidcProvider:
    """Small deterministic OIDC provider used only by browser acceptance."""

    def __init__(self, directory: pathlib.Path) -> None:
        self.key = directory / "oidc-key.pem"
        self.key_id = "acceptance-key"
        self._generate_key()
        self.codes: dict[str, dict[str, str]] = {}
        self.next_subject = 1
        self.next_login_subjects: list[int] = []
        self.next_authorization_faults: list[str] = []
        self.next_token_faults: list[str] = []
        self.next_token_delays: list[float] = []
        self.next_avatar_failures = 0
        self.subject_names: dict[int, str] = {}
        self.avatar_requests = 0
        self.authorization_requests: list[dict[str, list[str]]] = []
        self.last_callback_url: str | None = None
        self.avatar_png = base64.b64decode(
            "iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAADklEQVR4nGNg+A+FMAYAQ84H+fei4u8AAAAASUVORK5CYII="
        )
        provider = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, format: str, *args: Any) -> None:
                return

            def json_response(self, value: object) -> None:
                body = json.dumps(value).encode()
                self.send_response(200)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def do_GET(self) -> None:
                parsed = urllib.parse.urlparse(self.path)
                if parsed.path == "/.well-known/openid-configuration":
                    self.json_response({
                        "issuer": OIDC_ISSUER,
                        "authorization_endpoint": f"{OIDC_ISSUER}/authorize",
                        "token_endpoint": f"{OIDC_ISSUER}/token",
                        "jwks_uri": f"{OIDC_ISSUER}/jwks",
                        "response_types_supported": ["code"],
                        "subject_types_supported": ["public"],
                        "id_token_signing_alg_values_supported": ["RS256"],
                        "scopes_supported": ["openid", "profile"],
                        "token_endpoint_auth_methods_supported": ["client_secret_basic"],
                        "claims_supported": ["iss", "sub", "aud", "exp", "iat", "nonce", "name", "picture"],
                        "code_challenge_methods_supported": ["S256"],
                    })
                    return
                if parsed.path == "/jwks":
                    self.json_response({"keys": [provider.jwk]})
                    return
                if parsed.path == "/avatar.png":
                    provider.avatar_requests += 1
                    if provider.next_avatar_failures:
                        provider.next_avatar_failures -= 1
                        self.send_error(503)
                        return
                    self.send_response(200)
                    self.send_header("Content-Type", "image/png")
                    self.send_header("Content-Length", str(len(provider.avatar_png)))
                    self.end_headers()
                    self.wfile.write(provider.avatar_png)
                    return
                if parsed.path == "/authorize":
                    query = urllib.parse.parse_qs(parsed.query)
                    provider.authorization_requests.append(query)
                    fault = provider.next_authorization_faults.pop(0) if provider.next_authorization_faults else None
                    if fault == "denied":
                        location = query["redirect_uri"][0] + "?" + urllib.parse.urlencode(
                            {
                                "error": "access_denied",
                                "error_description": "acceptance denial secret must not be reflected",
                                "state": query["state"][0],
                            }
                        )
                        provider.last_callback_url = location
                        self.send_response(302)
                        self.send_header("Location", location)
                        self.end_headers()
                        return
                    if provider.next_login_subjects:
                        subject = provider.next_login_subjects.pop(0)
                    else:
                        subject = provider.next_subject
                        provider.next_subject += 1
                    name = provider.subject_names.get(subject, f"Acceptance Player {subject}")
                    code = base64url(os.urandom(24))
                    provider.codes[code] = {
                        "nonce": query["nonce"][0],
                        "challenge": query["code_challenge"][0],
                        "subject": f"acceptance-subject-{subject}",
                        "name": name,
                    }
                    separator = "&" if "?" in query["redirect_uri"][0] else "?"
                    location = (
                        query["redirect_uri"][0]
                        + separator
                        + urllib.parse.urlencode({"code": code, "state": query["state"][0]})
                    )
                    self.send_response(302)
                    self.send_header("Location", location)
                    self.end_headers()
                    provider.last_callback_url = location
                    return

            def do_POST(self) -> None:
                if self.path != "/token":
                    self.send_error(404)
                    return
                length = int(self.headers.get("Content-Length", "0"))
                form = urllib.parse.parse_qs(self.rfile.read(length).decode())
                code = form.get("code", [""])[0]
                verifier = form.get("code_verifier", [""])[0]
                attempt = provider.codes.pop(code, None)
                challenge = base64url(hashlib.sha256(verifier.encode()).digest())
                if attempt is None or challenge != attempt["challenge"]:
                    self.send_error(400)
                    return
                fault = provider.next_token_faults.pop(0) if provider.next_token_faults else None
                if provider.next_token_delays:
                    time.sleep(provider.next_token_delays.pop(0))
                if fault == "outage":
                    self.send_error(503)
                    return
                if fault == "malformed":
                    body = b"not-json"
                    self.send_response(200)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(body)))
                    self.end_headers()
                    self.wfile.write(body)
                    return
                now = int(time.time())
                claims = {
                    "iss": OIDC_ISSUER,
                    "sub": attempt["subject"],
                    "aud": "acceptance-client",
                    "exp": now + 300,
                    "iat": now,
                    "nonce": attempt["nonce"],
                    "name": attempt["name"],
                    "picture": AVATAR_URL,
                }
                if fault == "wrong_nonce":
                    claims["nonce"] = "wrong-acceptance-nonce"
                elif fault == "wrong_issuer":
                    claims["iss"] = "https://wrong-issuer.example"
                elif fault == "wrong_audience":
                    claims["aud"] = "wrong-acceptance-client"
                elif fault == "wrong_authorized_party":
                    claims["aud"] = ["acceptance-client", "another-client"]
                    claims["azp"] = "wrong-acceptance-client"
                elif fault == "missing_authorized_party":
                    claims["aud"] = ["acceptance-client", "another-client"]
                elif fault == "expired":
                    claims["exp"] = now - 1
                elif fault == "future_issued_at":
                    claims["iat"] = now + 3600
                elif fault == "old_issued_at":
                    claims["iat"] = now - 3600
                elif fault == "empty_subject":
                    claims["sub"] = ""
                header = base64url(json.dumps({"alg": "RS256", "kid": provider.key_id, "typ": "JWT"}, separators=(",", ":")).encode())
                payload = base64url(json.dumps(claims, separators=(",", ":")).encode())
                signing_input = f"{header}.{payload}".encode()
                signature = subprocess.run(
                    ["openssl", "dgst", "-sha256", "-sign", str(provider.key)],
                    input=signing_input,
                    check=True,
                    capture_output=True,
                ).stdout
                if fault == "bad_signature":
                    signature = bytes([signature[0] ^ 1]) + signature[1:]
                self.json_response({
                    "access_token": "acceptance-access-token",
                    "token_type": "Bearer",
                    "expires_in": 300,
                    "id_token": f"{header}.{payload}.{base64url(signature)}",
                })

        self.server = http.server.ThreadingHTTPServer((HOST, OIDC_PORT), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)

    def _generate_key(self) -> None:
        subprocess.run(
            ["openssl", "genpkey", "-algorithm", "RSA", "-pkeyopt", "rsa_keygen_bits:2048", "-out", str(self.key)],
            check=True,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        details = subprocess.run(
            ["openssl", "pkey", "-in", str(self.key), "-pubout", "-text", "-noout"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout
        modulus_text = details.split("Modulus:", 1)[1].split("Exponent:", 1)[0]
        modulus = bytes.fromhex("".join(modulus_text.replace(":", " ").split()))
        exponent = int(details.split("Exponent:", 1)[1].split()[0])
        self.jwk = {
            "kty": "RSA",
            "kid": self.key_id,
            "use": "sig",
            "alg": "RS256",
            "n": base64url(modulus),
            "e": base64url(exponent.to_bytes((exponent.bit_length() + 7) // 8, "big")),
        }

    def rotate_key(self) -> None:
        self.key_id = f"acceptance-key-{time.time_ns()}"
        self._generate_key()

    def start(self) -> None:
        self.thread.start()

    def close(self) -> None:
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


class WebSocket:
    def __init__(self, url: str) -> None:
        if not url.startswith("ws://"):
            raise AcceptanceError(f"unsupported DevTools URL: {url}")
        authority, path = url[5:].split("/", 1)
        host, port = authority.rsplit(":", 1)
        self.socket = socket.create_connection((host, int(port)), timeout=TIMEOUT)
        key = base64.b64encode(os.urandom(16)).decode()
        request = (
            f"GET /{path} HTTP/1.1\r\nHost: {authority}\r\nUpgrade: websocket\r\n"
            f"Connection: Upgrade\r\nSec-WebSocket-Key: {key}\r\n"
            "Sec-WebSocket-Version: 13\r\n\r\n"
        )
        self.socket.sendall(request.encode())
        response = self._read_until(b"\r\n\r\n")
        if b" 101 " not in response.split(b"\r\n", 1)[0]:
            raise AcceptanceError(f"DevTools WebSocket upgrade failed: {response!r}")

    def _read_until(self, marker: bytes) -> bytes:
        data = b""
        while marker not in data:
            chunk = self.socket.recv(4096)
            if not chunk:
                raise AcceptanceError("DevTools WebSocket closed")
            data += chunk
        return data

    def _read_exactly(self, length: int) -> bytes:
        data = b""
        while len(data) < length:
            chunk = self.socket.recv(length - len(data))
            if not chunk:
                raise AcceptanceError("DevTools WebSocket closed")
            data += chunk
        return data

    def send(self, payload: dict[str, Any]) -> None:
        body = json.dumps(payload).encode()
        mask = os.urandom(4)
        header = bytearray([0x81])
        length = len(body)
        if length < 126:
            header.append(0x80 | length)
        elif length < 65536:
            header.append(0x80 | 126)
            header.extend(struct.pack("!H", length))
        else:
            header.append(0x80 | 127)
            header.extend(struct.pack("!Q", length))
        header.extend(mask)
        header.extend(byte ^ mask[index % 4] for index, byte in enumerate(body))
        self.socket.sendall(header)

    def receive(self) -> dict[str, Any]:
        first, second = self._read_exactly(2)
        opcode = first & 0x0F
        length = second & 0x7F
        if length == 126:
            length = struct.unpack("!H", self._read_exactly(2))[0]
        elif length == 127:
            length = struct.unpack("!Q", self._read_exactly(8))[0]
        if second & 0x80:
            mask = self._read_exactly(4)
        else:
            mask = None
        payload = self._read_exactly(length)
        if mask:
            payload = bytes(byte ^ mask[index % 4] for index, byte in enumerate(payload))
        if opcode == 0x8:
            raise AcceptanceError("DevTools WebSocket closed")
        if opcode == 0x9:
            self.send({})
            return self.receive()
        return json.loads(payload)

    def close(self) -> None:
        self.socket.close()


class DevTools:
    def __init__(self, url: str) -> None:
        self.websocket = WebSocket(url)
        self.next_id = 1

    def call(self, method: str, params: dict[str, Any] | None = None) -> dict[str, Any]:
        call_id = self.next_id
        self.next_id += 1
        self.websocket.send({"id": call_id, "method": method, "params": params or {}})
        while True:
            response = self.websocket.receive()
            if response.get("id") != call_id:
                continue
            if "error" in response:
                raise AcceptanceError(f"{method} failed: {response['error']}")
            return response.get("result", {})

    def close(self) -> None:
        self.websocket.close()


@dataclass
class Browser:
    process: subprocess.Popen[str]
    profile: pathlib.Path
    tools: DevTools

    @classmethod
    def launch(cls, chrome: str, debugging_port: int, profile: pathlib.Path) -> "Browser":
        process = subprocess.Popen(
            [
                chrome,
                "--headless=new",
                "--disable-gpu",
                "--no-first-run",
                "--no-default-browser-check",
                f"--remote-debugging-port={debugging_port}",
                f"--user-data-dir={profile}",
                "about:blank",
            ],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            text=True,
        )
        deadline = time.monotonic() + TIMEOUT
        target: dict[str, Any] | None = None
        while time.monotonic() < deadline:
            if process.poll() is not None:
                raise AcceptanceError("Chrome exited before DevTools became ready")
            try:
                targets = json.load(urllib.request.urlopen(f"http://{HOST}:{debugging_port}/json", timeout=1))
                target = next(item for item in targets if item["type"] == "page")
                break
            except (OSError, StopIteration):
                time.sleep(0.1)
        if target is None:
            raise AcceptanceError("Chrome DevTools did not become ready")
        tools = DevTools(target["webSocketDebuggerUrl"])
        tools.call("Page.enable")
        tools.call("Runtime.enable")
        return cls(process, profile, tools)

    def evaluate(self, expression: str) -> Any:
        result = self.tools.call(
            "Runtime.evaluate",
            {"expression": expression, "awaitPromise": True, "returnByValue": True},
        )
        exception = result.get("exceptionDetails")
        if exception:
            raise AcceptanceError(f"browser expression failed: {exception}")
        return result.get("result", {}).get("value")

    def navigate(self, path: str) -> None:
        self.tools.call("Page.navigate", {"url": f"{BASE_URL}{path}"})
        self.wait("document.readyState === 'complete'")

    def wait(self, expression: str, timeout: float = TIMEOUT) -> Any:
        deadline = time.monotonic() + timeout
        last: Any = None
        while time.monotonic() < deadline:
            try:
                last = self.evaluate(expression)
            except AcceptanceError:
                time.sleep(0.1)
                continue
            if last:
                return last
            time.sleep(0.1)
        context = self.evaluate("({ url: location.href, text: document.body?.innerText?.slice(0, 2000) ?? '', html: document.body?.innerHTML?.slice(0, 2000) ?? '' })")
        raise AcceptanceError(
            f"timed out waiting for {expression}; last value: {last!r}; browser: {context!r}"
        )

    def submit(self, selector: str, values: dict[str, str] | None = None) -> None:
        values_json = json.dumps(values or {})
        selector_json = json.dumps(selector)
        self.evaluate(
            f"""(() => {{
                const form = document.querySelector({selector_json});
                if (!form) throw new Error('missing form: ' + {selector_json});
                for (const [name, value] of Object.entries({values_json})) {{
                    const input = form.elements.namedItem(name);
                    if (!input) throw new Error('missing input: ' + name);
                    input.value = value;
                    input.dispatchEvent(new Event('input', {{ bubbles: true }}));
                    input.dispatchEvent(new Event('change', {{ bubbles: true }}));
                }}
                form.querySelector('[type=submit]').click();
                return true;
            }})()"""
        )

    def full_submit(self, selector: str, values: dict[str, str]) -> None:
        values_json = json.dumps(values)
        selector_json = json.dumps(selector)
        self.evaluate(
            f"""(() => {{
                const form = document.querySelector({selector_json});
                if (!form) throw new Error('missing form: ' + {selector_json});
                for (const [name, value] of Object.entries({values_json})) {{
                    const input = form.elements.namedItem(name);
                    if (!input) throw new Error('missing input: ' + name);
                    input.value = value;
                }}
                form.method = 'POST';
                form.action = '/account/migrate';
                form.submit();
                return true;
            }})()"""
        )

    def close(self) -> None:
        self.tools.close()
        self.process.terminate()
        try:
            self.process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.process.kill()


def chrome_binary() -> str:
    configured = os.environ.get("CHROME_BIN")
    candidates = [
        configured,
        shutil.which("google-chrome"),
        shutil.which("chromium"),
        "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    ]
    for candidate in candidates:
        if candidate and pathlib.Path(candidate).is_file():
            return candidate
    raise AcceptanceError("Chrome not found; set CHROME_BIN")


def wait_for_server(process: subprocess.Popen[str]) -> None:
    deadline = time.monotonic() + 120
    while time.monotonic() < deadline:
        if process.poll() is not None:
            raise AcceptanceError("application server exited during startup")
        try:
            urllib.request.urlopen(BASE_URL, timeout=1)
            return
        except OSError:
            time.sleep(0.2)
    raise AcceptanceError("application server did not become ready")


def set_viewport(browser: Browser, width: int, height: int = 900) -> None:
    browser.tools.call(
        "Emulation.setDeviceMetricsOverride",
        {
            "width": width,
            "height": height,
            "deviceScaleFactor": 1,
            "mobile": False,
        },
    )


def assert_responsive_shell(
    browser: Browser, path: str, content_selector: str, width: int
) -> None:
    set_viewport(browser, width)
    browser.navigate(path)
    browser.wait(f"Boolean(document.querySelector({json.dumps(content_selector)}))")
    layout = browser.evaluate(
        f"""(() => {{
            const content = document.querySelector({json.dumps(content_selector)});
            const rect = content?.getBoundingClientRect();
            return {{
                viewport: document.querySelector('meta[name="viewport"]')?.content ?? null,
                documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
                contentLeft: rect?.left ?? null,
                contentRight: rect?.right ?? null,
                contentWidth: rect?.width ?? null,
            }};
        }})()"""
    )
    if layout["viewport"] != "width=device-width, initial-scale=1":
        raise AcceptanceError(f"responsive viewport metadata is missing: {layout!r}")
    if layout["documentOverflow"] != 0:
        raise AcceptanceError(f"{path} overflows at {width}px: {layout!r}")
    if (
        layout["contentLeft"] is None
        or layout["contentRight"] is None
        or layout["contentLeft"] < 0
        or layout["contentRight"] > width
    ):
        raise AcceptanceError(f"{path} content escapes its shell at {width}px: {layout!r}")


def assert_game_controls(browser: Browser, width: int) -> None:
    set_viewport(browser, width)
    path = browser.evaluate("location.pathname")
    browser.navigate(path)
    browser.wait("Boolean(document.querySelector('#game-board'))")
    original_zoom = browser.evaluate("document.querySelector('#game-board')?.dataset.boardZoom")
    original_square_size = browser.evaluate(
        "document.querySelector('#game-board .board-square')?.getBoundingClientRect().width"
    )
    browser.submit('form:has(input[value="ZOOM_OUT"])')
    browser.wait(
        f"document.querySelector('#game-board')?.dataset.boardZoom !== {json.dumps(original_zoom)}"
    )
    compact_zoom = browser.evaluate("document.querySelector('#game-board')?.dataset.boardZoom")
    compact_square_size = browser.evaluate(
        "document.querySelector('#game-board .board-square')?.getBoundingClientRect().width"
    )
    if compact_square_size >= original_square_size:
        raise AcceptanceError(
            f"zoom out did not shrink board squares: {original_square_size} -> {compact_square_size}"
        )
    browser.submit('form:has(input[value="ZOOM_IN"])')
    browser.wait(
        f"document.querySelector('#game-board')?.dataset.boardZoom !== {json.dumps(compact_zoom)}"
    )
    restored_square_size = browser.evaluate(
        "document.querySelector('#game-board .board-square')?.getBoundingClientRect().width"
    )
    if restored_square_size <= compact_square_size:
        raise AcceptanceError(
            f"zoom in did not enlarge board squares: {compact_square_size} -> {restored_square_size}"
        )
    browser.submit('form:has(input[value="ZOOM_RESET"])')
    browser.wait("document.querySelector('#game-board')?.dataset.boardZoom === 'Fit'")
    fit_square_size = browser.evaluate(
        "document.querySelector('#game-board .board-square')?.getBoundingClientRect().width"
    )
    if fit_square_size >= compact_square_size:
        raise AcceptanceError(
            f"fit did not produce the smallest board squares: {compact_square_size} -> {fit_square_size}"
        )
    before_shuffle = browser.evaluate(
        "Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => tile.dataset.tileId).sort().join(',')"
    )
    browser.submit('form:has(input[value="SHUFFLE_RACK"])')
    browser.wait("Boolean(document.querySelector('#player-rack'))")
    after_shuffle = browser.evaluate(
        "Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => tile.dataset.tileId).sort().join(',')"
    )
    if before_shuffle != after_shuffle:
        raise AcceptanceError("shuffle changed rack membership")

    menu = browser.evaluate(
        """(() => {
            const board = document.querySelector('#game-board');
            const rail = document.querySelector('#activity-rail');
            const menuButton = Array.from(document.querySelectorAll('button')).find(button => button.textContent?.includes('Menu'));
            const before = board?.getBoundingClientRect();
            menuButton?.click();
            const after = board?.getBoundingClientRect();
            const railRect = rail?.getBoundingClientRect();
            const samplePoint = railRect ? {x: Math.min(railRect.right - 8, railRect.left + 40), y: Math.min(railRect.bottom - 8, railRect.top + 80)} : null;
            const topElement = samplePoint ? document.elementFromPoint(samplePoint.x, samplePoint.y) : null;
            const dock = document.querySelector('#play-console')?.getBoundingClientRect();
            return {
                shift: before && after ? Math.abs(after.top - before.top) + Math.abs(after.height - before.height) : null,
                railRight: railRect?.right ?? null,
                railBottom: railRect?.bottom ?? null,
                viewportWidth: innerWidth,
                viewportHeight: innerHeight,
                menuOnTop: Boolean(topElement && rail?.contains(topElement)),
                dockCenter: dock ? dock.left + dock.width / 2 : null,
                viewportCenter: innerWidth / 2,
            };
        })()"""
    )
    if (
        menu["shift"] != 0
        or menu["railRight"] is None
        or menu["railRight"] > menu["viewportWidth"]
        or menu["railBottom"] > menu["viewportHeight"]
        or not menu["menuOnTop"]
    ):
        raise AcceptanceError(f"game menu is not a contained top-layer overlay at {width}px: {menu!r}")
    if abs(menu["dockCenter"] - menu["viewportCenter"]) > 1:
        raise AcceptanceError(f"floating rack dock is not horizontally centered at {width}px: {menu!r}")
    browser.evaluate("Array.from(document.querySelectorAll('#activity-rail button')).find(button => button.textContent?.includes('Close'))?.click()")


def assert_responsive_game_layout(browser: Browser, width: int) -> None:
    set_viewport(browser, width)
    browser.navigate(browser.evaluate("location.pathname"))
    browser.wait("Boolean(document.querySelector('#game-board'))")
    layout = browser.evaluate(
        """(() => {
            const board = document.querySelector('#game-board');
            const scroller = board?.querySelector('.board-viewport');
            const rack = document.querySelector('#player-rack');
            const dock = document.querySelector('#play-console');
            const blankPicker = document.querySelector('.blank-letter-layer');
            const boardRect = board?.getBoundingClientRect();
            const scrollerRect = scroller?.getBoundingClientRect();
            const rackRect = rack?.getBoundingClientRect();
            const rackTiles = Array.from(rack?.querySelectorAll('.rack-tile') ?? []);
            const rackTileRects = rackTiles.map(tile => tile.getBoundingClientRect());
            const rackFaces = rackTiles.map(tile => tile.querySelector('.rack-tile-face'));
            const rackPoints = rackTiles.map(tile => tile.querySelector('.rack-tile-points'));
            const rackFaceSizes = rackFaces.map(face => Number.parseFloat(getComputedStyle(face).fontSize));
            const rackPointSizes = rackPoints.map(points => Number.parseFloat(getComputedStyle(points).fontSize));
            const rackFaceRects = rackFaces.map(face => face?.getBoundingClientRect());
            const rackPointRects = rackPoints.map(points => points?.getBoundingClientRect());
            const dockRect = dock?.getBoundingClientRect();
            const ids = Array.from(document.querySelectorAll('[id]')).map(element => element.id);
            return {
                viewport: document.querySelector('meta[name="viewport"]')?.content ?? null,
                documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
                boardOverflow: scroller ? scroller.scrollWidth - scroller.clientWidth : null,
                boardRight: scrollerRect?.right ?? null,
                boardBottom: boardRect?.bottom ?? null,
                dockTop: dockRect?.top ?? null,
                rackVisibleInViewport: Boolean(rackRect && rackRect.top >= 0 && rackRect.bottom <= innerHeight),
                rackTileCount: rackTiles.length,
                rackTilesContained: Boolean(rackRect && rackTileRects.every(rect => rect.left >= rackRect.left && rect.right <= rackRect.right)),
                rackTilesSingleRow: rackTileRects.length > 0 && Math.max(...rackTileRects.map(rect => rect.top)) - Math.min(...rackTileRects.map(rect => rect.top)) < 1,
                rackTilesSquare: rackTileRects.length > 0 && rackTileRects.every(rect => Math.abs(rect.width - rect.height) <= 1),
                rackTypeScales: rackTileRects.length > 0 && rackTileRects.every((rect, index) =>
                    rackFaces[index]
                    && rackPoints[index]
                    && rackFaceSizes[index] <= rect.width * 0.4 + 1
                    && rackPointSizes[index] <= rect.width * 0.2 + 1
                    && rackFaceRects[index].left >= rect.left
                    && rackFaceRects[index].right <= rect.right
                    && rackFaceRects[index].top >= rect.top
                    && rackFaceRects[index].bottom <= rect.bottom
                    && rackPointRects[index].left >= rect.left
                    && rackPointRects[index].right <= rect.right
                    && rackPointRects[index].top >= rect.top
                    && rackPointRects[index].bottom <= rect.bottom
                ),
                rackOverflow: rack ? rack.scrollWidth - rack.clientWidth : null,
                blankPickerContained: !blankPicker || (() => {
                    const rect = blankPicker.getBoundingClientRect();
                    return rect.left >= 0 && rect.right <= innerWidth && rect.top >= 0 && rect.bottom <= innerHeight;
                })(),
                dockVisibleInViewport: Boolean(dockRect && dockRect.top >= 0 && dockRect.bottom <= innerHeight),
                boardClearOfDock: Boolean(boardRect && dockRect && boardRect.bottom <= dockRect.top),
                hasRack: Boolean(rack),
                hasActions: Boolean(document.querySelector('#turn-actions')) || document.body.innerText.includes('is playing') || document.body.innerText.includes('Game complete'),
                hasPreview: Boolean(document.querySelector('#draft-preview')) || document.body.innerText.includes('is playing') || document.body.innerText.includes('Game complete'),
                hasAwareness: Boolean(document.querySelector('#game-awareness')),
                hasViewerBench: Boolean(document.querySelector('#viewer-bench')),
                hasTurnDock: Boolean(document.querySelector('#play-console.turn-dock')),
                hasActivityRail: Boolean(document.querySelector('#activity-rail')),
                hasScene: Boolean(document.querySelector('#app-page.game-scene')),
                hasPlatform: Boolean(document.querySelector('#play-stage.board-platform')),
                menuClosed: !document.querySelector('#game-menu')?.open,
                stageTop: document.querySelector('#play-stage')?.getBoundingClientRect().top ?? null,
                stageBottom: document.querySelector('#play-stage')?.getBoundingClientRect().bottom ?? null,
                menuTop: document.querySelector('#game-menu')?.getBoundingClientRect().top ?? null,
                duplicateIds: ids.filter((id, index) => ids.indexOf(id) !== index),
            };
        })()"""
    )
    if layout["viewport"] != "width=device-width, initial-scale=1":
        raise AcceptanceError(f"responsive viewport metadata is missing: {layout!r}")
    if layout["documentOverflow"] != 0:
        raise AcceptanceError(f"game page overflows the viewport at {width}px: {layout!r}")
    if not layout["rackVisibleInViewport"] or not layout["dockVisibleInViewport"]:
        raise AcceptanceError(f"fixed rack dock is not fully visible at {width}px: {layout!r}")
    if (
        layout["rackTileCount"] != 7
        or not layout["rackTilesContained"]
        or not layout["rackTilesSingleRow"]
        or not layout["rackTilesSquare"]
        or not layout["rackTypeScales"]
        or layout["rackOverflow"] != 0
        or not layout["blankPickerContained"]
    ):
        raise AcceptanceError(f"rack tiles do not fit on one contained row at {width}px: {layout!r}")
    if not layout["boardClearOfDock"]:
        raise AcceptanceError(f"fixed rack dock obscures the board at {width}px: {layout!r}")
    if not all(layout[key] for key in [
        "hasRack", "hasActions", "hasPreview", "hasAwareness",
        "hasTurnDock", "hasActivityRail", "hasScene",
    ]):
        raise AcceptanceError(f"game interaction state is missing at {width}px: {layout!r}")
    if not layout["menuClosed"]:
        raise AcceptanceError(f"secondary game information displaces the ordinary play scene: {layout!r}")
    if layout["duplicateIds"]:
        raise AcceptanceError(f"game page contains duplicate IDs: {layout!r}")
    if width >= 800 and layout["boardOverflow"] != 0:
        raise AcceptanceError(f"desktop board unexpectedly scrolls: {layout!r}")
    if width < 800 and (
        layout["boardOverflow"] is None
        or layout["boardOverflow"] <= 0
        or layout["boardRight"] > width
    ):
        raise AcceptanceError(f"mobile board scrolling is not contained: {layout!r}")


def google_login(browser: Browser) -> None:
    browser.navigate("/login")
    google_href = browser.evaluate("document.querySelector('a[href^=\"/auth/google/start\"]')?.getAttribute('href')")
    if not google_href:
        raise AcceptanceError("Google login link is missing")
    browser.navigate(google_href)
    browser.wait("location.origin === " + json.dumps(BASE_URL))
    browser.wait("location.pathname === '/' && document.body.innerText.includes('Signed in as ')")


def failed_google_login(
    browser: Browser,
    provider: FakeOidcProvider,
    *,
    authorization_fault: str | None = None,
    token_fault: str | None = None,
    token_delay: float | None = None,
) -> None:
    before_authorizations = len(provider.authorization_requests)
    if authorization_fault:
        provider.next_authorization_faults.append(authorization_fault)
    if token_fault:
        provider.next_login_subjects.append(99)
        provider.next_token_faults.append(token_fault)
    if token_delay is not None:
        provider.next_login_subjects.append(99)
        provider.next_token_delays.append(token_delay)
    browser.navigate("/login")
    google_href = browser.evaluate("document.querySelector('a[href^=\"/auth/google/start\"]')?.getAttribute('href')")
    if not google_href:
        raise AcceptanceError("Google login link is missing")
    browser.navigate(google_href)
    browser.wait("document.body.innerText.includes('Google sign-in')")
    body = browser.evaluate("document.body.innerText")
    if "acceptance denial secret" in body or "acceptance-access-token" in body:
        raise AcceptanceError("OIDC failure reflected provider secrets")
    if len(provider.authorization_requests) != before_authorizations + 1:
        raise AcceptanceError("failed Google login did not reach the fake provider exactly once")


def logout(browser: Browser) -> None:
    browser.navigate("/logout")
    browser.submit('form[hx-post="/logout"]')
    browser.wait("document.body.innerText.includes('Sign in required')")


def profile_avatar_url(browser: Browser) -> str | None:
    return browser.evaluate(
        "document.querySelector('#dashboard-shell img[alt=\"Profile avatar\"]')?.getAttribute('src') ?? null"
    )


def create_invitation_link(browser: Browser) -> str:
    browser.submit('form:has(input[value="CREATE_INVITATION"])')
    browser.wait("document.body.innerText.includes('Invitation ready')")
    link = browser.evaluate(
        "document.querySelector('#created-invitation a[href*=\"/join?invite=\"]')?.getAttribute('href') ?? null"
    )
    if not link:
        raise AcceptanceError("private invitation link was not returned")
    return link


def set_display_name(browser: Browser, name: str) -> None:
    browser.submit(
        'form:has(input[value="SET_DISPLAY_NAME"])',
        {"display_name": name},
    )
    browser.wait(f"document.body.innerText.includes({json.dumps(name)})")


def migrate_legacy_account(browser: Browser, provider: FakeOidcProvider) -> None:
    browser.navigate("/account/migrate")
    provider.next_login_subjects.append(4)
    browser.full_submit(
        'form[method="post"]',
        {
            "username": "legacy-acceptance",
            "password": "correct horse battery staple",
        },
    )
    browser.wait("location.origin === " + json.dumps(BASE_URL))
    browser.wait("location.pathname === '/' && document.body.innerText.includes('Signed in as ')")
    text = browser.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
    if "@legacy-acceptance" not in text or "Acceptance Player 4" not in text:
        raise AcceptanceError("legacy migration did not preserve the account handle and initialize its profile")
    logout(browser)
    browser.navigate("/login")
    browser.evaluate(
        "fetch('/login', {method: 'POST', headers: {'content-type': 'application/x-www-form-urlencoded'}, body: 'username=legacy-acceptance&password=correct+horse+battery+staple'}).then(response => response.text()).then(html => { document.open(); document.write(html); document.close(); })"
    )
    browser.wait("document.body.innerText.includes('Password sign-in is no longer available')")
    provider.next_login_subjects.append(4)
    google_login(browser)
    returning = browser.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
    if "@legacy-acceptance" not in returning:
        raise AcceptanceError("migrated Google identity did not return to the existing account")


def install_readiness_probe(browser: Browser) -> None:
    source = """(() => {
        window.__acceptanceSubscriptions = [];
        window.__acceptanceUpdates = [];
        window.__acceptanceLifecycle = [];
        for (const name of ['connecting', 'connected', 'reconnecting', 'disconnected']) {
            window.addEventListener(`v-shared-state-${name}`, () => {
                window.__acceptanceLifecycle.push(name);
            });
        }
        window.addEventListener('v-shared-state-subscribed', event => {
            window.__acceptanceSubscriptions.push(JSON.parse(event.detail).channel_id);
        });
        window.addEventListener('v-shared-state-update', event => {
            window.__acceptanceUpdates.push(JSON.parse(event.detail).channel_id);
        });
    })();"""
    browser.tools.call("Page.addScriptToEvaluateOnNewDocument", {"source": source})
    browser.evaluate(source)


def active_game_path(browser: Browser) -> str:
    return browser.wait(
        "Array.from(document.querySelectorAll('a')).map(link => link.getAttribute('href')).find(href => href?.startsWith('/games/')) || ''"
    )


def wait_for_turn(browser: Browser) -> None:
    browser.wait("document.querySelector('#named-turn-status')?.textContent === 'Your move'")


def word_from_rack(
    rack: list[dict[str, str]], *, accepted: bool
) -> list[dict[str, str]]:
    letter_tiles: dict[str, list[str]] = {}
    blank_tiles = []
    for tile in rack:
        letter = tile["letter"]
        if letter == "?":
            blank_tiles.append(tile["id"])
        elif letter:
            letter_tiles.setdefault(letter.lower(), []).append(tile["id"])

    candidates = BUNDLED_WORDS if accepted else (
        f"{first}{second}"
        for first in "abcdefghijklmnopqrstuvwxyz"
        for second in "abcdefghijklmnopqrstuvwxyz"
        if not bundled_dictionary_contains(f"{first}{second}")
    )
    for word in sorted(candidates, key=lambda candidate: (len(candidate), candidate)):
        if not 2 <= len(word) <= 7 or not word.isascii() or not word.isalpha():
            continue
        available = {letter: ids.copy() for letter, ids in letter_tiles.items()}
        blanks = blank_tiles.copy()
        selected = []
        for letter in word.lower():
            ids = available.get(letter)
            if ids:
                selected.append({"id": ids.pop(), "blank": ""})
            elif blanks:
                selected.append({"id": blanks.pop(), "blank": letter.upper()})
            else:
                break
        else:
            return selected
    kind = "valid" if accepted else "invalid"
    raise AcceptanceError(f"rack has no deterministic {kind} word")


def compose_word(actor: Browser, selected: list[dict[str, str]]) -> None:
    for index, tile in enumerate(selected):
        tile_selector = f'form:has([data-tile-id="{tile["id"]}"])'
        actor.submit(tile_selector)
        actor.wait(
            f'document.querySelector(\'[data-tile-id="{tile["id"]}"]\')?.classList.contains("rack-tile-selected")'
        )
        if tile["blank"]:
            actor.submit(
                f'form:has([data-blank-letter="{tile["blank"]}"])'
            )
            actor.wait(
                f'getComputedStyle(document.querySelector(\'[data-blank-letter="{tile["blank"]}"]\')).backgroundColor === "rgb(232, 241, 227)"'
            )
        x = 7 + index
        actor.wait(
            f'Boolean(document.querySelector(\'.open-square[data-x="{x}"][data-y="7"]\'))'
        )
        actor.submit(f'form:has(.open-square[data-x="{x}"][data-y="7"])')
        actor.wait(
            f'document.querySelectorAll(\'#turn-actions form input[name^="tile_"]\').length === {index + 1}'
        )


def play_valid_word(actor: Browser, observer: Browser) -> None:
    rack = actor.evaluate(
        "Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => ({ id: tile.getAttribute('data-tile-id'), letter: tile.querySelector('span')?.textContent?.trim() }))"
    )
    selected = word_from_rack(rack, accepted=True)
    compose_word(actor, selected)
    preview = actor.evaluate(
        "document.querySelector('#draft-preview')?.innerText ?? ''"
    )
    score_bubble = actor.evaluate(
        """(() => {
            const bubble = document.querySelector('.draft-score-bubble');
            if (!bubble) return null;
            const bubbleRect = bubble.getBoundingClientRect();
            const centerX = bubbleRect.left + bubbleRect.width / 2;
            const centerY = bubbleRect.top + bubbleRect.height / 2;
            const topElement = document.elementFromPoint(centerX, centerY);
            return {
                text: bubble.textContent?.trim() ?? '',
                topmost: bubbleRect.width > 0 && bubbleRect.height > 0 && topElement !== null && !topElement.closest('.board-square'),
                topTag: topElement?.tagName ?? null,
                topClass: topElement?.className ?? null,
            };
        })()"""
    )
    if "points" not in preview or not score_bubble or not score_bubble["text"] or not score_bubble["topmost"]:
        raise AcceptanceError(
            f"server-derived play preview or topmost score bubble is missing: preview={preview!r}; bubble={score_bubble!r}"
        )

    observer_revision = observer.evaluate(
        "document.querySelector('#game-board')?.getAttribute('data-revision')"
    )
    observer_open_squares = observer.evaluate(
        "document.querySelectorAll('#game-board .open-square').length"
    )
    observer_update_count = observer.evaluate("window.__acceptanceUpdates.length")

    actor.submit('#turn-actions form:has(input[value="PLAY"])')
    actor.wait(
        f'document.querySelector(\'#game-board\')?.getAttribute("data-revision") !== {json.dumps(observer_revision)}'
    )
    if not actor.evaluate("Boolean(document.querySelector('#game-board .latest-move-square'))"):
        raise AcceptanceError("accepted play was not highlighted as the latest move")
    observer.wait(
        f'window.__acceptanceUpdates.length > {observer_update_count}'
    )
    observer.wait(
        f'document.querySelector(\'#game-board\')?.getAttribute("data-revision") !== {json.dumps(observer_revision)}'
    )
    if not observer.evaluate("Boolean(document.querySelector('#game-board .latest-move-square'))"):
        raise AcceptanceError("opponent did not receive latest-move highlighting")
    observer.wait(
        f'document.querySelectorAll(\'#game-board .open-square\').length < {observer_open_squares}'
    )


def submit_invalid_word(browser: Browser) -> None:
    rack = browser.evaluate(
        "Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => ({ id: tile.getAttribute('data-tile-id'), letter: tile.querySelector('span')?.textContent?.trim() }))"
    )
    selected = word_from_rack(rack, accepted=False)
    revision = browser.evaluate(
        "document.querySelector('#game-board')?.getAttribute('data-revision')"
    )
    rack_count = browser.evaluate(
        "document.querySelectorAll('#player-rack .rack-tile').length"
    )
    compose_word(browser, selected)
    browser.submit('#turn-actions form:has(input[value="PLAY"])')
    browser.wait("document.body.innerText.includes('dictionary rejected')")
    if browser.evaluate(
        "document.querySelector('#game-board')?.getAttribute('data-revision')"
    ) != revision:
        raise AcceptanceError("invalid word changed the authoritative game revision")
    if browser.evaluate(
        "document.querySelectorAll('#player-rack .rack-tile').length"
    ) != rack_count:
        raise AcceptanceError("invalid word replaced or changed the current game view")
    browser.submit('form:has(button[type="submit"]):has(input[value="CLEAR"])')
    browser.wait("document.body.innerText.includes('Start by covering the starred center square.')")


def pass_turn(browser: Browser) -> None:
    revision = browser.evaluate("document.querySelector('[name=expected_revision]')?.value")
    browser.submit('#turn-actions form:has(input[value="CONFIRM_PASS"])')
    browser.wait('Boolean(document.querySelector(\'#turn-actions input[value="PASS"]\'))')
    browser.submit('#turn-actions form:has(input[value="PASS"])')
    browser.wait(
        f"Number(document.querySelector('[name=expected_revision]')?.value ?? {revision}) > Number({json.dumps(revision)})"
        " || document.body.innerText.includes('Game complete')"
    )


def exercise_rack_and_exchange(browser: Browser, width: int, *, submit_exchange: bool) -> None:
    set_viewport(browser, width)
    browser.navigate(browser.evaluate("location.pathname"))
    browser.wait("Boolean(document.querySelector('#turn-actions'))")
    first_tile = browser.evaluate(
        "document.querySelector('#player-rack [data-tile-id]')?.getAttribute('data-tile-id')"
    )
    original_order = browser.evaluate(
        "Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => tile.getAttribute('data-tile-id')).join(',')"
    )
    browser.submit(f'form:has(input[value="PICK_RACK_TILE"]):has(input[value="{first_tile}"])')
    browser.wait(
        f'document.querySelector(\'[data-tile-id="{first_tile}"]\')?.classList.contains("rack-tile-selected")'
    )
    browser.submit('#player-rack form:has(input[value="SWAP_RACK_TILES"]):not(:has(input[value="' + first_tile + '"]))')
    browser.wait(
        f"Array.from(document.querySelectorAll('#player-rack [data-tile-id]')).map(tile => tile.getAttribute('data-tile-id')).join(',') !== {json.dumps(original_order)}"
    )
    browser.submit('#turn-actions form:has(input[value="BEGIN_EXCHANGE"])')
    browser.wait("document.body.innerText.includes('0 tile(s) selected for exchange')")
    browser.submit('#player-rack form:has(input[value="TOGGLE_EXCHANGE"])')
    browser.wait("document.body.innerText.includes('1 tile(s) selected for exchange')")
    browser.submit('#turn-actions form:has(input[value="REVIEW_EXCHANGE"])')
    browser.wait("document.body.innerText.includes('Exchange 1 selected tile(s)?')")
    if submit_exchange:
        revision = browser.evaluate("document.querySelector('#game-board')?.getAttribute('data-revision')")
        browser.submit('#turn-actions form:has(input[value="EXCHANGE"])')
        browser.wait(
            f'document.querySelector(\'#game-board\')?.getAttribute("data-revision") !== {json.dumps(revision)}'
        )
    else:
        browser.submit('#turn-actions form:has(input[value="CANCEL_MODE"])')
        browser.wait('Boolean(document.querySelector(\'#turn-actions input[value="CONFIRM_PASS"]\'))')


def run() -> None:
    chrome = chrome_binary()
    with tempfile.TemporaryDirectory(prefix="wwmtf-browser-") as temporary:
        temp = pathlib.Path(temporary)
        database = temp / "acceptance.db"
        oidc_provider = FakeOidcProvider(temp)
        oidc_provider.start()
        environment = os.environ.copy()
        environment.update(
            {
                "WWMTF_BIND_ADDRESS": HOST,
                "WWMTF_PORT": str(PORT),
                "WWMTF_PUBLIC_BASE_URL": BASE_URL,
                "WWMTF_DATABASE_PATH": str(database),
                "WWMTF_DEV_MODE": "true",
                "WWMTF_GOOGLE_CLIENT_ID": "acceptance-client",
                "WWMTF_GOOGLE_CLIENT_SECRET": "acceptance-secret",
                "WWMTF_DEVELOPMENT_OIDC_ISSUER": OIDC_ISSUER,
                "WWMTF_DEV_BOOTSTRAP_USERNAME": "legacy-acceptance",
                "WWMTF_DEV_BOOTSTRAP_PASSWORD": "correct horse battery staple",
            }
        )
        application_command = os.environ.get("WWMTF_ACCEPTANCE_SERVER")
        if application_command:
            command = application_command.split()
        else:
            command = [
                "cargo",
                "run",
                "-p",
                "wwmtf_app",
                "--features",
                "insecure",
                "--bin",
                "wwmtf",
                "--",
                "serve",
            ]
        server = subprocess.Popen(
            command,
            cwd=ROOT,
            env=environment,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
        )
        alice: Browser | None = None
        bob: Browser | None = None
        invitee: Browser | None = None
        migrant: Browser | None = None
        try:
            wait_for_server(server)
            alice = Browser.launch(chrome, 19221, temp / "alice-profile")
            bob = Browser.launch(chrome, 19222, temp / "bob-profile")
            assert_responsive_shell(alice, "/login", "main", 390)
            set_viewport(alice, 1440)
            install_readiness_probe(alice)
            install_readiness_probe(bob)
            for fault in (
                "wrong_nonce",
                "wrong_issuer",
                "wrong_audience",
                "wrong_authorized_party",
                "missing_authorized_party",
                "expired",
                "future_issued_at",
                "old_issued_at",
                "empty_subject",
                "bad_signature",
                "malformed",
                "outage",
            ):
                failed_google_login(alice, oidc_provider, token_fault=fault)
            replay_url = oidc_provider.last_callback_url
            if not replay_url:
                raise AcceptanceError("failed OIDC callback URL was not recorded")
            alice.navigate(replay_url.removeprefix(BASE_URL))
            alice.wait("document.body.innerText.includes('Google sign-in session expired')")
            failed_google_login(alice, oidc_provider, authorization_fault="denied")
            failed_google_login(alice, oidc_provider, token_delay=11.0)
            oidc_provider.rotate_key()
            oidc_provider.next_login_subjects.append(99)
            google_login(alice)
            logout(alice)
            alice.navigate("/auth/google/callback?code=unknown&state=unknown")
            alice.wait("document.body.innerText.includes('Google sign-in session expired')")
            migrant = Browser.launch(chrome, 19224, temp / "migrant-profile")
            migrate_legacy_account(migrant, oidc_provider)
            migrant.close()
            migrant = None
            oidc_provider.next_avatar_failures = 1
            oidc_provider.next_login_subjects.append(1)
            google_login(alice)
            if profile_avatar_url(alice) is not None:
                raise AcceptanceError("failed provider avatar was unexpectedly persisted")
            alice_text = alice.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            if "Acceptance Player 1" not in alice_text:
                raise AcceptanceError("avatar failure prevented otherwise valid Google login")
            oidc_provider.next_login_subjects.append(1)
            logout(alice)
            google_login(alice)
            oidc_provider.next_login_subjects.append(2)
            google_login(bob)
            alice_text = alice.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            bob_text = bob.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            if "Acceptance Player 1" not in alice_text or "Acceptance Player 2" not in bob_text:
                raise AcceptanceError(
                    f"Google profile names were not initialized: alice={alice_text!r}; bob={bob_text!r}"
                )
            alice_avatar = profile_avatar_url(alice)
            bob_avatar = profile_avatar_url(bob)
            if not alice_avatar or not bob_avatar:
                raise AcceptanceError(
                    f"Google profile avatars were not mirrored; provider requests={oidc_provider.avatar_requests}; "
                    f"alice={alice_avatar!r}; bob={bob_avatar!r}; alice_text={alice_text!r}"
                )
            if alice.evaluate(
                f"document.querySelector('img[src={json.dumps(alice_avatar)}]')?.naturalWidth !== 128"
            ):
                raise AcceptanceError("normalized Google avatar dimensions were not rendered")
            alice_identity = alice.evaluate(
                "document.querySelector('#dashboard-shell')?.innerText?.match(/@[a-z0-9-]+-[0-9a-f]{8}/)?.[0] ?? null"
            )
            oidc_provider.next_login_subjects.append(1)
            logout(alice)
            google_login(alice)
            returning_text = alice.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            if alice_identity not in returning_text or "Acceptance Player 1" not in returning_text:
                raise AcceptanceError("returning Google login did not resolve the same WWMTF account")
            if profile_avatar_url(alice) != alice_avatar:
                raise AcceptanceError("returning Google login did not retain the mirrored avatar")

            set_display_name(alice, "Custom Acceptance Name")
            alice.submit('form:has(input[value="REMOVE_AVATAR"])')
            alice.wait("!document.querySelector('#dashboard-shell img[alt=\"Profile avatar\"]')")
            oidc_provider.subject_names[1] = "Provider Name After Customization"
            oidc_provider.next_login_subjects.append(1)
            logout(alice)
            google_login(alice)
            customized_text = alice.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            if "Custom Acceptance Name" not in customized_text:
                raise AcceptanceError("returning Google login overwrote a custom display name")
            if profile_avatar_url(alice) is not None:
                raise AcceptanceError("returning Google login restored an explicitly removed avatar")
            alice.submit('form:has(input[value="USE_GOOGLE_NAME"])')
            alice.submit('form:has(input[value="USE_GOOGLE_AVATAR"])')
            oidc_provider.subject_names[1] = "Restored Provider Name"
            oidc_provider.next_login_subjects.append(1)
            logout(alice)
            google_login(alice)
            restored_text = alice.evaluate("document.querySelector('#dashboard-shell')?.innerText ?? ''")
            if "Restored Provider Name" not in restored_text or not profile_avatar_url(alice):
                raise AcceptanceError("explicit Google profile synchronization was not restored")

            invitation_link = create_invitation_link(alice)
            invitation_token = urllib.parse.parse_qs(urllib.parse.urlparse(invitation_link).query)["invite"][0]
            before_invitation_authorizations = len(oidc_provider.authorization_requests)
            invitee = Browser.launch(chrome, 19223, temp / "invitee-profile")
            try:
                invitee.navigate(f"/join?invite={urllib.parse.quote(invitation_token)}")
                invitee.navigate(f"/login?invite={urllib.parse.quote(invitation_token)}")
                google_href = invitee.evaluate(
                    "document.querySelector('a[href^=\"/auth/google/start\"]')?.getAttribute('href')"
                )
                if not google_href:
                    raise AcceptanceError("invitation Google login link is missing")
                oidc_provider.next_login_subjects.append(3)
                invitee.navigate(google_href)
                invitee.wait("document.body.innerText.includes('Game with ')")
                authorization = oidc_provider.authorization_requests[before_invitation_authorizations]
                if invitation_token in json.dumps(authorization):
                    raise AcceptanceError("invitation token was disclosed to the OIDC provider")
                invitation_game = invitee.evaluate(
                    "document.querySelector('#active-games a[href^=\"/games/\"]')?.getAttribute('href')"
                )
                if not invitation_game:
                    raise AcceptanceError("invitation login did not create a private game")
                invitee.navigate(invitation_game)
                invitee.wait("Boolean(document.querySelector('#game-board'))")
            finally:
                invitee.close()
                invitee = None
            assert_responsive_shell(alice, "/", "#dashboard-shell", 390)
            set_viewport(alice, 1440)
            alice.navigate("/")

            # The currently pinned framework predates the readiness event. Keep the
            # assertion explicit so this harness becomes green only after repinning.
            readiness_expression = (
                "window.__acceptanceSubscriptions?.some?.(channel => "
                "channel.startsWith('dashboard:'))"
            )
            if not alice.evaluate(readiness_expression) or not bob.evaluate(readiness_expression):
                raise AcceptanceError(
                    "authenticated dashboard subscription readiness was not emitted by HyperChad"
                )
            alice_handle = alice.evaluate("Array.from(document.body.innerText.matchAll(/@[a-z0-9-]+-[0-9a-f]{8}/g))[0]?.[0]?.slice(1)")
            bob_handle = bob.evaluate("Array.from(document.body.innerText.matchAll(/@[a-z0-9-]+-[0-9a-f]{8}/g))[0]?.[0]?.slice(1)")
            if not alice_handle or not bob_handle or alice_handle == bob_handle:
                raise AcceptanceError("Google accounts did not expose unique stable handles")
            alice.submit('form[hx-post="/dashboard/action"]', {"action": "CHALLENGE", "username": bob_handle})
            alice.wait("document.body.innerText.includes('Challenge sent to Acceptance Player 2')")
            bob.wait("window.__acceptanceUpdates?.some?.(channel => channel.startsWith('dashboard:'))")
            bob.wait("document.body.innerText.includes('Challenge from Restored Provider Name')")
            bob.submit('form:has(input[value="ACCEPT_CHALLENGE"])', {})
            bob.wait(
                "Array.from(document.querySelectorAll('a')).some(link => link.getAttribute('href')?.startsWith('/games/'))"
            )
            game_path = active_game_path(bob)
            alice.wait(
                "Array.from(document.querySelectorAll('a')).some(link => link.getAttribute('href')?.startsWith('/games/'))"
            )
            alice.navigate(game_path)
            bob.navigate(game_path)
            alice.wait(
                "document.querySelector('#app-page')?.getAttribute('v-onevent')?.startsWith('shared-state-event:')"
            )
            bob.wait(
                "document.querySelector('#app-page')?.getAttribute('v-onevent')?.startsWith('shared-state-event:')"
            )
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            bob.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            alice.wait("window.__acceptanceLifecycle?.includes('connected')")
            bob.wait("window.__acceptanceLifecycle?.includes('connected')")
            alice.wait("!document.querySelector('#live-status-connected')?.hidden")
            bob.wait("!document.querySelector('#live-status-connected')?.hidden")
            assert_responsive_game_layout(alice, 1440)
            assert_responsive_game_layout(alice, 390)
            assert_responsive_game_layout(alice, 360)
            assert_game_controls(alice, 1440)
            assert_game_controls(alice, 390)
            set_viewport(alice, 1440)
            alice.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            exercise_rack_and_exchange(alice, 1440, submit_exchange=False)
            exercise_rack_and_exchange(alice, 390, submit_exchange=True)
            bob.wait("document.querySelector('#named-turn-status')?.textContent === 'Your move'")
            set_viewport(alice, 1440)
            alice.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")

            stale_csrf_token = alice.evaluate(
                "document.querySelector('meta[name=\"hyperchad-shared-state-csrf\"]')?.content"
            )
            stale_csrf_cookie = alice.evaluate(
                "document.cookie.split('; ').find(value => value.startsWith('wwmtf-csrf='))?.split('=').slice(1).join('=')"
            )
            if stale_csrf_cookie != stale_csrf_token:
                raise AcceptanceError("initial CSRF cookie did not match rendered metadata")

            server.terminate()
            try:
                server.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.communicate()
            # Shared-state lifecycle transitions are transport timing concerns and are
            # covered by Rust reconnect tests. This browser acceptance flow verifies
            # restored authenticated state and subsequent live updates after restart.
            server = subprocess.Popen(
                command,
                cwd=ROOT,
                env=environment,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
            )
            wait_for_server(server)

            alice.navigate(game_path)
            bob.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            bob.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            refreshed_csrf_token = alice.evaluate(
                "document.querySelector('meta[name=\"hyperchad-shared-state-csrf\"]')?.content"
            )
            refreshed_csrf_cookie = alice.evaluate(
                "document.cookie.split('; ').find(value => value.startsWith('wwmtf-csrf='))?.split('=').slice(1).join('=')"
            )
            if not refreshed_csrf_token:
                raise AcceptanceError("full-page reload did not restore the CSRF token")
            if refreshed_csrf_cookie != refreshed_csrf_token:
                raise AcceptanceError("full-page reload did not synchronize the durable CSRF cookie")

            invalid_actor = (
                alice
                if alice.evaluate(
                    "document.querySelector('#named-turn-status')?.textContent === 'Your move'"
                )
                else bob
            )
            submit_invalid_word(invalid_actor)

            valid_actor = (
                alice
                if alice.evaluate(
                    "document.querySelector('#named-turn-status')?.textContent === 'Your move'"
                )
                else bob
            )
            valid_observer = bob if valid_actor is alice else alice
            play_valid_word(valid_actor, valid_observer)
            valid_actor.wait("Boolean(document.querySelector('.played-word-definition'))")
            definition_path = valid_actor.evaluate("location.pathname")
            valid_actor.evaluate("document.querySelector('.played-word-definition').click()")
            valid_actor.wait(
                "Boolean(document.querySelector('#game-definition-layer.game-definition-panel'))"
            )
            if valid_actor.evaluate("location.pathname") != definition_path:
                raise AcceptanceError("opening a definition navigated away from the game")
            if not valid_actor.evaluate(
                "Boolean(document.querySelector('#game-board') && document.querySelector('#player-rack'))"
            ):
                raise AcceptanceError("opening a definition replaced the game UI")
            valid_actor.evaluate(
                "document.querySelector('#game-definition-layer button').click()"
            )
            valid_actor.wait(
                "getComputedStyle(document.querySelector('#game-definition-layer')).display === 'none'"
            )
            for width in [1440, 390]:
                assert_responsive_game_layout(valid_actor, width)
                assert_responsive_game_layout(valid_observer, width)
            set_viewport(alice, 1440)
            set_viewport(bob, 1440)
            alice.navigate(game_path)
            bob.navigate(game_path)

            for _ in range(12):
                if alice.evaluate("document.body.innerText.includes('Game complete')"):
                    break
                actor = alice if alice.evaluate("document.querySelector('#named-turn-status')?.textContent === 'Your move'") else bob
                observer = bob if actor is alice else alice
                wait_for_turn(actor)
                old_revision = observer.evaluate(
                    "document.querySelector('#game-board')?.getAttribute('data-revision')"
                )
                pass_turn(actor)
                observer.wait(
                    "document.querySelector('#game-board')?.getAttribute('data-revision') !== "
                    + json.dumps(old_revision)
                    + " || document.body.innerText.includes('Game complete')"
                )
            else:
                raise AcceptanceError("game did not complete after repeated legal passes")

            # The earlier server restart already verifies durable authenticated reconnect.
            # Keep the same authenticated browser for final persisted-game assertions.
            bob.navigate(game_path)
            bob.wait("document.body.innerText.includes('Game complete')")
            bob.wait("Boolean(document.querySelector('section[id=\"move-history\"]'))")
            for width in [1440, 390, 360]:
                assert_responsive_game_layout(bob, width)
                if not bob.evaluate("Boolean(document.querySelector('#completed-game-summary'))"):
                    raise AcceptanceError(f"completed summary is missing at {width}px")
            bob.evaluate(
                "Array.from(document.querySelectorAll('#completed-game-summary button')).find(button => button.textContent?.includes('Close'))?.click()"
            )
            bob.wait(
                "getComputedStyle(document.querySelector('#completed-game-summary')).display === 'none'"
            )
            if not bob.evaluate("Boolean(document.querySelector('#game-board'))"):
                raise AcceptanceError("dismissing completed summary hid the board")
            print("two-browser acceptance passed")
        except Exception:
            if server.stdout:
                server.terminate()
                try:
                    output, _ = server.communicate(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    output, _ = server.communicate()
                print(output[-8000:], file=sys.stderr)
            raise
        finally:
            if alice:
                alice.close()
            if bob:
                bob.close()
            if invitee:
                invitee.close()
            if migrant:
                migrant.close()
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)
            oidc_provider.close()


if __name__ == "__main__":
    try:
        run()
    except AcceptanceError as error:
        print(f"browser acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
