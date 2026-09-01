# Configuration

Every option can be set four ways, and they are resolved in a fixed order
(ADR-0012 Decision 2):

```
CLI flag  →  env var  →  per-user config file  →  machine-wide config file  →  compiled default
```

Each layer only supplies what the layers above it didn't. Nothing is all-or-nothing: a
machine-wide file can supply the issuer while a flag overrides the scopes.

| Layer | Where |
|---|---|
| 1. CLI flag | `--issuer …` |
| 2. Env var | `GOVERNANCE_AUTH_ISSUER=…` |
| 3. Per-user file | `$XDG_CONFIG_HOME/governance-auth/config.toml`, else `~/.config/governance-auth/config.toml` |
| 4. Machine-wide file | `/etc/governance-auth/config.toml` |
| 5. Compiled default | only for `scopes`, `otel_headers_debounce_ms`, `open_browser`, `token_exchange` |

`~/.config` on **macOS too** — a deliberate divergence from the Claude Code managed-settings
convention, argued in ADR-0012 Decision 1. There is no XDG-like systemwide analogue on
either platform, so the machine-wide path is a fixed constant.

## The full option matrix

| Flag | Env var | Config key | Default | What it is |
|---|---|---|---|---|
| `--issuer` | `GOVERNANCE_AUTH_ISSUER` | `issuer` | **required** | Base URL of the issuing OIDC realm. Endpoints are found underneath it by discovery, never hand-derived. |
| `--client-id` | `GOVERNANCE_AUTH_CLIENT_ID` | `client_id` | **required** | Public OAuth2 client id. Public — no client secret ships in a binary distributed to laptops. |
| `--scopes` | `GOVERNANCE_AUTH_SCOPES` | `scopes` | `openid profile offline_access` | Space-separated scopes requested at login. |
| `--audience` | `GOVERNANCE_AUTH_AUDIENCE` | `audience` | — | Optional `resource`/`audience` parameter, if the server needs one to scope the token to the gateway. |
| `--gateway-url` | `GOVERNANCE_AUTH_GATEWAY_URL` | `gateway_url` | — | AI gateway base URL. Presence turns on **inference** wiring in `configure`. |
| `--otel-endpoint` | `GOVERNANCE_AUTH_OTEL_ENDPOINT` | `otel_endpoint` | — | OTLP collector **base** URL. Presence turns on **telemetry** wiring. Do not append a signal path. |
| `--otel-token` | `GOVERNANCE_AUTH_OTEL_TOKEN` | `otel_token` / `otel_token_file` | — | Long-lived OTLP ingest credential. See [Secrets](#secrets-in-config-files). |
| `--copilot-spool-path` | `GOVERNANCE_AUTH_COPILOT_SPOOL_PATH` | `copilot_spool_path` | `<state dir>/governance-auth/copilot-otel.jsonl` | Where VS Code Copilot Chat's file exporter writes, for [`copilot-push`](./commands.md#copilot-push) to drain. Must match `github.copilot.chat.otel.outfile`. Not checked for existence — Copilot creates it on its first export. |
| `--otel-headers-debounce-ms` | `GOVERNANCE_AUTH_OTEL_HEADERS_DEBOUNCE_MS` | `otel_headers_debounce_ms` | `240000` | How often Claude Code re-runs the helpers. Must stay **below** the access-token lifetime. |
| `--open-browser` | `GOVERNANCE_AUTH_OPEN_BROWSER` | `open_browser` | `false` | Whether `login`'s loopback flow launches a browser. Usable bare (`--open-browser`) or explicit (`--open-browser=false`). |
| `--token-exchange` | `GOVERNANCE_AUTH_TOKEN_EXCHANGE` | `token_exchange` | `false` | Opt into RFC 8693 exchange in `token`/`otel-headers`. See [`token-exchange.md`](./token-exchange.md). |
| `--exchange-issuer` | `GOVERNANCE_AUTH_EXCHANGE_ISSUER` | `exchange_issuer` | — | Exchange server, resolved by discovery. |
| `--exchange-token-endpoint` | `GOVERNANCE_AUTH_EXCHANGE_TOKEN_ENDPOINT` | `exchange_token_endpoint` | — | Exchange token endpoint given directly; skips discovery. **Wins over `--exchange-issuer`** when both are set. |
| `--exchange-client-id` | `GOVERNANCE_AUTH_EXCHANGE_CLIENT_ID` | `exchange_client_id` | — | `client_id` presented on the exchange. **Required** once exchange is on. |
| `--exchange-scopes` | `GOVERNANCE_AUTH_EXCHANGE_SCOPES` | `exchange_scopes` | — | Scopes requested on the exchange. Omitting it takes the server's allow-list. |

Three flags are **not** global config, because they belong to one subcommand only:
`login --device-code`, `self-update --check` and `copilot-push --dry-run`.

`RUST_LOG` is honoured for tracing output, which goes to stderr like everything else.

### `240000` is not an arbitrary number

Claude Code's own helper debounce default is **29 minutes**. An access token here lives 300
seconds. Left alone, that means exporting telemetry with an expired token for most of every
half hour — and failing *silently* while doing it. 240s sits comfortably inside the token
window. If your realm's `accessTokenLifespan` differs, this value has to move with it.

## Config file format

TOML, snake_case keys mirroring the flags. Both file layers use the same shape.

```toml
issuer    = "https://auth.example/realms/platform"
client_id = "governance-auth-cli"

gateway_url    = "https://api.example"
otel_endpoint  = "https://otel.example"
otel_token_file = "/etc/governance-auth/otlp-token"

otel_headers_debounce_ms = 240000
open_browser = false

token_exchange          = true
exchange_issuer         = "https://auth.example"
exchange_client_id      = "governance-auth-exchange-cli"
```

Three behaviours worth knowing:

- **A missing file is normal**, not an error. Most machines have no machine-wide file and a
  fresh developer has no per-user one.
- **A malformed file is a loud error**, not a silent fall-through to the next layer. Falling
  through would hide a real typo in a file its author believes is in effect.
- **Unknown keys are rejected.** This file is owned entirely by `governance-auth` (unlike
  Claude Code's or Codex's, which are merged into), so an unrecognised key is a typo — and a
  typo that fails loudly beats one field silently never taking effect.

## Validation

Every URL-shaped option — `issuer`, `exchange_issuer`, `exchange_token_endpoint`,
`otel_endpoint`, `gateway_url` — must be `https://`, unless it is loopback
(`127.0.0.1`, `::1`, `localhost`).

This binary is a public OAuth2 client handling authorization codes, PKCE verifiers, access
and refresh tokens. Plaintext HTTP anywhere in that path — whether from an operator's typo
or from an attacker rewriting a response — lets those credentials be replayed against the
real authorization server. The predicate is therefore applied at **three independent
points**, so no single omission reopens the hole:

1. At CLI-parse time, before any network use.
2. In discovery, which re-validates the issuer and **origin-pins** every endpoint the
   discovery document hands back against it — so a discovery response cannot redirect
   credential-bearing requests to a different host.
3. In the HTTP client's redirect policy, which re-checks **every hop**, so a same-origin
   HTTPS request cannot be walked down to plaintext by a `3xx`.

A value that arrives from a **config file** never passes through the CLI parser, so it is
re-validated on resolve. Without that, a plaintext typo in `/etc/governance-auth/config.toml`
would reach the network unchecked.

The loopback carve-out is structural and fixed — deliberately *not* an "allow insecure" flag
or env var, which would be a test double reachable from a production path.

## Secrets in config files

`otel_token` is the one genuinely-credential field a config file can carry. Two rules:

- A file that **inlines** `otel_token` and is readable by group or other is **refused**, not
  loaded.
- `otel_token_file = "/path"` lets a machine-wide file — which, like `/etc/gitconfig`, is
  reasonably world-readable — point at MDM- or ESO-managed material instead of inlining the
  secret. The pointed-at file carries the same hazard, so it gets the same permission check.

Setting both `otel_token` and `otel_token_file` is an error. Silently preferring one would
be a misconfiguration nobody would ever notice.

---

## For maintainers: the `default_value` trap

**Never give a config-backed option a clap `default_value` or `default_value_t`.**

Clap fills a default in the instant the flag *and* its env var are both absent — which is
*before* either config-file layer is consulted. The field is then never `None`, so there is
nothing for a config file to win against, and layers 3 and 4 silently stop existing for that
option. This is not hypothetical: `scopes` and `otel_headers_debounce_ms` both shipped that
way and both had to be converted to `Option` with the default moved into `resolve`.

The same trap applies to booleans in a different costume: a plain `ArgAction::SetTrue` flag
bakes in `false` the moment the flag is absent. `open_browser` and `token_exchange` are
therefore `Option<bool>` with `num_args = 0..=1` + `default_missing_value = "true"`, which
keeps `--open-browser` usable bare while still leaving the field `None` when it is never
mentioned.

`config.rs`'s `tests::precedence` module guards this, but note *which* tests can: only the
handful that go through **real clap parsing** (`try_parse_from`) can observe a mistake in the
`#[arg(...)]` attribute itself. Every test that hand-builds `OauthConfigArgs { scopes: None,
… }` proves the layering logic and is blind to the attribute. If you add an option, copy a
real-parse test, not a hand-built one — and sabotage it once to confirm it fails.

One free guard exists and one does not: `default_value_t = 240_000` no longer compiles on an
`Option<u64>` field (it needs `Display`), but the string form `default_value = "240000"`
compiles fine and reintroduces the identical bug silently.
