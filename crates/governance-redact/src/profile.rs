//! Redaction profiles: what we detect, and what we do about it.
//!
//! A profile is the whole policy for one class of traffic. It decides three
//! things, and keeping them together is the point — a threshold without an
//! action is meaningless, and an action without a fail-closed rule is a
//! liability.
//!
//! 1. **Which entities are in scope**, and at what confidence floor.
//! 2. **What happens** to each one — see [`Action`].
//! 3. **What happens when we cannot decide** — see [`Profile::fail_closed`].

use std::collections::HashMap;

use pii::{anonymize::Operator, config::PolicyConfig, types::EntityType};

use crate::secrets::{private_key_entity, secret_entity};

/// What to do with a detected entity.
///
/// [`Action::Block`] is deliberately *not* a `pii` [`Operator`]: upstream's
/// operators all transform a span in place, whereas blocking rejects the whole
/// request and never forwards a body. That is a decision about the request, not
/// an edit to a string, so it lives here.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Leave the span untouched. Used for entity classes that are noise in our
    /// traffic (see [`Profile::coding_assistant`]).
    Allow,
    /// Reject the entire request. The body is never forwarded upstream.
    Block,
    /// Replace with a label, e.g. `<EMAIL>`.
    Replace,
    /// Mask, keeping a trailing suffix (last 4 of a card, say).
    Mask,
    /// Replace with a salted SHA-256 digest, so the same value is consistently
    /// pseudonymous across a conversation without being recoverable.
    Hash,
}

impl Action {
    /// The `pii` operator that implements this action, if it is a transform.
    ///
    /// Returns `None` for [`Action::Allow`] (nothing to do) and
    /// [`Action::Block`] (handled before anonymization — the request never
    /// reaches the transform stage).
    #[must_use]
    pub fn operator(&self, salt: &str) -> Option<Operator> {
        match self {
            Self::Allow | Self::Block => None,
            Self::Replace => Some(Operator::Redact),
            Self::Mask => Some(Operator::Mask {
                ch: '*',
                from_end: 4,
            }),
            Self::Hash => Some(Operator::HashSha256 {
                salt: salt.to_string(),
            }),
        }
    }
}

/// A named redaction policy.
#[derive(Debug, Clone)]
pub struct Profile {
    /// Stable identifier, used in metrics and audit records.
    pub name: &'static str,
    /// Action applied to any in-scope entity with no explicit override.
    pub default_action: Action,
    /// Per-entity action overrides, keyed by [`EntityType::as_str`].
    pub actions: HashMap<String, Action>,
    /// Confidence floors handed to the analyzer.
    pub policy: PolicyConfig,
    /// When true, any failure in the redaction path rejects the request rather
    /// than forwarding content we have not inspected.
    ///
    /// ⚠️ This is the setting that decides whether an outage of *this* service
    /// becomes an outage or a silent data leak. It defaults to `true` on every
    /// profile that handles real traffic.
    pub fail_closed: bool,
}

impl Profile {
    /// Resolves the action for a detected entity type.
    #[must_use]
    pub fn action_for(&self, entity: &EntityType) -> Action {
        self.actions
            .get(&entity.as_str())
            .cloned()
            .unwrap_or_else(|| self.default_action.clone())
    }

    /// Whether this profile can detect personal *names*.
    ///
    /// ⚠️ Currently always `false`, and that is not an oversight worth hiding.
    /// `Person`/`Location`/`Organization` in the `pii` crate come only from an
    /// NER model, and the `candle-ner` feature ships a trait rather than a
    /// model — so until we implement and pin one, names are not detected by any
    /// profile. Anything that reports coverage to a user must read this rather
    /// than assume.
    #[must_use]
    pub const fn detects_names(&self) -> bool {
        false
    }

    /// The default profile for traffic from coding assistants.
    ///
    /// Tuned for what our gateway actually carries — opencode, Kilo-Code and
    /// LibreChat, i.e. source code, stack traces, package names and file paths.
    /// Two deliberate departures from a naive "redact everything" policy:
    ///
    /// - **`Url`, `Domain` and `Hostname` are [`Action::Allow`].** Upstream
    ///   scores them 0.5–0.7 and their regexes match anything dotted, so in
    ///   this traffic they fire on every import, crate name and file path. A
    ///   redactor that mangles `serde.rs` in a code review is one that gets
    ///   turned off, which protects nothing.
    /// - **Credentials and private keys are [`Action::Block`], not replaced.**
    ///   A leaked key is not improved by being redacted *after* we have already
    ///   decided to forward the request; the request should not go at all.
    #[must_use]
    pub fn coding_assistant() -> Self {
        let mut actions = HashMap::new();

        // Credentials never leave the boundary.
        actions.insert(secret_entity().as_str(), Action::Block);
        actions.insert(private_key_entity().as_str(), Action::Block);

        // High-sensitivity identifiers: keep a suffix so a human can still
        // recognise which card/account was involved when reading an audit log.
        actions.insert(EntityType::CreditCard.as_str(), Action::Mask);
        actions.insert(EntityType::Iban.as_str(), Action::Mask);
        actions.insert(EntityType::BankAccount.as_str(), Action::Mask);
        actions.insert(EntityType::Ssn.as_str(), Action::Mask);
        actions.insert(EntityType::Itin.as_str(), Action::Mask);
        actions.insert(EntityType::TaxId.as_str(), Action::Mask);

        // Structural noise in code. See the doc comment above.
        actions.insert(EntityType::Url.as_str(), Action::Allow);
        actions.insert(EntityType::Domain.as_str(), Action::Allow);
        actions.insert(EntityType::Hostname.as_str(), Action::Allow);
        // A UUID in a stack trace is an identifier, not personal data.
        actions.insert(EntityType::Uuid.as_str(), Action::Allow);

        let mut thresholds = HashMap::new();
        // Raise the floor on the two noisiest pattern recognizers even though
        // they are allowed above, so they do not crowd out a real detection
        // during overlap resolution (which prefers the higher score).
        thresholds.insert(EntityType::Domain, 0.95);
        thresholds.insert(EntityType::Hostname, 0.95);
        // Phone stays at the default 0.6 floor and is deliberately NOT raised.
        // Upstream's phone recognizer is a `RegexRecognizer`, which emits a
        // *fixed* score of 0.6 with no context boost, so any threshold above
        // 0.6 does not tighten the detector — it switches it off entirely.
        // This previously sat at 0.7, which read as tuning but meant no phone
        // number was ever redacted in this profile. The pattern does collide
        // with version strings and byte arrays in code; a mangled digit run in
        // a code review is the cheaper failure of the two.

        Self {
            name: "coding-assistant",
            default_action: Action::Replace,
            actions,
            policy: PolicyConfig {
                enabled_entities: std::collections::HashSet::new(),
                thresholds,
                default_threshold: 0.6,
            },
            fail_closed: true,
        }
    }

