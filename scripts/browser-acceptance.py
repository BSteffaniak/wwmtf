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


BUNDLED_WORDS = frozenset(
    (ROOT / "packages/game_domain/data/enable1.txt").read_text().splitlines()
)


def bundled_dictionary_contains(word: str) -> bool:
    return word.lower() in BUNDLED_WORDS


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


def assert_responsive_game_layout(browser: Browser, width: int) -> None:
    set_viewport(browser, width)
    browser.navigate(browser.evaluate("location.pathname"))
    browser.wait("Boolean(document.querySelector('#game-board'))")
    layout = browser.evaluate(
        """(() => {
            const board = document.querySelector('#game-board');
            const scroller = board?.querySelector(':scope > div');
            const rack = document.querySelector('#player-rack');
            const boardRect = board?.getBoundingClientRect();
            const scrollerRect = scroller?.getBoundingClientRect();
            const rackRect = rack?.getBoundingClientRect();
            const ids = Array.from(document.querySelectorAll('[id]')).map(element => element.id);
            return {
                viewport: document.querySelector('meta[name="viewport"]')?.content ?? null,
                documentOverflow: document.documentElement.scrollWidth - document.documentElement.clientWidth,
                boardOverflow: scroller ? scroller.scrollWidth - scroller.clientWidth : null,
                boardRight: scrollerRect?.right ?? null,
                rackBelowBoard: Boolean(boardRect && rackRect && rackRect.top >= boardRect.bottom),
                hasRack: Boolean(rack),
                hasActions: Boolean(document.querySelector('#turn-actions')) || document.body.innerText.includes('Waiting for opponent') || document.body.innerText.includes('Game complete'),
                hasPreview: Boolean(document.querySelector('#draft-preview')) || document.body.innerText.includes('Waiting for opponent') || document.body.innerText.includes('Game complete'),
                hasAwareness: Boolean(document.querySelector('#game-awareness')),
                duplicateIds: ids.filter((id, index) => ids.indexOf(id) !== index),
            };
        })()"""
    )
    if layout["viewport"] != "width=device-width, initial-scale=1":
        raise AcceptanceError(f"responsive viewport metadata is missing: {layout!r}")
    if layout["documentOverflow"] != 0:
        raise AcceptanceError(f"game page overflows the viewport at {width}px: {layout!r}")
    if not layout["rackBelowBoard"]:
        raise AcceptanceError(f"rack is not below the board at {width}px: {layout!r}")
    if not all(layout[key] for key in ["hasRack", "hasActions", "hasPreview", "hasAwareness"]):
        raise AcceptanceError(f"game interaction state is missing at {width}px: {layout!r}")
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


def register(browser: Browser, username: str) -> None:
    browser.navigate("/register")
    browser.submit('form[hx-post="/register"]', {"username": username, "password": "correct horse battery staple"})
    browser.wait("document.body.innerText.includes('Signed in as ')")


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
    browser.wait("document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'")


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
    if "points" not in preview or not any(tile["blank"] or tile["id"] for tile in selected):
        raise AcceptanceError(f"server-derived play preview is missing: {preview!r}")

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
    browser.wait("document.body.innerText.includes('Choose the exact position')")
    browser.submit('form:has(input[value="MOVE_RACK_TILE"]):has(input[name="slot"][value="6"])')
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
            assert_responsive_shell(alice, "/login", "main", 390)
            set_viewport(alice, 1440)
            alice.navigate("/register")
            bob.navigate("/register")
            install_readiness_probe(alice)
            install_readiness_probe(bob)
            register(alice, "acceptance-alice")
            register(bob, "acceptance-bob")
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
            alice.submit('form[hx-post="/dashboard/action"]', {"action": "CHALLENGE", "username": "acceptance-bob"})
            alice.wait("document.body.innerText.includes('Challenge sent to acceptance-bob')")
            bob.wait("window.__acceptanceUpdates?.some?.(channel => channel.startsWith('dashboard:'))")
            bob.wait("document.body.innerText.includes('Challenge from acceptance-alice')")
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
                "document.querySelector('#app-page')?.getAttribute('v-onevent')?.startsWith('shared-state-update:')"
            )
            bob.wait(
                "document.querySelector('#app-page')?.getAttribute('v-onevent')?.startsWith('shared-state-update:')"
            )
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            bob.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            alice.wait("window.__acceptanceLifecycle?.includes('connected')")
            bob.wait("window.__acceptanceLifecycle?.includes('connected')")
            alice.wait("!document.querySelector('#live-status-connected')?.hidden")
            bob.wait("!document.querySelector('#live-status-connected')?.hidden")
            assert_responsive_game_layout(alice, 1440)
            assert_responsive_game_layout(alice, 390)
            set_viewport(alice, 1440)
            alice.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")
            exercise_rack_and_exchange(alice, 1440, submit_exchange=False)
            exercise_rack_and_exchange(alice, 390, submit_exchange=True)
            bob.wait("document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'")
            set_viewport(alice, 1440)
            alice.navigate(game_path)
            alice.wait("window.__acceptanceSubscriptions?.some?.(channel => channel.startsWith('game:'))")

            stale_csrf_token = alice.evaluate(
                "document.querySelector('meta[name=\"hyperchad-shared-state-csrf\"]')?.content"
            )
            stale_csrf_cookie = alice.evaluate(
                "document.cookie.split('; ').find(value => value.startsWith('words-with-spouses-csrf='))?.split('=').slice(1).join('=')"
            )
            if stale_csrf_cookie != stale_csrf_token:
                raise AcceptanceError("initial CSRF cookie did not match rendered metadata")

            server.terminate()
            try:
                server.communicate(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()
                server.communicate()
            alice.wait("window.__acceptanceLifecycle?.includes('disconnected') || window.__acceptanceLifecycle?.includes('reconnecting')")
            alice.wait("!document.querySelector('#live-status-disconnected')?.hidden || !document.querySelector('#live-status-reconnecting')?.hidden")
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
                "document.cookie.split('; ').find(value => value.startsWith('words-with-spouses-csrf='))?.split('=').slice(1).join('=')"
            )
            if refreshed_csrf_token == stale_csrf_token:
                raise AcceptanceError("server restart did not rotate the CSRF token")
            if refreshed_csrf_cookie != refreshed_csrf_token:
                raise AcceptanceError("full-page reload did not synchronize the rotated CSRF cookie")

            invalid_actor = (
                alice
                if alice.evaluate(
                    "document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'"
                )
                else bob
            )
            submit_invalid_word(invalid_actor)

            valid_actor = (
                alice
                if alice.evaluate(
                    "document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'"
                )
                else bob
            )
            valid_observer = bob if valid_actor is alice else alice
            play_valid_word(valid_actor, valid_observer)
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
                actor = alice if alice.evaluate("document.querySelector('#viewer-turn-status')?.textContent === 'Your turn'") else bob
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

            bob.close()
            bob = Browser.launch(chrome, 19222, temp / "bob-reconnected-profile")
            bob.navigate("/login")
            bob.submit('form[hx-post="/login"]', {"username": "acceptance-bob", "password": "correct horse battery staple"})
            bob.wait("document.body.innerText.includes('Signed in as ')")
            bob.navigate(game_path)
            bob.wait("document.body.innerText.includes('Game complete')")
            bob.wait("Boolean(document.querySelector('section[id=\"move-history\"]'))")
            for width in [1440, 390]:
                assert_responsive_game_layout(bob, width)
                if not bob.evaluate("Boolean(document.querySelector('#completed-game-summary'))"):
                    raise AcceptanceError(f"completed summary is missing at {width}px")
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
