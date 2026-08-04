# Spike 0034 — Can Codex admin `requirements.toml` pin the `[otel]` block?

- Status: Findings recorded; answer established from source. No empirical run needed —
  the answer is a property of the config schema/loader, not of a live install.
- Ticket: [#34](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/34) · Epic: [#30](https://github.com/ADORSYS-GIS/lightbridge-governance/issues/30)
- Owner: @stephane-segning · Date: 2026-08-03

## Decision

**No — Codex's admin `requirements.toml` cannot pin `[otel]`.** The requirements schema is a
closed, enumerated set of fields that does not include `otel` (or any telemetry key), and
requirements are applied as separate constraints — never merged into the config TOML that
`[otel]` is read from. An admin putting `[otel]` in `requirements.toml` is **silently
ignored** (dropped at deserialization), not enforced.

**Consequence for #33:** Codex telemetry is **advisory, not enforceable**. Any developer can
disable it in their own `~/.codex/config.toml`. #33's coverage claim must be downgraded to
**best-effort**, and the epic's success metric for Codex restated as a *proportion* of
enrolled developers, not "~all".

## Evidence (from source, not docs)

The answer is established from `openai/codex`'s Rust source, per the ticket's acceptance
criterion (this repo's docs have been shown to lag behaviour — cf. the `user.email` finding
in #29).

| Question | Answer | Source (file:line) |
|---|---|---|
| Does `requirements.toml` exist as an admin layer? | **Yes** — `/etc/codex/requirements.toml` (Unix), `%ProgramData%\OpenAI\Codex\requirements.toml` (Windows); plus cloud bundle, legacy `managed_config.toml`, macOS MDM | `codex-rs/config/src/loader/mod.rs:634-635`, `:684-686`, `:142-192` |
| Does it support `allow_managed_hooks_only`? | **Yes** — a real, documented field | `codex-rs/config/src/config_requirements.rs:889` (`ConfigRequirementsToml`), `:162` |
| Is `otel` a requirements field? | **No** — absent from `ConfigRequirementsToml` and `ConfigRequirements` (closed structs, destructured without `..`) | `codex-rs/config/src/config_requirements.rs:875-965`, `:1000` |
| Are requirements merged into the config TOML? | **No** — composed separately, applied afterward as constraints | `codex-rs/config/src/loader/mod.rs:195`, `:432-433`; `state.rs` `effective_config()` |
| What does `apply_to_config` actually overwrite? | Only `sqlite_home`, `log_dir`, `model_catalog_json`, `check_for_update_on_startup`, `allow_login_shell`, `feedback.enabled`, `windows.sandbox_private_desktop`. **No `otel`.** | `codex-rs/core/src/config/requirements.rs:13-50` |
| Where is `[otel]` resolved from? | Only from the merged config layers (`cfg.otel`), untouched by requirements | `codex-rs/core/src/config/mod.rs:4009` |
| Where is the precedence decision? | Requirements layers: system → cloud → legacy → admin. Config layers: admin → system → cloud → user → profile → cwd → tree → repo → runtime. Requirements and config are separate stacks. | `codex-rs/config/src/loader/mod.rs:82-116` |

## Version checked

- **`main` HEAD as of 2026-08-03** (0.147.0-alpha.x line).
- Latest stable release: **0.146.0** (tag `rust-v0.146.0`, 2026-07-29).
- `allow_managed_hooks_only` landed in PR #20319 (2026-05-13), present in 0.145.0+.
- **Not version-pinned** — read current `main`. The conclusion (no `otel` in requirements)
  holds across the versions inspected (0.144/0.145-era commits and current main); the
  requirements field list has grown over time but `otel` has never been among them.

## Variants: `codex`, `codex exec`, `codex mcp-server`

The **"no, requirements can't pin otel"** answer is identical for all three — it is a
property of the config schema/loader, not the entry point. The entry points differ only in
whether telemetry is emitted at all:

| Mode | Traces | Logs | Metrics | Notes |
|---|---|---|---|---|
| `codex` (interactive) | ✅ | ✅ | ✅ | Full OTel init |
| `codex exec` | ✅ | ✅ | ✅ (now) | Metrics were missing (#12913); fixed by PR #13083 |
| `codex mcp-server` | ✅ (now) | ✅ (now) | ✅ (now) | Had no OTel at all (#12913); PR #13080 added a collector |

None can be forced or disabled via `requirements.toml`, because `otel` is not in the
requirements schema for any entry point.

## Documented vs. source-only

- **Documented**: that `requirements.toml` exists, its locations, and that it constrains a
  specific set of security-sensitive settings (approval policy, sandbox, web search, managed
  hooks, MCP servers, plugins, etc.).
- **Source-only**: the specific fact that `[otel]` **cannot** be pinned is **not explicitly
  documented**. The docs list the requirements keys (and `otel` is absent), but nothing
  states "otel is not a requirements field." This conclusion is established only by reading
  the closed `ConfigRequirementsToml` struct and the separate requirements-application path.

## Upstream docs issue

Because the mechanism exists but the specific "otel is not a requirements field" fact is
undocumented, an upstream docs issue should be filed against openai/codex (the same courtesy
we would want). **Not yet filed** — pending owner confirmation.

## Verification evidence (to be completed)

- [ ] Decision comment posted on #33 and #30 (this ticket's acceptance criterion — the
      finding is posted on the issues, not held in a branch)
- [ ] #33's coverage claim downgraded to best-effort (explicit acceptance criterion)
- [ ] Upstream docs issue filed against openai/codex
- [ ] This file's Status flipped to "Findings recorded; decision made"