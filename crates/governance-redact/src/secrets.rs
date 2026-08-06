//! First-party credential/secret recognizers.
//!
//! The `pii` crate's [`EntityType`] has **no** secrets category — it covers
//! personal identifiers (email, phone, SSN, IBAN, cards, …) and nothing that
//! looks like an API key. For an AI gateway that is the wrong way round: the
//! highest-value thing to stop leaving our boundary is a credential pasted into
//! a prompt on its way to a third-party model provider.
//!
//! So we own this pack. That is not a workaround for a gap — it is the right
//! place for it either way, because the credentials that matter here are *ours*
//! (gateway keys, GitHub App tokens, Keycloak tokens, cluster kubeconfigs) and
//! no upstream pattern set would know their shapes.
//!
//! Every pattern below is anchored on a **distinctive literal prefix** rather
//! than on entropy or length. That is a deliberate trade: it will not catch a
//! bare high-entropy string, but it also will not fire on the base64 blobs,
//! hashes and UUIDs that fill ordinary coding-assistant traffic. A secrets
//! detector that cries wolf on every checksum gets its profile turned off,
//! which protects nothing.
//!
//! Confidences are high (0.9+) precisely *because* the prefixes are
//! distinctive: when `ghp_` is followed by 36 base62 characters, it is a GitHub
//! token, not a coincidence.

use pii::{
    recognizers::{Recognizer, regex::RegexRecognizer},
    types::EntityType,
};

/// Entity type emitted for a detected credential.
///
/// A [`EntityType::Custom`] string rather than a new upstream variant, since
/// the `pii` crate models entity types as a closed enum plus `Custom`.
#[must_use]
pub fn secret_entity() -> EntityType {
    EntityType::Custom("Secret".to_string())
}

/// Entity type emitted for a detected private key block.
#[must_use]
pub fn private_key_entity() -> EntityType {
    EntityType::Custom("PrivateKey".to_string())
}

/// One credential pattern: a stable id, the regex, and its confidence.
struct SecretPattern {
    id: &'static str,
    pattern: &'static str,
    score: f32,
    private_key: bool,
}

/// The credential patterns this pack recognises.
///
/// Ordered roughly by how catastrophic a leak would be. Each entry's regex is
/// prefix-anchored — see the module docs for why entropy heuristics are
/// deliberately not used.
const PATTERNS: &[SecretPattern] = &[
    // --- Private key material -----------------------------------------------
    // Matches the PEM armour rather than the body, so it fires on the opening
    // line without needing to span the (arbitrarily long) base64 payload.
    SecretPattern {
        id: "secret_private_key_pem",
        pattern: r"-----BEGIN (?:RSA |EC |DSA |OPENSSH |PGP )?PRIVATE KEY(?: BLOCK)?-----",
        score: 0.99,
        private_key: true,
    },
    // Standard PKCS#8 armour for a password-protected key. Deliberately a
    // separate pattern rather than folding into the one above: the prefix
    // alternation there is closed over specific key kinds, and "ENCRYPTED"
    // is not one of them, so a naive extension would need its own branch
    // anyway. Most modern tooling (`openssl pkcs8`, `ssh-keygen -p`) writes
    // this exact armour for a password-protected key.
    SecretPattern {
        id: "secret_encrypted_private_key_pem",
        pattern: r"-----BEGIN ENCRYPTED PRIVATE KEY-----",
        score: 0.99,
        private_key: true,
    },
    // --- GitHub -------------------------------------------------------------
    // ghp_ personal, gho_ oauth, ghu_/ghs_ App user/server, ghr_ refresh.
    // github_pat_ is the fine-grained form. Both shapes are fixed-length.
    SecretPattern {
        id: "secret_github_token",
        pattern: r"\bgh[pousr]_[A-Za-z0-9]{36,255}\b",
        score: 0.98,
        private_key: false,
    },
    SecretPattern {
        id: "secret_github_pat_fine_grained",
        pattern: r"\bgithub_pat_[A-Za-z0-9_]{50,255}\b",
        score: 0.98,
        private_key: false,
    },
    // --- Cloud providers ----------------------------------------------------
    // AWS access key ids: AKIA (long-term), ASIA (temporary/STS).
    SecretPattern {
        id: "secret_aws_access_key_id",
        pattern: r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b",
        score: 0.97,
        private_key: false,
    },
    // Google API keys.
    SecretPattern {
        id: "secret_gcp_api_key",
        pattern: r"\bAIza[0-9A-Za-z_-]{35}\b",
        score: 0.97,
        private_key: false,
    },
    // --- Model providers ----------------------------------------------------
    // The ones most likely to be pasted into a prompt *about* an AI integration.
    SecretPattern {
        id: "secret_openai_api_key",
        pattern: r"\bsk-(?:proj-)?[A-Za-z0-9_-]{20,}\b",
        score: 0.95,
        private_key: false,
    },
    SecretPattern {
        id: "secret_anthropic_api_key",
        pattern: r"\bsk-ant-[A-Za-z0-9_-]{20,}\b",
        score: 0.98,
        private_key: false,
    },
    // --- Generic service tokens ---------------------------------------------
    SecretPattern {
        id: "secret_slack_token",
        pattern: r"\bxox[baprs]-[A-Za-z0-9-]{10,}\b",
        score: 0.97,
        private_key: false,
    },
    SecretPattern {
        id: "secret_stripe_key",
        pattern: r"\b[rs]k_(?:live|test)_[A-Za-z0-9]{20,}\b",
        score: 0.97,
        private_key: false,
    },
    // --- JWTs ---------------------------------------------------------------
    // Three base64url segments with the fixed `eyJ` header prefix. Scored a
    // little lower: a JWT in a prompt is often a decoded example rather than a
    // live credential, and the header is only *probably* `{"` in base64.
    SecretPattern {
        id: "secret_jwt",
        pattern: r"\beyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\.[A-Za-z0-9_-]{10,}\b",
        score: 0.90,
        private_key: false,
    },
];

/// Builds the first-party credential recognizers.
///
/// A pattern that fails to compile is **skipped, not fatal** — one bad regex
/// must not take down the gateway, and the caller can compare the returned
/// length against [`pattern_count`] to detect it. That mirrors how `pii`'s own
/// presets degrade.
#[must_use]
pub fn secret_recognizers() -> Vec<Box<dyn Recognizer>> {
    let mut out: Vec<Box<dyn Recognizer>> = Vec::new();
    for p in PATTERNS {
        let entity = if p.private_key {
            private_key_entity()
        } else {
            secret_entity()
        };
        if let Ok(r) = RegexRecognizer::new(p.id, entity, p.pattern, p.score, p.id) {
            out.push(Box::new(r));
        }
    }
    out
}

/// How many credential patterns this pack defines.
///
/// Compare against `secret_recognizers().len()` to detect a regex that failed
/// to compile.
#[must_use]
pub const fn pattern_count() -> usize {
    PATTERNS.len()
}

#[cfg(test)]
mod tests {
    use super::{PATTERNS, pattern_count, secret_recognizers};

    #[test]
    fn every_pattern_compiles() {
        // The builder skips patterns that fail to compile, so a length
        // mismatch is exactly how a broken regex would show up in production.
        assert_eq!(
            secret_recognizers().len(),
            pattern_count(),
            "a credential regex failed to compile and was silently skipped"
        );
    }

    #[test]
    fn pattern_ids_are_unique() {
        let mut ids: Vec<&str> = PATTERNS.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        let before = ids.len();
        ids.dedup();
        assert_eq!(before, ids.len(), "duplicate credential pattern id");
    }
}
