#!/usr/bin/env python3
"""Unstick governance-auth's local otel daemon (`serve --otel`) when its
durable spool is permanently wedged behind one or more corrupt records.

DEV-WORKSTATION TOOL ONLY -- run by a developer on their own machine against
their own state directory. Nothing in the server image, CI, or the deploy
path calls this. Standard library only, same rule as scripts/generate_dashboards.py.

## Background -- why this exists

`otel_daemon::drain::quarantine` (app/governance-auth/src/otel_daemon/drain/quarantine.rs)
only discards a record the collector has permanently refused (HTTP 400/413/422)
once it can *prove* the collector accepts something else: it peeks exactly ONE
record past the stuck one (`DurableSpool::peek_next`, spool/read.rs) and offers
it as a probe. If that next record is ALSO permanently refused, the probe fails
and the drain stays wedged forever -- even when perfectly good records sit
right behind both bad ones. Two (or more) consecutive corrupt records is
therefore a permanent stall, not a self-healing one, discovered live on
2026-09-09 (koufan-ws): two consecutive OTLP protobuf records from
`codex-app-server`, both with a genuine wire-type mismatch
(`proto: wrong wireType = 2 for field TimeUnixNano` -- confirmed identical
against otelcol-contrib 0.158.0, 0.160.0, and the real production endpoint, so
this is not a collector bug), deadlocked the daemon at 447+ refusals while 384
good records waited behind them.

This script finds the run of consecutive permanently-refused records at the
head of the spool (by actually replaying each one against the real collector,
the same way the daemon itself would -- not by guessing from local structure
alone, since a spool line can decode as valid JSON while the OTLP protobuf it
carries is still corrupt) and, with --apply, removes exactly that run.

## Why removing them is safe (read before editing anything by hand)

The spool's checkpoint identifies "which file `offset` was measured against"
by a SHA-256 digest of the file's first 4096 bytes (`HEAD_BYTES`,
copilot/spool/identity.rs) plus (inode, device). As long as the run being
removed starts at a byte offset >= 4096, editing it can never change that
digest, so the daemon will not treat the edited file as "replaced" and reset
to byte 0 (which would re-send the entire spool as first-attempt records,
carrying no idempotency key -- see otel_daemon/normalize/mod.rs's module doc
-- and risking duplicate rows downstream). This script refuses to run
(without --force) when the stuck offset is inside that first 4096 bytes,
because the safety argument above does not apply there.

`mv`-ing a replacement file onto the original path changes its inode (a
rename creates a new directory entry pointing at the NEW inode, at the
original inode's expense) -- if you are fixing this by hand rather than with
this script, plan to also update `checkpoint.json`'s recorded inode/device to
the post-edit file's actual values, or the very next daemon start will see an
identity mismatch and restart at byte 0 anyway.

## What this never does

Never prints spool or response body content -- only signal names, byte
lengths, and HTTP status codes (AGENTS.md: never log a request/response
body; a collector's 4xx body can echo the payload it rejected).
"""

from __future__ import annotations

import argparse
import base64
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

HEAD_BYTES = 4096  # Must match copilot::spool::identity::HEAD_BYTES.
SPOOL_FILE = "otel-daemon-spool.jsonl"
CHECKPOINT_FILE = "otel-daemon-checkpoint.json"

# Mirrors copilot::push::Signal::path() -- kept in lockstep by hand since
# this script has no access to the Rust enum.
SIGNAL_PATHS = {"Logs": "/v1/logs", "Metrics": "/v1/metrics"}


def default_state_dir() -> Path:
    """Same resolution as governance-auth's own `cache::state_dir()`."""
    xdg = os.environ.get("XDG_STATE_HOME")
    if xdg:
        return Path(xdg) / "governance-auth"
    home = Path.home()
    if sys.platform == "darwin":
        return home / "Library" / "Application Support" / "governance-auth"
    return home / ".local" / "state" / "governance-auth"


def sha256_prefix(data: bytes, n: int) -> str:
    return hashlib.sha256(data[:n]).hexdigest()


def read_line_at(raw: bytes, start: int) -> tuple[bytes, int]:
    """The raw bytes of one line starting at `start` (newline included, if
    present), and the offset right after it."""
    nl = raw.find(b"\n", start)
    end = len(raw) if nl == -1 else nl + 1
    return raw[start:end], end


