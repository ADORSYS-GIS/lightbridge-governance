# Spike #34 Findings: Can Codex Admin Config Pin OTel?

## Answer

**NO**

## Evidence from Source Code

### 1. `ConfigRequirementsToml` structure (`codex-rs/config/src/config_requirements.rs:875-909`)

The `ConfigRequirementsToml` structure (which represents `requirements.toml` / admin config) contains these fields:

- `sqlite_home`
- `log_dir`
- `model_catalog_json`
- `check_for_update_on_startup`
- `allow_login_shell`
- `feedback`
- `allowed_approval_policies`
- `allowed_approvals_reviewers`
- `allowed_sandbox_modes`
- `allowed_permission_profiles`
- `default_permissions`
- `remote_sandbox_config`
- `allowed_web_search_modes`
- `allow_managed_hooks_only`
- `allow_appshots`
- `allow_remote_control`
- `computer_use`
- `browser_use`
- `windows`
- `feature_requirements`
- `hooks`
- `mcp_servers`
- `plugins`
- `marketplaces`
- `apps`
- `rules`
- `enforce_residency`
- `network`
- `permissions`
- `models`
- `guardian_policy_config`

There is **no `otel` field** in `ConfigRequirementsToml`.

---

### 2. `OtelConfigToml` structure (`codex-rs/config/src/types.rs:549`)

The `[otel]` configuration exists **only** in `ConfigToml` (user configuration):

```rust
pub struct OtelConfigToml {
    pub log_user_prompt: Option<bool>,
    pub environment: Option<String>,
    pub exporter: Option<OtelExporterKind>,
    pub trace_exporter: Option<OtelExporterKind>,
    pub metrics_exporter: Option<OtelExporterKind>,
    pub span_attributes: Option<BTreeMap<String, String>>,
    pub tracestate: Option<BTreeMap<String, BTreeMap<String, String>>>,
}
```

This is part of `ConfigToml` (line 497 in `config_toml.rs`), **not** `ConfigRequirementsToml`.

---

### 3. Config layer precedence (`codex-rs/config/src/config_layer_source.rs:31-48`)

Although managed configs have the highest precedence (`LegacyManagedConfigTomlFromMdm = 50`), they can only override fields that exist in `ConfigRequirementsToml`.

Since `otel` is **not** part of that structure, managed/admin configs **cannot** pin or enforce OTel settings.

---

## Conclusion

Codex's admin configuration (`requirements.toml`) **cannot** pin or enforce OTel settings.

The `[otel]` section is **user-controlled only**. Developers can modify their `~/.codex/config.toml` to change or disable OTel settings, and there is currently no admin mechanism to prevent this.

---

## Impact on Story #33

This means:

- Coverage claims should be downgraded to **best-effort**.
- Developers can disable Codex telemetry by modifying their local configuration.
- Any dashboard showing **Codex spend per engineer** measures only users who have telemetry enabled, **not** actual Codex usage.
- Epic #30's Codex success metric should be expressed as a **proportion of participating users**, rather than assuming near-complete coverage.

---

## Recommendation

Document this limitation explicitly in **Story #33** and **Epic #30**.

The Codex telemetry rollout should be treated as **advisory rather than enforceable**. Reassess whether this limitation materially affects Story #33 or whether alternative approaches (for example, server-side telemetry or proxy-based instrumentation) are required to achieve the desired coverage guarantees.