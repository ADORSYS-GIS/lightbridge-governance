# ADR-0012: `governance-auth` on-disk layout, packaging and distribution

- Status: Proposed
- Date: 2026-08-14
- Decision owners: @stephane-segning

## Context

`governance-auth` (ADR-0010) started as a credential helper and has grown into
something developers install: it holds a refresh token, writes four other
tools' config files, and updates itself. That makes "where does it put things,
and how does a developer get and keep it current" a real design question rather
than an implementation detail.

The target is `governance-auth <command>` working immediately after an
`install`, with updates arriving through the channel the developer already
uses.

Everything below is marked by how it was established:

| Mark | Meaning |
|---|---|
| **[repo]** | read out of this repository |
| **[verified]** | confirmed this session against a primary source (upstream docs or source) |
| **[unverified]** | believed true, NOT confirmed — do not rely on it without checking |

### What is already true

- The session cache, its `0700` state directory, the discovery cache, the
  `logout` revocation and the self-update ownership refusal all landed
  separately as the safety fixes this ADR's research surfaced. This ADR does
  not restate them; it starts from that baseline. **[repo]**
- `otel.rs` writes `~/.config/governance-auth/otel.env` at `0600` with **no
  macOS branch**, while `cache.rs` branches per-platform for cache and state.
  That asymmetry is deliberate and load-bearing here: *one convention per KIND
  of data, not one per platform*. **[repo]**
- Config today is CLI flags plus `GOVERNANCE_AUTH_*` env vars. There is no
  config file, so every invocation must be given `--issuer` and `--client-id`
  or it errors. **[repo]**
- Codex spawns `auth.command` **without a shell**, so commands written into
  other tools' configs must be absolute paths. **[repo]**

## Decision

### 1. On-disk layout

`/bin/governance` and `~/.ai-governance/` from the original sketch are both
rejected: `/bin` is OS-owned, and a new dotdir contradicts the
`~/.config/governance-auth/` this binary already writes.

| Role | Linux | macOS |
|---|---|---|
| Binary, per-user | `~/.local/bin/governance-auth` | same |
| Binary, manual system-wide | `/usr/local/bin/` | `/usr/local/bin/` |
| Machine-wide config | `/etc/governance-auth/config.toml` | `/etc/governance-auth/config.toml` |
| Per-user config | `$XDG_CONFIG_HOME`, else `~/.config/governance-auth/` | `~/.config/governance-auth/` |
| State (session, lock) | `$XDG_STATE_HOME`, else `~/.local/state/` | `~/Library/Application Support/` |
| Cache (OIDC discovery) | `$XDG_CACHE_HOME`, else `~/.cache/` | `~/Library/Caches/` |
| Log | `$XDG_STATE_HOME/governance-auth/logs/`, else `~/.local/state/…` | `~/Library/Logs/governance-auth/` |

**The log row was added after the fact** (see `app/governance-auth/src/logging`) and follows
the same "one convention per KIND of data" rule as the rest. Logs are their own kind: the XDG
spec names "actions history (logs, …)" as `$XDG_STATE_HOME` content, so Linux is state plus one
segment; macOS is `~/Library/Logs` rather than Application Support because that is what
Console.app reads and — decisively — where the launchd agent this binary installs already
redirected the drain's stderr. That capture was moved onto the same file rather than a second
one being created beside it, and it is now bounded (copy-truncate at 1 MiB, 3 generations).

**Machine-wide config is `/etc/` on macOS too**, and this is the one place we
knowingly diverge from
[`claude-code-managed-settings.md`](../integrations/claude-code-managed-settings.md),
which uses `/Library/Application Support/ClaudeCode/`. Reasons, in order:
this binary already rejected per-platform config paths at the per-user layer
for a stated reason that applies identically here; `/Library/Application
Support` is an app-bundle convention and `governance-auth` is a bare Unix CLI
with no bundle identity; and `git` — the closest comparable — uses
`/etc/gitconfig` unchanged on macOS **[verified]**.

### 2. Config file, five layers

Highest wins: **CLI flag → env var → per-user file → machine-wide file →
compiled default.**