    /// Credentials only — everything else passes untouched.
    ///
    /// For traffic where PII redaction would do more harm than good but a
    /// leaked key still must not leave. The narrowest useful policy.
    #[must_use]
    pub fn secrets_only() -> Self {
        let mut actions = HashMap::new();
        actions.insert(secret_entity().as_str(), Action::Block);
        actions.insert(private_key_entity().as_str(), Action::Block);

        Self {
            name: "secrets-only",
            default_action: Action::Allow,
            actions,
            policy: PolicyConfig {
                enabled_entities: std::collections::HashSet::new(),
                thresholds: HashMap::new(),
                default_threshold: 0.6,
            },
            fail_closed: true,
        }
    }

    /// Detect and report, change nothing.
    ///
    /// The only profile with `fail_closed: false`, because it makes no
    /// promises: it is for measuring what a stricter profile *would* have done
    /// against real traffic before enforcing it.
    #[must_use]
    pub fn observe_only() -> Self {
        Self {
            name: "observe-only",
            default_action: Action::Allow,
            actions: HashMap::new(),
            policy: PolicyConfig {
                enabled_entities: std::collections::HashSet::new(),
                thresholds: HashMap::new(),
                default_threshold: 0.5,
            },
            fail_closed: false,
        }
    }

    /// Looks up a profile by name.
    ///
    /// Returns `None` for an unknown name — the caller must reject rather than
    /// fall back, since silently substituting a weaker profile is the exact
    /// failure this type exists to prevent.
    #[must_use]
    pub fn by_name(name: &str) -> Option<Self> {
        match name {
            "coding-assistant" => Some(Self::coding_assistant()),
            "secrets-only" => Some(Self::secrets_only()),
            "observe-only" => Some(Self::observe_only()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use pii::types::EntityType;

    use super::{Action, Profile};
    use crate::secrets::{private_key_entity, secret_entity};

    #[test]
    fn coding_assistant_blocks_credentials() {
        let p = Profile::coding_assistant();
        assert_eq!(p.action_for(&secret_entity()), Action::Block);
        assert_eq!(p.action_for(&private_key_entity()), Action::Block);
    }

    #[test]
    fn coding_assistant_allows_structural_noise() {
        let p = Profile::coding_assistant();
        for entity in [
            EntityType::Url,
            EntityType::Domain,
            EntityType::Hostname,
            EntityType::Uuid,
        ] {
            assert_eq!(
                p.action_for(&entity),
                Action::Allow,
                "{entity:?} should be allowed in coding traffic"
            );
        }
    }

    #[test]
    fn coding_assistant_replaces_by_default() {
        let p = Profile::coding_assistant();
        assert_eq!(p.action_for(&EntityType::Email), Action::Replace);
    }

    #[test]
    fn secrets_only_leaves_personal_data_alone() {
        let p = Profile::secrets_only();
        assert_eq!(p.action_for(&EntityType::Email), Action::Allow);
        assert_eq!(p.action_for(&secret_entity()), Action::Block);
    }

    #[test]
    fn every_traffic_handling_profile_fails_closed() {
        assert!(Profile::coding_assistant().fail_closed);
        assert!(Profile::secrets_only().fail_closed);
        // observe-only is the documented exception: it promises nothing.
        assert!(!Profile::observe_only().fail_closed);
    }

    #[test]
    fn unknown_profile_name_is_rejected_not_defaulted() {
        // Silently falling back to a weaker profile is the failure mode this
        // guards against; `None` forces the caller to reject.
        assert!(Profile::by_name("does-not-exist").is_none());
        assert!(Profile::by_name("").is_none());
    }

    #[test]
    fn known_profile_names_round_trip() {
        for name in ["coding-assistant", "secrets-only", "observe-only"] {
            let p = Profile::by_name(name).expect("profile should resolve");
            assert_eq!(p.name, name);
        }
    }

    #[test]
    fn block_and_allow_have_no_transform_operator() {
        assert!(Action::Block.operator("salt").is_none());
        assert!(Action::Allow.operator("salt").is_none());
        assert!(Action::Replace.operator("salt").is_some());
        assert!(Action::Mask.operator("salt").is_some());
        assert!(Action::Hash.operator("salt").is_some());
    }

    #[test]
    fn no_profile_claims_name_detection_yet() {
        // Guards the honesty of `detects_names`. When NER lands this test is
        // the thing that should force the claim to be re-examined.
        assert!(!Profile::coding_assistant().detects_names());
    }
}
