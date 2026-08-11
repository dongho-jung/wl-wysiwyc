#!/usr/bin/env python3
"""Exercise navigation through Hyprland's real keyboard shortcut path."""

from __future__ import annotations

import argparse
import json
import math
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time


ROOT = Path(__file__).resolve().parents[2]
BINARY = ROOT / "target/release/wl-wysiwyc"
BUILD = ROOT / "target/nav-probe"
KEYBOARD = BUILD / "virtual-keyboard"
FIXTURES = ("column", "grid", "rows", "sparse", "form", "prose")
TITLES = {"column": "nav: column of links"}
ARROWS = {"left", "right", "up", "down"}

CASES = {
    "tap-right": (
        ("down:right", "wait:100", "up:right"),
        (1.0, 0.0),
    ),
    "tap-down": (
        ("down:down", "wait:100", "up:down"),
        (0.0, 1.0),
    ),
    "diagonal-rd": (
        ("down:right", "down:down", "wait:220", "up:right", "up:down"),
        (math.sqrt(0.5), math.sqrt(0.5)),
    ),
    "diagonal-lu": (
        ("down:left", "down:up", "wait:220", "up:up", "up:left"),
        (-math.sqrt(0.5), -math.sqrt(0.5)),
    ),
    "diagonal-ru": (
        ("down:right", "down:up", "wait:220", "up:right", "up:up"),
        (math.sqrt(0.5), -math.sqrt(0.5)),
    ),
    "diagonal-ld": (
        ("down:left", "down:down", "wait:220", "up:down", "up:left"),
        (-math.sqrt(0.5), math.sqrt(0.5)),
    ),
    "hold-right": (
        ("down:right", "wait:900", "up:right"),
        (1.0, 0.0),
    ),
    "hold-diagonal": (
        ("down:right", "down:down", "wait:900", "up:down", "up:right"),
        (math.sqrt(0.5), math.sqrt(0.5)),
    ),
    "release-right-first": (
        (
            "down:right",
            "down:down",
            "wait:220",
            "up:right",
            "wait:180",
            "up:down",
        ),
        None,
    ),
    "release-down-first": (
        (
            "down:right",
            "down:down",
            "wait:220",
            "up:down",
            "wait:180",
            "up:right",
        ),
        None,
    ),
    "opposites": (
        (
            "down:left",
            "down:right",
            "wait:180",
            "up:left",
            "wait:180",
            "up:right",
        ),
        None,
    ),
    "opposites-vertical": (
        (
            "down:up",
            "down:down",
            "wait:180",
            "up:up",
            "wait:180",
            "up:down",
        ),
        None,
    ),
    "rolling-turn": (
        (
            "down:right",
            "wait:120",
            "down:down",
            "wait:180",
            "up:right",
            "wait:160",
            "up:down",
        ),
        None,
    ),
    "rapid-taps": (
        (
            "down:right",
            "wait:45",
            "up:right",
            "wait:35",
            "down:down",
            "wait:45",
            "up:down",
            "wait:35",
            "down:left",
            "wait:45",
            "up:left",
            "wait:35",
            "down:up",
            "wait:45",
            "up:up",
        ),
        None,
    ),
}

KEY_RE = re.compile(r"^NAV key t=(\d+) edge=([^ ]+) key=([^ ]+)$")
FRAME_RE = re.compile(
    r"^NAV frame t=(\d+) gap=([\d.]+) "
    r"input=\(([-\d.]+),([-\d.]+)\) "
    r"at=\(([-\d.]+),([-\d.]+)\) "
    r"velocity=\(([-\d.]+),([-\d.]+)\) speed=([-\d.]+) "
    r"state=(\w+) anchor=(none|\([^)]+\)) "
    r"distance=([^ ]+) done=(true|false)$"
)
ELEMENT_RE = re.compile(
    r" at \(([-\d.]+), ([-\d.]+)\) size ([-\d.]+)x([-\d.]+)$"
)


def command(args: list[str], **kwargs: object) -> subprocess.CompletedProcess[str]:
    return subprocess.run(args, check=True, text=True, **kwargs)


def build() -> None:
    command(["cargo", "build", "--release"], cwd=ROOT)
    BUILD.mkdir(parents=True, exist_ok=True)
    xml = ROOT / "tests/nav/virtual-keyboard-unstable-v1.xml"
    header = BUILD / "virtual-keyboard-protocol.h"
    code = BUILD / "virtual-keyboard-protocol.c"
    command(["wayland-scanner", "client-header", str(xml), str(header)])
    command(["wayland-scanner", "private-code", str(xml), str(code)])
    flags = command(
        ["pkg-config", "--cflags", "--libs", "wayland-client", "xkbcommon"],
        capture_output=True,
    ).stdout.split()
    command(
        [
            "cc",
            "-std=c11",
            "-O2",
            "-Wall",
            "-Wextra",
            "-Werror",
            f"-I{BUILD}",
            str(ROOT / "tests/nav/virtual-keyboard.c"),
            str(code),
            "-o",
            str(KEYBOARD),
            *flags,
        ]
    )