Machine-wide is the *lowest* file layer and deliberately **not** enforcing,
which is the opposite of the Claude Code managed-settings model. Nothing in
this file is security-enforcing — a developer who overrides `issuer` simply
fails to authenticate. Enforcement lives at the gateway and at Keycloak. Of
five comparable tools surveyed, only `git` has a genuine machine-wide layer at
all (`/etc/gitconfig`); `gh`, `kubectl`, `docker` and `aws-cli` have none
**[verified]**.

**Format: TOML**, deserialized with `toml_edit`, which is already a dependency
— but its `serde` feature is **not currently enabled** and must be added
**[repo]**. `serde_yaml` is declared in the workspace and used by zero members,
and is archived upstream, so it is not a candidate **[repo, verified]**.

⚠️ **A trap that makes this more than an additive change.** `scopes` and
`otel_headers_debounce_ms` use clap's `default_value`/`default_value_t`, so
clap fills them the instant flag and env are absent — before any file layer is
consulted. They would *silently never* fall through to a config file. Both must
become `Option`, with the compiled defaults moved into `resolve()`. **[repo]**

**Secrets.** If a config file carrying `otel_token` is group- or
other-readable, refuse to load it and print the exact `chmod` — the SSH
precedent, and the same posture `otel.rs` already takes with its `0600` env
file **[repo, verified]**. Also accept `otel_token_file = "/path"`
(the `*_FILE` convention) so a machine-wide file can point at MDM/ESO-managed
material rather than inlining it. Wrap on load in the existing `Redacted<T>`.

Target state — the runbook's step 2 becomes `governance-auth login`, no flags:

```toml
# /etc/governance-auth/config.toml
issuer          = "https://auth.verif.fyi/realms/camer-digital"
client_id       = "governance-auth-cli"
gateway_url     = "https://api.ai.camer.digital"
otel_endpoint   = "https://otel.ai.camer.digital"
otel_token_file = "/etc/governance-auth/otel-token"
```

**`configure` keeps embedding `--issuer`/`--client-id`** in the commands it
writes. The string that lands in someone's `settings.json` must be
self-contained and must not change meaning when a config file is edited or
deleted — and on Codex the failure mode of getting that wrong is silent
unauthenticated operation.

### 3. Credentials stay in files. No OS keychain.

| Consideration | Verdict |
|---|---|
| Better at rest | True on macOS. On Linux, libsecret is readable by any process running as that user once the keyring is unlocked — roughly what a `0600` file already gives, against a "code execution as the user" threat model |
| Headless / SSH / Coder | **Decisive against.** `--device-code` exists because headless is first-class, and the org's rollout channel is Coder workspaces — containers with no D-Bus session or keyring daemon |
| Non-interactive invocation | **Decisive against.** `token` is spawned unattended every 240s by two programs. A keychain prompt there is a *hang*, not a failure |
| Dependency cost | A large new surface on a security-adjacent binary, plus a `deny.toml` review |

The effort goes instead into server-side revocation on `logout` (already
landed), `0700` on the state directory (landed), and Keycloak-side refresh
rotation and idle timeouts — controls the org already owns. A
`credential_store` key is deliberately **not** added as an unimplemented stub.

### 4. Distribution: Homebrew tap first, install script alongside

**A Homebrew formula can install a bare, non-archived binary.** Homebrew's
`UnpackStrategy` falls back to `Uncompressed`, which copies the file through
untouched — confirmed in Homebrew's own `unpack_strategy/uncompressed.rb`, and
corroborated by a maintainer in Homebrew Discussions #4439 **[verified]**. Our
assets are bare executables, so this matters. `chmod 0755` explicitly; a
curl-fetched file is not executable by default **[unverified]**.

Order: **tap first** (one formula, macOS *and* Linuxbrew, gives `brew upgrade`
essentially free), then the **`curl | sh` installer** — not a competitor but
the fallback for Coder images and non-brew Linux, and the thing that writes the
install receipt self-update's ownership rule wants. Then `mise`/`ubi` (a doc
line). `cargo-binstall` is closed off by `publish = false` **[repo]**.
deb/rpm last and only on demand — highest ongoing cost and the channel where
self-update most obviously must refuse.

