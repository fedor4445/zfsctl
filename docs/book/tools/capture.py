#!/usr/bin/env python3
"""Captures real zfsctl screens (or command output) for The zfsctl Book.

TUI capture:   capture.py --name 08-pools --section Pools --cols 110 --rows 30 --wait 10 [--keys '<Down>'] [--crop 0:20]
Command capture: capture.py --name 05-check-host --cmd 'sudo ./zfsctl/target/release/zfsctl check-host fbsd16'

Environment: HUB (default fb@172.16.10.90) and ZFSCTL_BIN (default ./zfsctl/target/release/zfsctl).
Needs: pip install pexpect pyte. Captures are written to docs/book/captures/NAME.txt and are never edited by hand.
"""
import argparse, os, re, subprocess, sys, time
from pathlib import Path

SECTIONS = ["Hosts", "Pools", "Datasets", "Snapshots", "Disks", "Shares", "Boot Envs", "Jobs", "Alerts"]
KEYS = {"<Tab>": "\t", "<S-Tab>": "\x1b[Z", "<Enter>": "\r", "<Esc>": "\x1b", "<Down>": "\x1b[B", "<Up>": "\x1b[A",
        "<Left>": "\x1b[D", "<Right>": "\x1b[C", "<Space>": " ", "<Backspace>": "\x7f"}
# An address, but not user@192.0.2.1 (an ssh target with a numeric host is not an email address).
EMAIL = re.compile(r"[\w.+-]+@(?!\d+\.\d+\.\d+\.\d+)[\w-]+(\.[\w-]+)+")


def expand(keys):
    out, i = [], 0
    while i < len(keys):
        for token, seq in KEYS.items():
            if keys.startswith(token, i):
                out.append(seq)
                i += len(token)
                break
        else:
            out.append(keys[i])
            i += 1
    return out


def clean(lines):
    """Trailing spaces off; emails replaced; lines that mention a password dropped (except the backup form's password-file label, which holds only a path)."""
    out = []
    for line in lines:
        if "password" in line.lower() and "password file (absolute" not in line.lower():
            continue
        out.append(EMAIL.sub("you@example.com", line.rstrip()))
    while out and not out[-1]:
        out.pop()
    return out


def tui(args):
    import pexpect, pyte
    hub, binary = os.environ.get("HUB", "fb@172.16.10.90"), os.environ.get("ZFSCTL_BIN", "./zfsctl/target/release/zfsctl")
    screen = pyte.Screen(args.cols, args.rows)
    stream = pyte.Stream(screen)
    child = pexpect.spawn(f"ssh -tt -o BatchMode=yes {hub} 'sudo {binary}'", dimensions=(args.rows, args.cols), encoding=None)

    def pump(seconds):
        end = time.time() + seconds
        while time.time() < end:
            try:
                stream.feed(child.read_nonblocking(65536, 0.3).decode("utf8", "replace"))
            except pexpect.TIMEOUT:
                pass
            except pexpect.EOF:
                return

    pump(args.wait)
    if args.section:
        for _ in range(SECTIONS.index(args.section)):
            child.send("\t")
            pump(0.6)
        pump(args.wait / 2)
    for key in expand(args.keys or ""):
        child.send(key)
        pump(0.6)
    pump(args.after)
    lines = list(screen.display)
    child.send("q")
    pump(0.5)
    return lines


def command(args):
    hub = os.environ.get("HUB", "fb@172.16.10.90")
    result = subprocess.run(["ssh", "-o", "BatchMode=yes", hub, args.cmd], capture_output=True, text=True, timeout=120)
    return (result.stdout + result.stderr).splitlines()


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--name", required=True)
    p.add_argument("--cols", type=int, default=110)
    p.add_argument("--rows", type=int, default=30)
    p.add_argument("--wait", type=float, default=10)
    p.add_argument("--after", type=float, default=1.5, help="seconds to wait after the last key")
    p.add_argument("--keys")
    p.add_argument("--section", choices=SECTIONS)
    p.add_argument("--crop", help="FIRST:LAST line numbers (0-based, LAST exclusive)")
    p.add_argument("--cmd")
    args = p.parse_args()
    lines = command(args) if args.cmd else tui(args)
    if args.crop:
        first, last = (int(x) if x else None for x in args.crop.split(":"))
        lines = lines[first:last]
    text = "\n".join(clean(lines)) + "\n"
    out = Path(__file__).resolve().parent.parent / "captures" / f"{args.name}.txt"
    out.write_text(text)
    print(f"wrote {out} ({len(text.splitlines())} lines)")


if __name__ == "__main__":
    sys.exit(main())
