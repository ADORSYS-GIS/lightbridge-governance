# The default flow: one login, three tools, one collector

How a developer points Claude Code, Codex and VS Code at the Lightbridge
gateway and the governed OTel collector, using the authz IdP for identity.

Everything below was **verified live on 2026-08-31** against
`auth.ai.camer.digital`, `api.ai.camer.digital` and `otel.ai.camer.digital`.
Where a value differs from an older document, this one is the measurement and
the other is the memory — see *Corrections* at the end.

## The three endpoints

| Role | URL | Verified |
|---|---|---|
| IdP (authz) | `https://auth.ai.camer.digital` | discovery returns 200 |
| Gateway | `https://api.ai.camer.digital` | `/v1/models/info` returns 200 |
| Collector | `https://otel.ai.camer.digital` | returns **401** without a bearer |

⚠️ **The issuer has no realm path.** `authz-idp` *is* the issuer; a
`/realms/<something>` suffix 404s at discovery. ADR-0012's example
(`auth.verif.fyi/realms/camer-digital`) predates the move to authz-idp and no
longer resolves.

⚠️ **The collector authenticates.** A client that exports to it without a
bearer gets 401 and drops every span silently. That is the correct posture, and
it is why one of the three tools below has a caveat rather than a clean path.

## Step 1 — install the binary

```bash
cargo build --release --bin governance-auth
install -m 755 target/release/governance-auth ~/.local/bin/governance-auth
```

`~/.local/bin` is the per-user location ADR-0012 defines. A locally built binary
always reports `governance-auth 0.1.0` regardless of how current it is — the
workspace version is never bumped, and released builds get their version
injected at build time. **`--version` cannot tell you whether a local build is
current**; compare against `origin/main` instead.

## Step 2 — write the config once

```bash
mkdir -p ~/.config/governance-auth
cat > ~/.config/governance-auth/config.toml <<'EOF'
issuer        = "https://auth.ai.camer.digital"
client_id     = "governance-auth-cli"
gateway_url   = "https://api.ai.camer.digital"
otel_endpoint = "https://otel.ai.camer.digital"
EOF
chmod 600 ~/.config/governance-auth/config.toml
```

With this present, no command below needs a flag. `gateway_url` gates the
**inference** wiring and `otel_endpoint` gates the **telemetry** wiring; set one
and you get that half only. Both are set here because the default flow wants
both.

The file must not be group- or other-readable — it may later carry
`otel_token_file`, and a permissive mode is refused rather than loaded.

## Step 3 — log in

```bash
governance-auth login
```

Browser, authorization code + PKCE, one time. The refresh token lands at `0600`
under the state directory. Afterwards:

```bash
governance-auth status
```

Run this in a **terminal** — the dashboard only renders for a TTY, and the
piped form still prints raw negative seconds for an expired session.

## Step 4 — configure the tools

```bash
governance-auth configure
```

One command writes all three. Every key is **merged into** the existing file,
never replacing it, and every key it writes is recorded in
`~/.config/governance-auth/managed.json` as a hash — so it can be retracted
later, and a value you have since edited yourself is never removed.

### Claude Code — `~/.claude/settings.json`

| Key | Value | Gated on |
|---|---|---|
| `apiKeyHelper` | `<abs path to governance-auth> token` | gateway |
| `env.ANTHROPIC_BASE_URL` | `<gateway>/anthropic` | gateway |
| `env.CLAUDE_CODE_API_KEY_HELPER_TTL_MS` | refresh cadence | gateway |
| `env.CLAUDE_CODE_ENABLE_GATEWAY_MODEL_DISCOVERY` | `1` | gateway |
| `env.CLAUDE_CODE_ENABLE_TELEMETRY` | `1` | collector |
| `env.OTEL_EXPORTER_OTLP_ENDPOINT` | the collector | collector |
| `env.OTEL_EXPORTER_OTLP_PROTOCOL` | `http/protobuf` | collector |
| `env.OTEL_METRICS_EXPORTER` / `OTEL_LOGS_EXPORTER` | `otlp` | collector |
| `otelHeadersHelper` + debounce | re-runs `otel headers` | collector |

**Claude Code is the only tool with a fully self-renewing credential**, because
it supports a headers *helper*: a hook that re-runs `governance-auth
otel headers` on a debounce shorter than the token lifetime. Nothing goes stale
and no long-lived secret is stored.

### Codex — `~/.codex/config.toml`

| Key | Value | Gated on |
|---|---|---|
| `model_provider` | `governance` | gateway |
| `model_providers.governance.{name,base_url,wire_api}` | `<gateway>/v1`, OpenAI-compatible | gateway |
| `model_providers.governance.auth.command` | absolute path to `governance-auth` | gateway |
| `otel.environment` | environment tag | collector |
| `otel.{exporter,metrics_exporter}.otlp-http.{endpoint,protocol,headers.Authorization}` | collector wiring | collector |

⚠️ **`auth.command` must be an absolute path** — Codex spawns it without a
shell, so `PATH` is not consulted.

⚠️ **Codex's `headers.Authorization` is a static string read once at start.**
It does not refresh. That is a known limit of Codex's config surface, not a
choice made here.

### VS Code — `<User>/settings.json`

Written for each flavour present: `Code`, `Code - Insiders`, `VSCodium`.