def quarantine_key_at(raw: bytes, start: int) -> str:
    """Reproduces `Quarantine::key` (copilot/quarantine.rs) exactly: SHA-256
    of the line's text -- WITHOUT its trailing newline, and `.trim()`med --
    truncated to 32 hex chars. `copilot::spool::drain` builds that `text` by
    splitting on b'\\n' (so the newline itself is never part of any segment)
    and then lossily-decoding and trimming each segment; getting this wrong
    (e.g. hashing the raw line including its newline) silently fails to find
    the stale quarantine entry, rather than erroring -- confirmed by hand
    while writing this script, not assumed."""
    line, _ = read_line_at(raw, start)
    text = line.decode("utf-8", errors="replace").strip()
    return hashlib.sha256(text.encode("utf-8")).hexdigest()[:32]


def post_record(endpoint: str, token: str, signal: str, body: bytes, timeout: float) -> int:
    """Replays one record exactly as `forward::post` would (same headers,
    same wire format) and returns the HTTP status -- 200-299 means accepted.
    Never returns or prints the response body (see module doc)."""
    path = SIGNAL_PATHS[signal]
    url = endpoint.rstrip("/") + path
    req = urllib.request.Request(
        url,
        data=body,
        method="POST",
        headers={
            "Content-Type": "application/x-protobuf",
            "Authorization": f"Bearer {token}",
        },
    )
    try:
        with urllib.request.urlopen(req, timeout=timeout) as resp:
            return resp.status
    except urllib.error.HTTPError as exc:
        return exc.code


