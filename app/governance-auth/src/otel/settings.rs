//! Shared settings for the OTLP configuration writers: what to write
//! ([`OtelSettings`]), plus the two helpers every writer and caller shares
//! ([`binary_path`], [`identity_attributes`]). The result type lives in
//! [`super::outcome`]. One module per target tool plus this shared settings
//! type is the natural seam (issue #176).

use std::{collections::BTreeMap, path::PathBuf};

use crate::redacted::Redacted;

/// Resolved OTLP export settings, shared by both writers so the two tools
/// can't drift to different endpoints or protocols.
#[derive(Debug, Clone)]
pub struct OtelSettings {
    /// The resolved issuer and client id, exported into the developer's shell
    /// so `governance-auth` itself works from any terminal without flags, and
    /// so a helper subprocess that does not inherit them can still resolve.
    pub issuer: String,
    pub client_id: String,
    /// Collector base URL, e.g. `https://otel.ai.camer.digital`. Signal
    /// suffixes (`/v1/metrics`, `/v1/logs`, `/v1/traces`) are appended by the
    /// SDKs themselves from this base -- do not include one here.
    ///
    /// `None` when the caller has no `--otel-endpoint` -- telemetry wiring is
    /// independent of inference/gateway wiring (see `gateway_url` below), so
    /// this can't be a bare `String` without forcing every caller to invent a
    /// value when only the gateway was configured. Every writer in this
    /// module treats `None` as "skip telemetry entirely for this tool", never
    /// as an empty-string endpoint.
    pub endpoint: Option<String>,
    /// Absolute path VS Code Copilot Chat's *file* exporter is told to write,
    /// and the path `copilot push` drains. Resolved ONCE by the caller through
    /// ADR-0012's five layers, so `settings.json`'s `outfile` and the drain's
    /// default cannot disagree -- which they silently would if each side
    /// computed its own. See `crate::copilot::resolve_spool_path`.
    pub copilot_spool: PathBuf,
    /// Whether Copilot's *file* exporter should be turned on at all --
    /// distinct from `endpoint.is_some()`, which under the `daemon` profile
    /// is true (it holds the loopback substitute) even though `daemon` uses
    /// [`Self::copilot_otlp_direct`] for Copilot instead of this path.
    /// `vscode::configure`'s own doc already refuses to turn the exporter on
    /// with nowhere to push -- this is that same rule, reached by profile
    /// instead of by a missing endpoint. `false` here must retract, not just
    /// skip writing, any exporter config a prior `manual` run left behind;
    /// see `managed::plan`'s own use of this field.
    pub copilot_drain_available: bool,
    /// Whether Copilot's OWN `otlp-http` exporter should point directly at
    /// `endpoint` (issue #272 AC3) -- the `daemon` profile's Copilot path,
    /// and mutually exclusive with [`Self::copilot_drain_available`] by
    /// construction (`TelemetryWiring::resolve` never sets both). `false`
    /// here must retract this path's keys for the same reason
    /// `copilot_drain_available = false` must retract the file exporter's.
    pub copilot_otlp_direct: bool,
    /// Long-lived OTLP ingest credential, rendered into the header value both
    /// tools send verbatim. `None` writes the endpoint but no header, which
    /// is only useful against a collector that doesn't authenticate.
    pub token: Option<Redacted<String>>,
    /// Stamped onto every exported signal. Carries who this developer is, so
    /// telemetry arriving at the collector is attributable without the
    /// collector having to resolve the OTLP credential back to a person.
    pub resource_attributes: BTreeMap<String, String>,
    /// Command Claude Code re-invokes for fresh OTLP headers
    /// (`otelHeadersHelper`). When set, telemetry auth is self-renewing and
    /// the static `OTEL_EXPORTER_OTLP_HEADERS` is not written for that
    /// client -- the two would fight, and a stale static value silently
    /// winning is exactly the failure this replaces.
    pub headers_helper: Option<String>,
    /// How often Claude Code re-runs the helper. Its own default is 29
    /// MINUTES, which is far longer than a Keycloak access token lives
    /// (300s) -- leaving it alone would mean exporting with an expired token
    /// for most of every half-hour, silently. This must stay below the
    /// token lifetime.
    pub headers_helper_debounce_ms: u64,
    /// The `governance-auth … token` command Claude Code spawns through
    /// `apiKeyHelper` for a fresh inference credential. Codex receives the
    /// same logical invocation as a separate executable and argument array.
    ///
    /// Built from [`super::binary_path`] so both clients name the same
    /// installed binary. Codex requires that absolute path in `auth.command`
    /// and every flag in `auth.args`; combining them makes the entire string
    /// an executable filename and fails with OS error 2.
    pub token_command: String,
    /// Gateway base URL. `Some` turns on inference wiring in both writers;
    /// `None` leaves every inference key untouched, so a telemetry-only
    /// `configure` can't clobber a hand-tuned provider block.
    pub gateway_url: Option<String>,
}