| Key | Value | Gated on |
|---|---|---|
| `github.copilot.chat.otel.enabled` | `true` | collector |
| `github.copilot.chat.otel.exporterType` | `file` | collector |
| `github.copilot.chat.otel.outfile` | the resolved spool path | collector |
| `github.copilot.chat.otel.captureContent` | `false` | collector |

**Copilot does not export over the network at all. It appends to a file, and
`copilot push` ships it on a schedule `configure` installs.**

| Platform | Schedule | Files |
|---|---|---|
| Linux | systemd user timer, every 300s | `~/.config/systemd/user/governance-auth-copilot-push.{service,timer}` |
| macOS | launchd agent, `StartInterval 300` | `~/Library/LaunchAgents/digital.camer.ai.governance-auth.copilot-push.plist` |

That makes Copilot the **second self-renewing client** after Claude Code, by a
different route: Claude Code refreshes its own header through
`otelHeadersHelper`; Copilot never holds a credential at all, and the drain
obtains a fresh bearer per wake. **Codex is now the only client that needs a
long-lived `--otel-token`.**

🚨 **This used to be the broken half.** `exporterType` was `otlp-http` with no
Authorization header, so the collector returned 401 on every span while the
config looked complete. `github.copilot.chat.otel.headers` *does* exist — an
`{ "key": "value" }` map applied to the exporter — but it is **static**, and
`settings.json` is covered by Settings Sync, so writing a bearer there carries
it off-machine. The file exporter avoids the choice entirely.

⚠️ Restart VS Code after `configure`. Copilot reads these at window start.

⚠️ The spool grows fast (measured 73 KB → 315 KB in six minutes of ordinary use,
and 164 MB on one machine). `copilot push` now reclaims it: past 1 MiB, a wake
whose file size is exactly its checkpoint offset — so every byte was delivered —
truncates it to zero and says so. This reverses what this page said before, on
the evidence that Copilot opens the spool `O_APPEND`, confirmed on macOS by
descriptor offsets and on Linux by `/proc/PID/fdinfo`
([#230](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/230) /
[#241](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/241)).

### The shell — `~/.config/governance-auth/otel.env` (and `.fish`)

Both at `0600`, because they can hold a credential. Contents:
`GOVERNANCE_AUTH_ISSUER`, `GOVERNANCE_AUTH_CLIENT_ID`, `ANTHROPIC_BASE_URL`,
and — only when a collector is configured — `OTEL_EXPORTER_OTLP_ENDPOINT`,
`_PROTOCOL`, `OTEL_METRICS_EXPORTER`, `OTEL_LOGS_EXPORTER`,
`OTEL_RESOURCE_ATTRIBUTES` and `OTEL_EXPORTER_OTLP_HEADERS`.

Source it from your shell rc so the rc file itself stays safe to commit.

## Step 5 — the VS Code extension (separate from the above)

The Lightbridge extension serves **models**, not telemetry, and is configured
independently:

```json
"lightbridge.gatewayUrl": "https://api.ai.camer.digital",
"lightbridge.governanceAuthPath": "/home/<you>/.local/bin/governance-auth"
```

The base URL is the **bare host** — the extension appends `/v1/models/info` and
`/v1/chat/completions` itself. Adding `/v1` here produces `/v1/v1/...`.

`governanceAuthPath` should be absolute: the extension spawns without a shell,
and a desktop-launched VS Code often lacks `~/.local/bin` on `PATH`.

`configure` does not write these yet. When it does, workspace settings will
still win over user settings — which is deliberate, so a repo can point itself
at a local gateway.

## Verifying it actually works

Not "no errors" — these are the observable outcomes.

```bash
governance-auth status          # in a TTY: session fresh, and the managed targets
governance-auth token >/dev/null && echo "credential resolves"
curl -s -o /dev/null -w '%{http_code}\n' https://api.ai.camer.digital/v1/models/info
```

Per tool:

- **Claude Code** — a request succeeds and `ANTHROPIC_BASE_URL` in
  `~/.claude/settings.json` points at `<gateway>/anthropic`.
- **Codex** — `model_provider = "governance"` is present and `auth.command` is
  an absolute path that exists.
- **VS Code / Copilot telemetry** — read the OTLP row in `governance-auth
  status` (#217). `never applied` and `no credential` are the two ways this
  silently does nothing, and both are invisible from inside the editor.
- **VS Code / Lightbridge extension** — the picker lists models and
  **Lightbridge: Show log** reports the count. Zero models with no other error
  means a credential problem; zero with a mapping error means a schema problem.

## Corrections to older documents

| Document | Says | Actually |
|---|---|---|
| ADR-0012 §2 example | `https://auth.verif.fyi/realms/camer-digital` | `https://auth.ai.camer.digital`, **no realm path** |
| RFC-0003 *Risks* | "VS Code exposes no settings key for OTLP headers" | `github.copilot.chat.otel.headers` exists; it is unusable for a *different* reason (static, and synced) |
| RFC-0003 §4 | "advertises no `client_credentials` grant" — the widest blocker | the live discovery document **does** advertise `client_credentials` |

The last one matters beyond this runbook: RFC-0003 calls the missing
machine-to-machine grant the blocker for 7 of its 12 rows. Confirm a client is
actually provisioned for it before re-planning on that, but the discovery
document no longer says what the RFC says it says.