def mint_token(issuer: str, client_id: str, binary: str) -> str:
    out = subprocess.run(
        [binary, "--issuer", issuer, "--client-id", client_id, "token"],
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    if out.returncode != 0 or not out.stdout.strip().startswith("eyJ"):
        raise SystemExit(
            f"could not mint a token via `{binary} token` (exit {out.returncode}); "
            "run `governance-auth login` first, or pass --token directly"
        )
    return out.stdout.strip()


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--state-dir",
        type=Path,
        default=default_state_dir(),
        help="governance-auth state directory (default: auto-detected for this OS)",
    )
    parser.add_argument(
        "--otel-endpoint",
        required=True,
        help="the same --otel-endpoint the daemon runs with, e.g. https://otel.ai.camer.digital",
    )
    parser.add_argument("--issuer", help="OIDC issuer, to mint a token via `governance-auth token`")
    parser.add_argument("--client-id", help="OAuth2 client id, same purpose as --issuer")
    parser.add_argument("--token", help="a pre-minted access token, instead of --issuer/--client-id")
    parser.add_argument(
        "--governance-auth-bin",
        default=shutil.which("governance-auth") or "governance-auth",
        help="path to the governance-auth binary, if not on PATH",
    )
    parser.add_argument(
        "--apply",
        action="store_true",
        help="actually rewrite the spool and checkpoint (default: dry-run, report only)",
    )
    parser.add_argument(
        "--force",
        action="store_true",
        help="proceed even if the stuck run starts inside the first 4096 bytes "
        "(the head-digest safety argument in the module doc does not hold there)",
    )
    parser.add_argument(
        "--max-scan",
        type=int,
        default=20,
        help="give up looking for the first accepted record after this many consecutive "
        "refusals, rather than scanning an unbounded run (default: 20)",
    )
    args = parser.parse_args()

    spool_path = args.state_dir / SPOOL_FILE
    checkpoint_path = args.state_dir / CHECKPOINT_FILE

    if not spool_path.is_file() or not checkpoint_path.is_file():
        print(f"::error::no daemon spool/checkpoint at {args.state_dir} -- wrong --state-dir?", file=sys.stderr)
        return 1

    print(
        "⚠️  Stop the daemon first, on both OSes, or this races a live writer:\n"
        "  Linux:  systemctl --user stop governance-auth-serve-otel.service\n"
        "  macOS:  launchctl bootout gui/$(id -u)/digital.camer.ai.governance-auth.serve-otel\n",
        file=sys.stderr,
    )

    token = args.token
    if token is None:
        if not (args.issuer and args.client_id):
            print("::error::pass --token, or both --issuer and --client-id", file=sys.stderr)
            return 1
        token = mint_token(args.issuer, args.client_id, args.governance_auth_bin)

    checkpoint = json.loads(checkpoint_path.read_text())
    offset = checkpoint["offset"]
    recorded_head_len = checkpoint["spool"]["head_len"]
    recorded_head = checkpoint["spool"]["head"]

    raw = spool_path.read_bytes()
    print(f"spool: {len(raw)} bytes, checkpoint offset: {offset}")

    current_head = sha256_prefix(raw, recorded_head_len)
    if current_head != recorded_head:
        print(
            "::error::the spool's recorded head digest does not match the file on disk -- "
            "this is not the file the checkpoint was measured against (a replace-on-restart, "
            "per copilot/spool/identity.rs). Do not proceed; let the daemon's own restart "
            "detection handle this instead.",
            file=sys.stderr,
        )
        return 1

    if offset < HEAD_BYTES and not args.force:
        print(
            f"::error::checkpoint offset ({offset}) is inside the first {HEAD_BYTES} bytes -- "
            "editing there WOULD change the head digest and force a from-scratch restart. "
            "Pass --force only if you understand and accept that (re-sends the whole spool).",
            file=sys.stderr,
        )
        return 1

    # Walk forward from `offset`, replaying each record against the real
    # collector, until the first one that is ACCEPTED (proof the collector
    # itself is fine) or we run out of records or hit --max-scan.
    pos = offset
    bad_records: list[tuple[int, str, int, int]] = []  # (start, signal, len, status)
    good_start: int | None = None
    while pos < len(raw) and len(bad_records) < args.max_scan:
        line, next_pos = read_line_at(raw, pos)
        if not line.strip():
            break
        try:
            record = json.loads(line)
            payload = base64.b64decode(record["body"])
        except Exception as exc:  # noqa: BLE001 -- reported, not swallowed
            print(f"::error::record at byte {pos} does not even decode as {{signal, body}} JSON: {exc}", file=sys.stderr)
            return 1

        status = post_record(args.otel_endpoint, token, record["signal"], payload, timeout=15)
        if 200 <= status < 300:
            good_start = pos
            break
        print(f"  byte {pos}: {record['signal']}, {len(payload)}B -> HTTP {status} (refused)")
        bad_records.append((pos, record["signal"], len(payload), status))
        pos = next_pos

    if not bad_records:
        print("Nothing refused at the checkpoint offset -- the daemon is not stuck on a bad record.")
        print("(If it's still not draining, the cause is something else -- check the collector's own health.)")
        return 0

    if good_start is None:
        print(
            f"::error::{len(bad_records)} consecutive refusals and no accepted record found within "
            f"--max-scan={args.max_scan}. Refusing to guess how far the bad run extends -- re-run with "
            "a larger --max-scan, or investigate why the collector is refusing everything (it may not "
            "be a corrupt-record problem at all).",
            file=sys.stderr,
        )
        return 1

    print(
        f"\nFound {len(bad_records)} consecutive permanently-refused record(s) at the head of the "
        f"spool (bytes {offset}..{good_start}); the record at byte {good_start} is accepted."
    )

    if not args.apply:
        print("\nDry run only -- re-run with --apply to remove them.")
        return 0

    new_raw = raw[:offset] + raw[good_start:]
    new_head = sha256_prefix(new_raw, recorded_head_len)
    if new_head != recorded_head:
        print("::error::internal invariant violated -- new head digest would not match. Not writing anything.", file=sys.stderr)
        return 1

    tmp_path = spool_path.with_suffix(spool_path.suffix + ".unstick-tmp")
    tmp_path.write_bytes(new_raw)
    shutil.copymode(spool_path, tmp_path)
    tmp_path.replace(spool_path)  # atomic rename on both platforms

    st = spool_path.stat()
    checkpoint["spool"]["inode"] = st.st_ino
    checkpoint["spool"]["device"] = st.st_dev
    # Drop quarantine entries for the removed records -- their keys (a
    # digest of each record's exact JSON line, Quarantine::key) will never
    # be looked up again, so keeping them is just dead weight until the
    # weekly TTL prunes them anyway.
    for start, _signal, _len, _status in bad_records:
        key = quarantine_key_at(raw, start)
        checkpoint["quarantine"].pop(key, None)
    checkpoint_path.write_text(json.dumps(checkpoint))

    print(
        f"\nRemoved {len(bad_records)} record(s), {len(raw) - len(new_raw)} bytes. "
        f"Checkpoint offset stays {offset} -- it now points at the record that used to be at "
        f"byte {good_start}. Restart the daemon:\n"
        "  Linux:  systemctl --user start governance-auth-serve-otel.service\n"
        "  macOS:  launchctl bootstrap gui/$(id -u) "
        "~/Library/LaunchAgents/digital.camer.ai.governance-auth.serve-otel.plist"
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