impl OtelSettings {
    /// `key=value,key=value`, the W3C-ish encoding both
    /// `OTEL_RESOURCE_ATTRIBUTES` and Codex expect. `BTreeMap` (not a plain
    /// map) so the rendered string is deterministic -- an unstable ordering
    /// would make every `login` rewrite the config file with a spurious diff.
    pub(crate) fn resource_attributes_value(&self) -> String {
        self.resource_attributes
            .iter()
            .map(|(key, value)| format!("{key}={value}"))
            .collect::<Vec<_>>()
            .join(",")
    }

    pub(crate) fn headers_value(&self) -> Option<String> {
        self.token
            .as_ref()
            .map(|token| format!("Authorization=Bearer {}", token.expose()))
    }

    /// `<gateway>/anthropic` -- Claude Code appends `/v1/messages` itself.
    pub(crate) fn anthropic_base_url(&self) -> Option<String> {
        self.gateway_url
            .as_ref()
            .map(|base| format!("{}/anthropic", base.trim_end_matches('/')))
    }

    /// `<gateway>/v1` -- the OpenAI-compatible base Codex appends to.
    pub(crate) fn openai_base_url(&self) -> Option<String> {
        self.gateway_url
            .as_ref()
            .map(|base| format!("{}/v1", base.trim_end_matches('/')))
    }
}

/// Absolute path to the running binary, for any command string written into
/// another tool's config. Falls back to the bare name only when the path is
/// genuinely unavailable -- see [`OtelSettings::token_command`] for what a
/// bare name costs on Codex.
pub fn binary_path() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|path| path.to_str().map(str::to_owned))
        .unwrap_or_else(|| "governance-auth".to_owned())
}

/// Pulls `sub`/`email` out of a JWT access token's payload for use as OTLP
/// resource attributes, so exported telemetry is attributable to a person.
///
/// **Deliberately does not verify the signature**, and must not be used for
/// any authorization decision. This token came from the token endpoint over
/// TLS moments ago and is only being read to label this machine's own
/// outgoing telemetry; the collector re-derives trusted identity itself and
/// never trusts these attributes (RFC-0002's trust boundary: tenant context
/// comes from the authenticated credential, never from the payload body).
/// Returns whatever it can parse -- a token shaped differently, or one that
/// isn't a JWT at all, yields no attributes rather than an error, because
/// failing `login` over a cosmetic label would be the wrong trade.
pub fn identity_attributes(access_token: &str) -> BTreeMap<String, String> {
    use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

    let mut attributes = BTreeMap::new();
    let Some(payload) = access_token.split('.').nth(1) else {
        return attributes;
    };
    let Ok(decoded) = URL_SAFE_NO_PAD.decode(payload) else {
        return attributes;
    };
    let Ok(claims) = serde_json::from_slice::<serde_json::Value>(&decoded) else {
        return attributes;
    };

    for (claim, attribute) in [
        ("sub", "user.id"),
        ("email", "user.email"),
        ("preferred_username", "user.name"),
    ] {
        if let Some(value) = claims.get(claim).and_then(serde_json::Value::as_str)
            && !value.is_empty()
        {
            attributes.insert(attribute.to_owned(), value.to_owned());
        }
    }
    attributes
}
