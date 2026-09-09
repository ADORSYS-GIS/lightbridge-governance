# The local otel daemon stopped forwarding telemetry

**Symptom:** `governance-auth`'s `serve --otel` daemon logs, repeatedly and without ever
recovering on its own:

```
WARN governance_auth::otel_daemon::drain::quarantine: the collector refused this record on
enough separate attempts, but also refused the probe meant to confirm it accepts anything
else -- held, not discarded status=400 Bad Request
ERROR governance_auth::otel_daemon: collector permanently refused logs; discarding, not
retained status=400 Bad Request
```

Claude Code / Codex / VS Code Copilot telemetry from this machine stops reaching the
governed collector — nothing about the client tools themselves looks wrong, because they
are exporting fine to the *local* loopback daemon; it is the daemon's own outbound leg that
is stuck.

## Is this actually the wedge, or something else?

A single bad record is not this runbook — the daemon already recovers from that on its own
(see "Why this happens" below), and you will see `discarded_total` in the checkpoint file
move forward within seconds, with no repeating WARN. Confirm the wedge specifically:

```bash
# Linux
cat "${XDG_STATE_HOME:-$HOME/.local/state}/governance-auth/otel-daemon-checkpoint.json"
# macOS
cat "$HOME/Library/Application Support/governance-auth/otel-daemon-checkpoint.json"
```

A `quarantine` entry whose `refusals` keeps climbing across repeated checks (minutes apart,
not the same read twice) and never disappears, together with the two log lines above
repeating for the *same* record, confirms the wedge. `offset` in this file will also be
completely static across those same minutes — the daemon otherwise advances it continuously
as new telemetry arrives.

## Why this happens

`otel_daemon::drain::quarantine` ([`quarantine.rs`](../../app/governance-auth/src/otel_daemon/drain/quarantine.rs))
only discards a record the collector has permanently refused (HTTP 400/413/422) once it can
*prove* the collector accepts something else — otherwise a collector that is misconfigured to
refuse everything would be answered by silently emptying the spool, one real record at a
time. That proof is a single probe: the record **immediately after** the stuck one
([`DurableSpool::peek_next`](../../app/governance-auth/src/otel_daemon/spool/read.rs), which
reads at most one record ahead, on purpose — see its own doc comment).

That is sufficient for one bad record. It is not sufficient for **two or more consecutive**
bad records: the probe itself gets refused, the daemon reads that exactly like "the collector
is broken right now" (indistinguishable from a real outage, by design), and it never looks
further ahead. The stuck record is held forever — and because this daemon's spool is
strictly ordered, so is everything behind it, no matter how much good data is waiting.

Discovered live on 2026-09-09 (koufan-ws): two consecutive OTLP protobuf records from
`codex-app-server`, both genuinely malformed —

```
proto: wrong wireType = 2 for field TimeUnixNano
```

— confirmed identical against `otelcol-contrib` 0.158.0, 0.160.0, and the real production
endpoint (so this was never a collector-side bug, config bug, or a symptom of that day's
otel-collector chart changes — replaying the exact bytes reproduces the same rejection
everywhere). 447 refusals accumulated on the stuck record while 384 good records sat
undelivered behind it.