Installer conventions, from rustup / starship / uv **[verified]**: only uv
(cargo-dist) verifies a checksum in-shell; rustup and starship rely on TLS
alone. Starship prints PATH instructions rather than editing rc files. **Ours
verifies the `.sha256` we already publish, and prints the PATH line rather than
editing rc files** — this binary already writes a managed block to four rc
files, and two independent writers to `.zshrc` is how that becomes a mess.

⚠️ **Tap automation needs a PAT, not `GITHUB_TOKEN`** — the default token
cannot push to a separate tap repo. Both community bump actions document this
**[verified]**. That means a new secret in the release path.

### 5. Build matrix must change before any of this ships

Three findings, each independent of packaging:

1. **The glibc floor is too high.** `ubuntu-latest`/`ubuntu-24.04-arm` produce
   binaries requiring **glibc 2.39**, which fails on Ubuntu 22.04 (2.35),
   Debian 12 (2.36), RHEL/Rocky 9 (2.34) and Amazon Linux 2023 (2.34) — a
   loader failure before `main`, not a graceful error **[verified]**. Adding
   the two `-musl` targets fixes it.
2. **This binary uses `aws-lc-rs`, not `ring`.** The workspace pins
   `rustls = { features = ["ring"] }`, but that only binds crates depending on
   `rustls` *directly* — which is the server, not this binary, which gets it
   transitively via `reqwest`'s `rustls` feature → `__rustls-aws-lc-rs`
   **[verified via `cargo tree`]**. The manifest comment is misleading for
   `governance-auth`. Non-FIPS `aws-lc-sys` on musl needs only a C compiler
   (no cmake, no Go) **[verified]**, so musl is cheaper than assumed.
3. **`macos-13` was retired in December 2025** and the workflow still names it
   **[verified upstream; the entry is at line 34 of the release workflow]**.
   Prefer cross-compiling `x86_64-apple-darwin` from `macos-latest` over
   `macos-15-intel`, which itself sunsets in 2027.

Two consequences that are easy to miss: `asset_name()` branches only on
`target_os`/`target_arch`, so a musl build would still request the `-gnu`
asset — it needs a `target_env` branch **[repo]**. And the release workflow
builds with stock `--release`, not this repo's own `[profile.prod]` (LTO +
strip), so today's assets ship unstripped **[repo]**.

### 6. Provenance: attestations now; in-process verification deferred

Add `actions/attest-build-provenance` to the release workflow
(`id-token: write`, `attestations: write`, `contents: read`). Keyless, no key
custody, and it closes the gap for every human and every republisher
**[verified]**.

⚠️ **It does not close the gap for `self-update`**: `gh attestation verify`
currently **requires authentication**, so a colleague would need `gh auth
login` before the binary could verify itself **[verified via an open
`cli/cli` issue — recheck, it may have shipped]**.

For in-process verification, when it is done, prefer an **Ed25519-signed
checksum manifest** over the `sigstore` crate: `ed25519-dalek` is
BSD-3-Clause, which is already on this repo's allowlist **[repo]**, and the
tree is ~12 well-known crates, versus `sigstore`'s self-described experimental
API and 40+ dependencies including `tokio`, `reqwest`, `oci-client`,
`openidconnect` and `tough` **[verified]**. The honest cost: Ed25519
reintroduces long-lived private-key custody and a rotation dance that cosign's
keyless flow avoids entirely. That trade is **Open question 3**.

**Gatekeeper is a separate axis and smaller than it looks.**
`com.apple.quarantine` is set by browsers, not by `curl`, and a Homebrew
**Formula** (unlike a Cask) never touches it — Homebrew's quarantine module is
Cask-only **[verified]**. So `self-update`, which writes bytes directly, is
very likely unaffected today. The exposure is the *initial browser download*
from the releases page.

## Consequences

**Positive**

- `governance-auth login` with no flags becomes true — the actual ask.
- `brew upgrade` becomes the update channel, and self-update stops being able
  to fight it.
