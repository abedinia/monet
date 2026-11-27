#!/usr/bin/env python3
"""Exercise the release binary through a real terminal device."""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import socket
import struct
import subprocess
import termios
import threading
import time

ROOT = Path(__file__).resolve().parents[1]
BINARY = ROOT / "monet"
END_FRAME = b"\x1b[J"
HOME_CURSOR = b"\x1b[H"
CSI = re.compile(rb"\x1b\[[0-9;?]*[A-Za-z]")

if not BINARY.is_file():
    raise SystemExit("Build the executable first with ./scripts/build.sh")

master, slave = pty.openpty()


def resize(columns, rows):
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack("HHHH", rows, columns, 0, 0))


def read_until(marker, timeout=5):
    deadline = time.monotonic() + timeout
    data = bytearray()
    while marker not in data:
        ready, _, _ = select.select([master], [], [], max(0, deadline - time.monotonic()))
        assert ready, (marker, CSI.sub(b"", data[-1000:]))
        data.extend(os.read(master, 65536))
    return bytes(data)


def frame_containing(text):
    # Match on visible text: styling may change between any two cells.
    marker = text.encode()
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        frame = read_until(END_FRAME, deadline - time.monotonic())
        for part in frame.split(END_FRAME)[:-1]:
            if marker in CSI.sub(b"", part):
                return part
    raise AssertionError(f"No dashboard frame contained {text!r}")


resize(80, 24)
initial_mode = termios.tcgetattr(slave)
env = dict(os.environ, TERM="xterm-256color")
env.pop("NO_COLOR", None)
env.pop("COLORTERM", None)
process = subprocess.Popen(
    # Motion is checked separately; without it frames follow input.
    [str(BINARY), "--interval", "250", "--no-motion"],
    stdin=slave, stdout=slave, stderr=slave, env=env,
)
try:
    first = frame_containing("Connections")
    assert b"\x1b[?1049h" in first
    assert "◆ MONET".encode() in CSI.sub(b"", first)
    # 256-color output: a dark canvas and a spread of gradient inks.
    assert b";48;5;" in first
    inks = set(re.findall(rb";38;5;(\d+);", first))
    assert len(inks) >= 8, inks
    assert "↓ DOWNLOAD".encode() in first and "↑ UPLOAD".encode() in first
    assert "Traffic · all links".encode() in first
    assert "Top flows".encode() in first

    for key, caption in [(b"2", "mtu"), (b"3", "Socket table"), (b"4", "IPv4 routes")]:
        os.write(master, key)
        tab = frame_containing(caption)
        if key == b"3":
            assert b"established" in tab and b"listening" in tab

    os.write(master, b"5")
    frame_containing("Flows · TCP")
    for key, caption in [(b"d", "↓ download"), (b"g", "remote IPs"), (b"u", "↑ upload"), (b"b", "● SESSION")]:
        os.write(master, key)
        frame_containing(caption)

    for key, caption in [(b"6", "Bandwidth"), (b"7", "Mean TCP round-trip"), (b"8", "Event log")]:
        os.write(master, key)
        frame_containing(caption)
    os.write(master, b"1")
    frame_containing("Traffic")
    os.write(master, b"c")
    frame_containing("Traffic")

    os.write(master, b"?")
    frame_containing("close this panel")
    os.write(master, b"x")
    deadline = time.monotonic() + 5
    while b"close this panel" in read_until(END_FRAME, deadline - time.monotonic()).split(END_FRAME)[-2]:
        pass

    os.write(master, b"4")
    frame_containing("IPv4 routes")
    os.write(master, b"\x1b[B")
    frame_containing("↕ 2–")
    os.write(master, b"p")
    frame_containing("PAUSED")
    os.write(master, b"[")
    frame_containing("Traffic")
    os.write(master, b"p")
    frame_containing("LIVE")

    resize(26, 8)
    frame_containing("resize terminal")
    resize(64, 18)
    frame = frame_containing("quit")
    clean = CSI.sub(b"", frame.split(HOME_CURSOR)[-1]).decode()
    lines = clean.replace("\r\r\n", "\n").splitlines()
    assert len(lines) <= 18
    assert "Traffic" in clean and "Sockets" in clean

    resize(36, 12)
    frame = frame_containing("↓ down")
    clean = CSI.sub(b"", frame.split(HOME_CURSOR)[-1]).decode()
    assert len(clean.replace("\r\r\n", "\n").splitlines()) <= 12

    os.write(master, b"m")
    first_motion = read_until(END_FRAME)
    assert read_until(END_FRAME) != first_motion, "motion should animate"

    os.write(master, b"q")
    read_until(b"\x1b[?1049l")
    assert process.wait(timeout=3) == 0
    assert termios.tcgetattr(slave) == initial_mode
finally:
    if process.poll() is None:
        os.write(master, b"\x03")
        try:
            process.wait(timeout=3)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
    os.close(master)
    os.close(slave)

report = subprocess.check_output([str(BINARY), "--interval", "250"], input=b"")
assert b"\x1b" not in report and b"Monet" in report
snapshot = json.loads(subprocess.check_output([str(BINARY), "--once", "--interval", "250"]))
assert snapshot["interval_ms"] == 250
assert set(snapshot["tcp_traffic"]) == {"error", "flows"}
assert subprocess.run([str(BINARY), "--port", "7878"], capture_output=True).returncode != 0
print("PASS: terminal panels, views, motion, color, hotkeys, help, resize, cleanup and JSON")


def check_live_tcp():
    # Locked-down environments can deny both test traffic and SOCK_DIAG.
    if snapshot["tcp_traffic"]["error"]:
        print("SKIP: live TCP transfer test: " + snapshot["tcp_traffic"]["error"])
        return

    with socket.socket() as listener, socket.socket() as client:
        listener.bind(("127.0.0.1", 0))
        listener.listen()
        client.connect(listener.getsockname())
        peer, _ = listener.accept()
        with peer:
            client.settimeout(3)
            peer.settimeout(3)
            local = "%s:%s" % client.getsockname()
            remote = "%s:%s" % client.getpeername()
            done = threading.Event()
            errors = []

            def transfer():
                blocks = [(client, peer, b"u" * 8192), (peer, client, b"d" * 16384)]
                try:
                    while not done.wait(0.02):
                        for sender, receiver, payload in blocks:
                            sender.sendall(payload)
                            remaining = len(payload)
                            while remaining:
                                data = receiver.recv(remaining)
                                if not data:
                                    raise RuntimeError("TCP test peer closed early")
                                remaining -= len(data)
                except Exception as error:
                    errors.append(error)

            worker = threading.Thread(target=transfer)
            worker.start()
            try:
                probe = json.loads(subprocess.check_output(
                    [str(BINARY), "--once", "--interval", "800"], timeout=6,
                ))
            finally:
                done.set()
                worker.join()
            assert not errors, errors
            traffic = probe["tcp_traffic"]
            assert traffic["error"] is None, traffic["error"]
            flow = next(flow for flow in traffic["flows"] if flow["local"] == local and flow["remote"] == remote)
            assert flow["download_session_bytes"] > flow["upload_session_bytes"] > 0, flow
            assert flow["download_bytes_per_second"] > flow["upload_bytes_per_second"] > 0, flow
            print("PASS: Linux TCP download and acknowledged upload on a real connection")


check_live_tcp()
