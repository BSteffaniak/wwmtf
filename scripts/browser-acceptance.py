#!/usr/bin/env python3
"""Run the two-browser product acceptance flow against a local application server.

The harness uses Chrome's DevTools protocol directly so the repository does not
own application JavaScript or require a browser-automation package at runtime.
"""

from __future__ import annotations

import base64
import json
import os
import pathlib
import shutil
import socket
import struct
import subprocess
import sys
import tempfile
import time
import urllib.request
from dataclasses import dataclass
from typing import Any

ROOT = pathlib.Path(__file__).resolve().parents[1]
HOST = "127.0.0.1"
PORT = 18343
BASE_URL = f"http://{HOST}:{PORT}"
TIMEOUT = 20.0


class AcceptanceError(RuntimeError):
    pass


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
            last = self.evaluate(expression)
            if last:
                return last
            time.sleep(0.1)
        context = self.evaluate("({ url: location.href, text: document.body.innerText.slice(0, 2000), html: document.body.innerHTML.slice(0, 2000) })")
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


def register(browser: Browser, username: str) -> None:
    browser.navigate("/register")
    browser.submit('form[hx-post="/register"]', {"username": username, "password": "correct horse battery staple"})
    browser.wait("document.body.innerText.includes('Signed in as ')")


def install_readiness_probe(browser: Browser) -> None:
    source = """(() => {
        window.__acceptanceSubscriptions = [];
        window.addEventListener('v-shared-state-subscribed', event => {
            window.__acceptanceSubscriptions.push(JSON.parse(event.detail).channel_id);
        });
    })();"""
    browser.tools.call("Page.addScriptToEvaluateOnNewDocument", {"source": source})
    browser.evaluate(source)


def active_game_path(browser: Browser) -> str:
    return browser.wait(
        "Array.from(document.querySelectorAll('a')).map(link => link.getAttribute('href')).find(href => href?.startsWith('/games/')) || ''"
    )


def wait_for_turn(browser: Browser) -> None:
    browser.wait("document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'")


def pass_turn(browser: Browser) -> None:
    revision = browser.evaluate("document.querySelector('[name=expected_revision]')?.value")
    path = browser.evaluate("location.pathname")
    browser.submit('form[hx-post$="/turn"]', {"command": "PASS"})
    time.sleep(0.2)
    if browser.evaluate("document.body.innerText.includes('updated game could not be rendered')"):
        browser.navigate(path)
    browser.wait(
        f"Number(document.querySelector('[name=expected_revision]')?.value ?? {revision}) > Number({json.dumps(revision)})"
        " || document.body.innerText.includes('Game complete')"
    )


def run() -> None:
    chrome = chrome_binary()
    with tempfile.TemporaryDirectory(prefix="words-with-spouses-browser-") as temporary:
        temp = pathlib.Path(temporary)
        database = temp / "acceptance.db"
        environment = os.environ.copy()
        environment.update(
            {
                "WORDS_WITH_SPOUSES_BIND_ADDRESS": HOST,
                "WORDS_WITH_SPOUSES_PORT": str(PORT),
                "WORDS_WITH_SPOUSES_DATABASE_PATH": str(database),
            }
        )
        application_command = os.environ.get("WORDS_WITH_SPOUSES_ACCEPTANCE_SERVER")
        if application_command:
            command = application_command.split()
        else:
            command = [
                "cargo",
                "run",
                "-p",
                "words_with_spouses_app",
                "--bin",
                "words-with-spouses",
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
        try:
            wait_for_server(server)
            alice = Browser.launch(chrome, 19221, temp / "alice-profile")
            bob = Browser.launch(chrome, 19222, temp / "bob-profile")
            alice.navigate("/register")
            bob.navigate("/register")
            install_readiness_probe(alice)
            install_readiness_probe(bob)
            register(alice, "acceptance-alice")
            register(bob, "acceptance-bob")

            # The currently pinned framework predates the readiness event. Keep the
            # assertion explicit so this harness becomes green only after repinning.
            readiness_expression = (
                "window.__acceptanceSubscriptions?.some?.(channel => "
                "channel.startsWith('dashboard:'))"
            )
            if not alice.evaluate(readiness_expression) or not bob.evaluate(readiness_expression):
                raise AcceptanceError(
                    "authenticated dashboard subscription readiness was not emitted; "
                    "upstream and pin the adjacent HyperChad change"
                )

            alice.submit('form[hx-post="/dashboard/action"]', {"action": "CHALLENGE", "username": "acceptance-bob"})
            alice.wait("document.body.innerText.includes('CHALLENGE OUTGOING')")
            if not bob.evaluate("document.body.innerText.includes('CHALLENGE INCOMING')"):
                bob.navigate("/")
            bob.wait("document.body.innerText.includes('CHALLENGE INCOMING')")
            bob.submit('form:has(input[value="ACCEPT_CHALLENGE"])', {})
            bob.wait(
                "Array.from(document.querySelectorAll('a')).some(link => link.getAttribute('href')?.startsWith('/games/'))"
            )
            game_path = active_game_path(bob)
            if not alice.evaluate(
                "Array.from(document.querySelectorAll('a')).some(link => link.getAttribute('href')?.startsWith('/games/'))"
            ):
                alice.navigate("/")
            alice.wait(
                "Array.from(document.querySelectorAll('a')).some(link => link.getAttribute('href')?.startsWith('/games/'))"
            )
            alice.navigate(game_path)
            bob.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            bob.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")

            for _ in range(12):
                if alice.evaluate("document.body.innerText.includes('Game complete')"):
                    break
                actor = alice if alice.evaluate("document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'") else bob
                observer = bob if actor is alice else alice
                wait_for_turn(actor)
                old_revision = observer.evaluate(
                    "document.querySelector('#game-board')?.getAttribute('data-revision')"
                )
                pass_turn(actor)
                if not observer.evaluate(
                    "document.querySelector('#game-board')?.getAttribute('data-revision') !== "
                    + json.dumps(old_revision)
                    + " || document.body.innerText.includes('Game complete')"
                ):
                    observer.navigate(game_path)
                observer.wait(
                    "document.querySelector('#game-board')?.getAttribute('data-revision') !== "
                    + json.dumps(old_revision)
                    + " || document.body.innerText.includes('Game complete')"
                )
            else:
                raise AcceptanceError("game did not complete after repeated legal passes")

            bob.close()
            bob = Browser.launch(chrome, 19222, temp / "bob-reconnected-profile")
            bob.navigate("/login")
            bob.submit('form[hx-post="/login"]', {"username": "acceptance-bob", "password": "correct horse battery staple"})
            bob.wait("document.body.innerText.includes('Signed in as ')")
            bob.navigate(game_path)
            bob.wait("document.body.innerText.includes('Game complete')")
            bob.wait("Boolean(document.querySelector('section[id=\"move-history\"]'))")
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
            if server.poll() is None:
                server.terminate()
                try:
                    server.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    server.kill()
                    server.wait(timeout=5)


if __name__ == "__main__":
    try:
        run()
    except AcceptanceError as error:
        print(f"browser acceptance failed: {error}", file=sys.stderr)
        raise SystemExit(1)