- The Linux binaries start working on the distros developers actually run.
- Attestations close the asymmetry with this repo's cosign-signed images.

**Negative, stated plainly**

- A tap repo to own, and a PAT in the release path — a second repo and a new
  secret in the critical path.
- musl changes the build matrix and adds an `asset_name()` branch; musl also
  bypasses NSS entirely, resolving only via `/etc/hosts` and `/etc/resolv.conf`
  **[verified]**. On a laptop whose DNS goes through `systemd-resolved`'s NSS
  module, a musl binary can fail to resolve the issuer while every glibc tool
  on the same machine succeeds. **This needs testing on one real developer
  machine before musl becomes the default Linux asset.**
- The config file adds a layer to reason about and a permission check that can
  refuse to start. Correct, but it will surprise someone.
- Self-update becomes *less* capable for anyone on brew. That is the point, and
  it will still read as a regression in a bug report.

## Alternatives considered

- **OS keychain for the refresh token** — rejected in §3; the blocking issue is
  non-interactive and headless invocation, not security preference.
- **`homebrew/core` instead of a tap** — core requires source builds and has
  notability thresholds this tool would not meet **[unverified]**.
- **`macos-15-intel`** — defers the Intel-runner problem by roughly a year
  rather than solving it.
- **Renaming to `governance`** — rejected. The absolute path plus subcommand is
  embedded as a *string* in `settings.json`, `config.toml`, VS Code settings
  and four shell rc files, and `configure` cannot reach hand-written blocks,
  MDM-pushed settings or dotfiles repos. On Codex the failure is silent. It is
  a one-way door for a cosmetic gain; if ever done, it needs a `governance`
  multiplexer plus the old name as a shim for ≥2 releases.

## Open questions — maintainer decisions, not defaults

1. **Versioning.** This binary's version is the whole repo's, so every server
   release nags every laptop. Splitting it fixes that but breaks
   `releases/latest` as the self-update source. **[repo]**
2. **Apple Developer ID + notarization** (~$99/yr plus a CI signing identity) —
   buys a clean browser-download experience. Given §6, this is narrower than it
   first appears.
3. **Sigstore keyless vs org-held Ed25519** — the key-custody question.
4. **Should a stale client ever be refused tokens?** Guarantees currency;
   also turns a failed update into an outage. Recommendation is warn-only, but
   "everyone stays current" is a policy statement, so the strength of it is
   yours.
5. **Machine-wide config on macOS**: `/etc/` (this ADR) vs `/Library/
   Application Support/` (the Claude Code doc). Deliberate divergence — worth
   an explicit yes or no.
6. **The Linux distro floor** — decides whether musl is additive or replaces
   the glibc assets.
7. **ADR numbering collision**: `0010` is used by two files, and
   `0010-governance-auth-...` is missing from the index. **[repo]**

## Sequencing

| Stage | Work | Blocked on |
|---|---|---|
| ~~0~~ | Safety fixes: state/cache split, ordered version compare, revocation, discovery cache | **done** |
| 1 | Fix `macos-13`; add musl targets + `asset_name()` `target_env` branch; build with `[profile.prod]` | Q6 |
| 2 | Config file, both layers, permission refusal; `Option`-ise the two defaulted fields | Q5 |
| 3 | Install script (writes the install receipt) | Stage 2 |
| 4 | Homebrew tap + release-workflow bump job | Stage 3, and a PAT |
| 5 | `actions/attest-build-provenance` | — |
| 6 | In-process signature verification | Q3 |

Stage 1 is first because it is a correctness fix for people who cannot run the
binary today, and it is independent of every packaging decision above.

## Related

- [ADR-0010](./0010-governance-auth-keycloak-oauth2-credential-helper.md) —
  what this binary is and why it exists
- [`claude-code-managed-settings.md`](../integrations/claude-code-managed-settings.md)
  — the org's existing machine-wide-config convention, which §1 diverges from
- `docs/integrations/ai-client-flows.md` — why a failed credential helper is
  worse than it looks on Codex. Not linked relatively because it lands in
  PR #128, which is still open; make this a link once that merges.