def open_fixture(name: str) -> dict[str, object]:
    title = TITLES.get(name, f"nav: {name}")
    expected = f"{title} - Chromium"
    active = json.loads(
        command(["hyprctl", "activewindow", "-j"], capture_output=True).stdout
    )
    if active.get("title") == expected:
        return active
    clients = json.loads(
        command(["hyprctl", "clients", "-j"], capture_output=True).stdout
    )
    existing = next(
        (client for client in clients if client.get("title") == expected), None
    )
    if existing is None:
        url = (ROOT / f"tests/nav/{name}.html").as_uri()
        subprocess.Popen(
            ["chromium", url],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
    deadline = time.monotonic() + 5.0
    stable_since: float | None = None
    focused: str | None = None
    while time.monotonic() < deadline:
        clients = json.loads(
            command(["hyprctl", "clients", "-j"], capture_output=True).stdout
        )
        existing = next(
            (client for client in clients if client.get("title") == expected), None
        )
        if existing is not None and existing["address"] != focused:
            focused = str(existing["address"])
            expression = (
                "hl.dispatch(hl.dsp.focus({ window = "
                f'"address:{focused}" }}))'
            )
            command(["hyprctl", "eval", expression], stdout=subprocess.DEVNULL)
        active = json.loads(
            command(["hyprctl", "activewindow", "-j"], capture_output=True).stdout
        )
        if active.get("title") == expected:
            stable_since = stable_since or time.monotonic()
            if time.monotonic() - stable_since >= 0.35:
                return active
        else:
            stable_since = None
        time.sleep(0.05)
    raise RuntimeError(f"Chromium did not focus the {name} fixture")


def start_anchor(window: dict[str, object]) -> tuple[float, float]:
    output = command([str(BINARY), "--elements"], capture_output=True).stdout
    width, height = (float(value) for value in window["size"])
    candidates: list[tuple[float, float]] = []
    for line in output.splitlines()[1:]:
        found = ELEMENT_RE.search(line)
        if not found:
            continue
        x, y, w, h = (float(value) for value in found.groups())
        center = (x + w / 2.0, y + h / 2.0)
        if y >= 150.0 and 0.0 <= center[0] < width and 0.0 <= center[1] < height:
            candidates.append(center)
    if not candidates:
        raise RuntimeError("fixture has no visible page anchors")
    # Pick the middle of the fixture's anchors, not the middle of the browser
    # window. Several fixtures occupy only its left half, which used to put
    # every rightward case on the last column and left no forward target to
    # exercise attraction against.
    wanted = (
        (min(point[0] for point in candidates) + max(point[0] for point in candidates))
        / 2.0,
        (min(point[1] for point in candidates) + max(point[1] for point in candidates))
        / 2.0,
    )
    local = min(candidates, key=lambda at: math.dist(at, wanted))
    wx, wy = (float(value) for value in window["at"])
    return (wx + local[0], wy + local[1])


def inject(actions: tuple[str, ...] | list[str]) -> None:
    command([str(KEYBOARD), *actions])


def parse(lines: list[str]) -> list[dict[str, object]]:
    records: list[dict[str, object]] = []
    for line in lines:
        found = KEY_RE.match(line)
        if found:
            records.append(
                {
                    "kind": "key",
                    "t": int(found.group(1)),
                    "edge": found.group(2),
                    "key": found.group(3),
                }
            )
            continue
        found = FRAME_RE.match(line)
        if found:
            values = found.groups()
            records.append(
                {
                    "kind": "frame",
                    "t": int(values[0]),
                    "gap": float(values[1]),
                    "input": (float(values[2]), float(values[3])),
                    "at": (float(values[4]), float(values[5])),
                    "velocity": (float(values[6]), float(values[7])),
                    "speed": float(values[8]),
                    "state": values[9],
                    "distance": float(values[11]),
                    "done": values[12] == "true",
                }
            )
    return records


def direction(held: set[str]) -> tuple[float, float]:
    x = float("right" in held) - float("left" in held)
    y = float("down" in held) - float("up" in held)
    length = math.hypot(x, y)
    return (x / length, y / length) if length else (0.0, 0.0)


def close(a: tuple[float, float], b: tuple[float, float]) -> bool:
    return math.dist(a, b) <= 0.012


def analyze(
    actions: tuple[str, ...],
    intended: tuple[float, float] | None,
    records: list[dict[str, object]],
) -> tuple[list[str], dict[str, float]]:
    failures: list[str] = []
    expected_ups = [action[3:] for action in actions if action.startswith("up:")]
    observed_ups = [
        record["key"]
        for record in records
        if record["kind"] == "key"
        and record["edge"] in {"up", "marker-up"}
        and record["key"] in ARROWS
    ]
    for key in expected_ups:
        if key not in observed_ups:
            failures.append(f"missing {key} release")

    held: set[str] = set()
    started = False
    frames: list[dict[str, object]] = []
    mismatches = 0
    for record in records:
        if record["kind"] == "key" and record["key"] in ARROWS:
            started = True
            key = str(record["key"])
            if record["edge"] == "down":
                held.add(key)
            elif record["edge"] in {"up", "marker-up"}:
                held.discard(key)
        elif record["kind"] == "frame" and started:
            frames.append(record)
            if not close(record["input"], direction(held)):
                mismatches += 1
    if mismatches:
        failures.append(f"{mismatches} frames disagree with held keys")
    if not frames:
        return failures + ["no motion frames"], {}

    final = next((frame for frame in reversed(frames) if frame["done"]), None)
    if final is None:
        failures.append("motion did not settle")
        final = frames[-1]
    moving = frames[: frames.index(final) + 1]
    start = frames[0]["at"]
    if final["distance"] > 0.05:
        failures.append(f"stopped {final['distance']:.1f}px from the nearest anchor")

    max_gap = max(float(frame["gap"]) for frame in moving)
    short_ratio = sum(float(frame["gap"]) < 4.0 for frame in moving) / len(moving)
    if max_gap > 24.0:
        failures.append(f"motion frame gap reached {max_gap:.1f}ms")
    if short_ratio > 0.16:
        failures.append(f"{short_ratio:.0%} of motion frames were burst updates")

    end = final["at"]
    travel = math.dist(start, end)
    if intended is not None:
        progress = (end[0] - start[0]) * intended[0] + (end[1] - start[1]) * intended[1]
        release_frame = next(
            (
                frame
                for frame in frames
                if not close(frame["input"], intended)
                and frame["t"] > frames[0]["t"] + 20
            ),
            final,
        )
        released_progress = (
            (release_frame["at"][0] - start[0]) * intended[0]
            + (release_frame["at"][1] - start[1]) * intended[1]
        )
        if (
            released_progress > 8.0
            and travel > 8.0
            and progress < released_progress * 0.45
        ):
            failures.append(
                f"capture erased pushed progress ({released_progress:.1f}px to {progress:.1f}px)"
            )

    return failures, {
        "frames": float(len(moving)),
        "max_gap": max_gap,
        "burst_ratio": short_ratio,
        "travel": travel,
        "final_distance": float(final["distance"]),
        "attract_frames": float(sum(frame["state"] == "attract" for frame in moving)),
    }


def run_case(
    fixture: str,
    name: str,
    actions: tuple[str, ...],
    intended: tuple[float, float] | None,
    start: tuple[float, float],
    settle_ms: int,
) -> tuple[list[str], dict[str, float]]:
    command([str(BINARY), "--move-test", str(start[0]), str(start[1])])
    env = os.environ.copy()
    env.update({"WL_KEYS": "1", "WL_TRACE": "1"})
    process = subprocess.Popen(
        [str(BINARY)],
        cwd=ROOT,
        env=env,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
        text=True,
        bufsize=1,
    )
    lines: list[str] = []

    def read() -> None:
        assert process.stderr is not None
        lines.extend(line.rstrip("\n") for line in process.stderr)

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    deadline = time.monotonic() + 4.0
    while time.monotonic() < deadline and not any(
        line.startswith("NAV frame") and line.endswith("done=true") for line in lines
    ):
        if process.poll() is not None:
            break
        time.sleep(0.01)
    if not any(
        line.startswith("NAV frame") and line.endswith("done=true") for line in lines
    ):
        process.kill()
        reader.join(timeout=1.0)
        return ["overlay did not become ready"], {}

    time.sleep(0.25)
    inject(actions)
    time.sleep(settle_ms / 1000.0)
    inject(("down:escape", "up:escape"))
    try:
        process.wait(timeout=3.0)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait()
        lines.append("measure: overlay did not exit")
    reader.join(timeout=1.0)
    log_dir = ROOT / "target/nav-measure"
    log_dir.mkdir(parents=True, exist_ok=True)
    (log_dir / f"{fixture}-{name}.log").write_text("\n".join(lines) + "\n")
    return analyze(actions, intended, parse(lines))


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fixture", action="append", choices=FIXTURES)
    parser.add_argument("--case", action="append", choices=tuple(CASES))
    parser.add_argument("--no-build", action="store_true")
    parser.add_argument("--settle-ms", type=int, default=2500)
    args = parser.parse_args()
    if not args.no_build:
        build()
    fixtures = args.fixture or list(FIXTURES)
    cases = args.case or list(CASES)
    failed = 0
    total = 0
    for fixture in fixtures:
        window = open_fixture(fixture)
        start = start_anchor(window)
        print(f"{fixture}: start=({start[0]:.0f},{start[1]:.0f})", flush=True)
        for case in cases:
            total += 1
            actions, intended = CASES[case]
            failures, metrics = run_case(
                fixture, case, actions, intended, start, args.settle_ms
            )
            if failures:
                failed += 1
                status = "FAIL: " + "; ".join(failures)
            else:
                status = "PASS"
            details = " ".join(f"{key}={value:.2f}" for key, value in metrics.items())
            print(f"  {case:21} {status} {details}", flush=True)
    print(f"result: {total - failed}/{total} passed", flush=True)
    return int(failed != 0)


if __name__ == "__main__":
    sys.exit(main())