**Where the malformed bytes come from is still open.** They are forwarded byte-for-byte —
`normalize::stamp` only ever touches JSON payloads, and OTLP telemetry from these clients is
protobuf, passed through completely unchanged (see that module's doc). So the corruption
happens either in the originating client's own OTLP export, or somewhere before this daemon's
local receive path buffers it — not in anything this repository's collector or transform
config does to it afterward. Confirmed instead of assumed: a single corrupt record 40 minutes
after the fix below was cleanly self-healed by the *existing* one-ahead probe, which means the
occasional single bad record is not this bug — only a **run of two or more in a row** is.

## Fix: skip the stuck run, once you can prove the collector is fine past it

[`scripts/otel-daemon-unstick.py`](../../scripts/otel-daemon-unstick.py) automates exactly
the procedure below: it replays each record starting at the checkpoint offset against the
**real** collector (proof, not a guess from local structure — a spool line can be valid JSON
while the OTLP protobuf payload it carries is still corrupt) until it finds the first one
that is accepted, then removes exactly that run. Dev-workstation tool only, standard library
Python, same convention as `scripts/generate_dashboards.py`.

```bash
# 1. Stop the daemon first, on both OSes -- this races a live writer otherwise.
systemctl --user stop governance-auth-serve-otel.service                                    # Linux
launchctl bootout gui/$(id -u)/digital.camer.ai.governance-auth.serve-otel                  # macOS

# 2. Dry run first -- reports what it found, touches nothing.
python3 scripts/otel-daemon-unstick.py \
  --otel-endpoint https://otel.ai.camer.digital \
  --issuer https://auth.ai.camer.digital --client-id governance-auth-cli

# 3. Only once you've read that output, apply it.
python3 scripts/otel-daemon-unstick.py \
  --otel-endpoint https://otel.ai.camer.digital \
  --issuer https://auth.ai.camer.digital --client-id governance-auth-cli \
  --apply

# 4. Restart.
systemctl --user start governance-auth-serve-otel.service                                   # Linux
launchctl bootstrap gui/$(id -u) \
  ~/Library/LaunchAgents/digital.camer.ai.governance-auth.serve-otel.plist                  # macOS
```

Use `--otel-endpoint`/`--issuer`/`--client-id` matching whichever public collector this
machine's daemon actually points at — `otel.ai.camer.digital` /
`governance-auth-cli` for the AI-CLI fleet (Claude Code, Codex, VS Code Copilot),
`otel-opencode.ai.camer.digital` / `opencode-cli` for OpenCode. `--state-dir` defaults to the
right OS-specific location (see the paths above); override it if `$XDG_STATE_HOME` puts it
somewhere else. Pass `--token <access-token>` instead of
`--issuer`/`--client-id` if you already have one (e.g. from `governance-auth token`).

The script never prints spool or response body content — only signal names, byte lengths,
and HTTP status codes, the same rule `AGENTS.md` holds the collector service to.

### What it actually changes, and why it is safe

It removes exactly the consecutive run of permanently-refused records at the head of the
spool, and nothing else — `offset` in the checkpoint does not move (the good record that
used to sit right after the bad run now starts at that same byte, since deleting bytes
*before* it shifts everything after them up to fill the gap).

The one property that makes this safe to do with a plain file edit: the checkpoint identifies
"which file this offset belongs to" by a SHA-256 digest of the file's first 4096 bytes
(`HEAD_BYTES`, [`copilot/spool/identity.rs`](../../app/governance-auth/src/copilot/spool/identity.rs)),
not by size or by rehashing on every read. As long as the stuck run starts at byte 4096 or
later — true in every real case, because reaching this bug requires enough accumulated
backlog to have two records stuck behind each other — editing there can never change that
digest, so the daemon will not mistake the edited file for a different one and reset the
whole drain to byte 0 (which would re-send everything as first-attempt records, no
idempotency key, risking duplicate rows downstream — see
[`normalize/mod.rs`](../../app/governance-auth/src/otel_daemon/normalize/mod.rs)'s module doc).
The script checks this and refuses to run at all, unconditionally, if the stuck run starts
inside that first 4096 bytes — there is no override, because there is no way to edit there
and still have the digest agree.

⚠️ **If you ever do this by hand instead of with the script:** replacing the spool file (a
temp-file-then-rename, same pattern this binary itself uses everywhere else) changes its
**inode** — a rename repoints the directory entry at the new file, so the path's inode after
the rename is the replacement's, not the original's. The very next daemon start compares that
against the inode recorded in `checkpoint.json` and, seeing a mismatch, treats it as a file
*replacement* and restarts the whole drain at byte 0 — exactly the outcome the digest check
above exists to prevent, and the digest agreeing does not save you here; inode and digest are
two independent conditions the real daemon checks, and either one disagreeing is enough. You
must update `checkpoint.json`'s recorded `inode`/`device` to the edited file's actual
post-edit values before restarting. The script does this for you.

### Confirming it worked

```bash
journalctl --user -u governance-auth-serve-otel.service --since "1 minute ago" -f   # Linux
tail -f ~/Library/Logs/governance-auth/governance-auth.log                          # macOS
```

`offset` in the checkpoint should be advancing steadily (check it twice, a few seconds apart)
with no repeat of the two WARN/ERROR lines from the top of this page. A large backlog can take
a little while to fully drain — that is expected, not a sign the fix didn't take.

## The underlying gap

The probe-ahead-by-exactly-one design is the actual bug — this class of failure recurs any
time two or more consecutive records are permanently unrecoverable, from whatever cause. See
[`quarantine.rs`](../../app/governance-auth/src/otel_daemon/drain/quarantine.rs) and
[`spool/read.rs`](../../app/governance-auth/src/otel_daemon/spool/read.rs) for the fix that
makes the probe scan forward past a bounded run of consecutive refusals instead of stopping
at the first one — once that ships, this runbook's manual/scripted recovery should only ever
be needed for a run longer than that bound.
