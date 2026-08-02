//! The redaction engine: scan a string, decide, transform.

use pii::{
    Analyzer, PolicyConfig,
    anonymize::{AnonymizeConfig, Anonymizer},
    nlp::SimpleNlpEngine,
    presets::default_recognizers,
    types::{Detection, Language},
};

use crate::{
    error::{Error, Result},
    profile::{Action, Profile},
    secrets::secret_recognizers,
};

/// What the engine decided about one piece of text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing in scope was found; forward the original text.
    Clean,
    /// Text was rewritten. Forward the replacement.
    Redacted {
        /// The rewritten text.
        text: String,
        /// How many spans were transformed.
        count: usize,
    },
    /// A blocking entity was found. Do **not** forward anything.
    Blocked {
        /// Entity types that triggered the block, for the audit record and the
        /// caller-facing error. Deliberately the *types*, never the values.
        entities: Vec<String>,
    },
}

impl Verdict {
    /// Whether this verdict permits the request to proceed upstream.
    #[must_use]
    pub const fn is_blocked(&self) -> bool {
        matches!(self, Self::Blocked { .. })
    }
}

/// A configured redaction engine.
///
/// Holds an assembled analyzer, so recognizer construction and regex
/// compilation happen once at startup rather than per request.
pub struct Engine {
    analyzer: Analyzer,
    profile: Profile,
    salt: String,
    language: Language,
}

impl Engine {
    /// Builds an engine for a profile.
    ///
    /// `salt` is used for [`Action::Hash`]. It must be stable for the lifetime
    /// of a deployment (so the same value hashes consistently) and secret (so a
    /// digest cannot be brute-forced back to its input from a known value set —
    /// which for something like an email address is otherwise trivial).
    ///
    /// # Errors
    ///
    /// Returns [`Error::RecognizerBuild`] if any first-party credential pattern
    /// failed to compile. That is treated as fatal rather than degraded: a
    /// silently-missing credential recognizer is precisely the failure this
    /// service exists to prevent, so it must not start in that state.
    pub fn new(profile: Profile, salt: impl Into<String>) -> Result<Self> {
        let mut recognizers = default_recognizers();

        let secrets = secret_recognizers();
        let expected = crate::secrets::pattern_count();
        if secrets.len() != expected {
            return Err(Error::RecognizerBuild {
                expected,
                built: secrets.len(),
            });
        }
        recognizers.extend(secrets);

        let policy: PolicyConfig = profile.policy.clone();
        let analyzer = Analyzer::new(
            Box::new(SimpleNlpEngine::default()),
            recognizers,
            Vec::new(),
            policy,
        );

        Ok(Self {
            analyzer,
            profile,
            salt: salt.into(),
            language: Language::from("en"),
        })
    }

    /// The profile this engine enforces.
    #[must_use]
    pub const fn profile(&self) -> &Profile {
        &self.profile
    }

    /// Scans and, if needed, rewrites a single string.
    ///
    /// # Errors
    ///
    /// Returns [`Error::Analyze`] or [`Error::Anonymize`] if the underlying
    /// pipeline fails. Callers on a `fail_closed` profile must treat any error
    /// as a rejection — never as "nothing found".
    pub fn scan(&self, text: &str) -> Result<Verdict> {
        if text.is_empty() {
            return Ok(Verdict::Clean);
        }

        let analyzed = self
            .analyzer
            .analyze(text, &self.language)
            .map_err(|e| Error::Analyze(e.to_string()))?;

        if analyzed.entities.is_empty() {
            return Ok(Verdict::Clean);
        }

        // Blocking wins over every transform: if anything here must not leave,
        // the request stops and nothing is forwarded, so there is no point
        // computing a rewrite we will discard.
        let blocking: Vec<String> = analyzed
            .entities
            .iter()
            .filter(|d| self.profile.action_for(&d.entity_type) == Action::Block)
            .map(|d| d.entity_type.as_str())
            .collect();
        if !blocking.is_empty() {
            let mut entities = blocking;
            entities.sort_unstable();
            entities.dedup();
            return Ok(Verdict::Blocked { entities });
        }

        // Keep only what this profile actually transforms. Allowed entities are
        // dropped here rather than given a no-op operator, so the reported
        // count reflects real changes.
        let transformable: Vec<Detection> = analyzed
            .entities
            .into_iter()
            .filter(|d| self.profile.action_for(&d.entity_type) != Action::Allow)
            .collect();

        if transformable.is_empty() {
            return Ok(Verdict::Clean);
        }

        let mut config = AnonymizeConfig {
            default: self
                .profile
                .default_action
                .operator(&self.salt)
                .unwrap_or(pii::anonymize::Operator::Redact),
            per_entity: std::collections::HashMap::new(),
        };
        // Build one operator per entity TYPE, not per detection. `per_entity`
        // is keyed by type, so a body with 200 email addresses previously did
        // 200 identical `action_for` + `operator` round trips (each of which
        // allocates -- `Operator::HashSha256` owns its salt, and upstream's
        // field type gives us no way to borrow it) to write the same map entry
        // 200 times. The occupancy check makes it once per distinct type.
        for detection in &transformable {
            let key = detection.entity_type.as_str();
            if config.per_entity.contains_key(&key) {
                continue;
            }
            if let Some(op) = self
                .profile
                .action_for(&detection.entity_type)
                .operator(&self.salt)
            {
                config.per_entity.insert(key, op);
            }
        }

        let result = Anonymizer::anonymize(text, &transformable, &config)
            .map_err(|e| Error::Anonymize(e.to_string()))?;

        Ok(Verdict::Redacted {
            text: result.text,
            count: result.items.len(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{Engine, Verdict};
    use crate::profile::Profile;

    fn engine(profile: Profile) -> Engine {
        Engine::new(profile, "test-salt").expect("engine should build")
    }

    #[test]
    fn clean_text_is_untouched() {
        let e = engine(Profile::coding_assistant());
        let v = e.scan("fn main() { println!(\"hello\"); }").expect("scan");
        assert_eq!(v, Verdict::Clean);
    }

    #[test]
    fn empty_text_is_clean() {
        let e = engine(Profile::coding_assistant());
        assert_eq!(e.scan("").expect("scan"), Verdict::Clean);
    }

    #[test]
    fn email_is_redacted() {
        let e = engine(Profile::coding_assistant());
        let v = e.scan("mail me at jane@example.com please").expect("scan");
        match v {
            Verdict::Redacted { text, count } => {
                assert!(!text.contains("jane@example.com"), "email survived: {text}");
                assert_eq!(count, 1);
            }
            other => panic!("expected redaction, got {other:?}"),
        }
    }

    // ── The negative cases. These are the tests that would catch a weakened
    //    policy, so they are the point of this module. ──────────────────────

    #[test]
    fn github_token_blocks_the_request() {
        let e = engine(Profile::coding_assistant());
        let v = e
            .scan("use token ghp_abcdefghijklmnopqrstuvwxyz0123456789 to auth")
            .expect("scan");
        assert!(v.is_blocked(), "a GitHub token must block, got {v:?}");
    }

    #[test]
    fn aws_access_key_blocks_the_request() {
        let e = engine(Profile::coding_assistant());
        let v = e.scan("AKIAIOSFODNN7EXAMPLE").expect("scan");
        assert!(v.is_blocked(), "an AWS key must block, got {v:?}");
    }

    /// Regression: the `coding-assistant` profile once set a 0.7 threshold on
    /// `Phone`, but upstream's phone recognizer emits a *fixed* score of 0.6,
    /// so the detector was switched off rather than tightened and no phone
    /// number was ever redacted. Nothing caught it because the profile tests
    /// only assert `action_for`, which still correctly returned `Replace` —
    /// the threshold kills the detection before the action is ever consulted.
    /// Assert the behaviour, not the policy.
    #[test]
    fn phone_number_is_actually_redacted() {
        let e = engine(Profile::coding_assistant());
        let v = e.scan("call me on +1-415-555-0142 tomorrow").expect("scan");
        match v {
            Verdict::Redacted { text, .. } => {
                assert!(!text.contains("555-0142"), "phone survived: {text}");
            }
            other => panic!("expected the phone number to be redacted, got {other:?}"),
        }
    }

    #[test]
    fn private_key_header_blocks_the_request() {
        let e = engine(Profile::coding_assistant());
        let v = e
            .scan("-----BEGIN RSA PRIVATE KEY-----\nMIIEow...")
            .expect("scan");
        assert!(v.is_blocked(), "a private key must block, got {v:?}");
    }

    #[test]
    fn blocked_verdict_never_carries_the_secret_value() {
        let e = engine(Profile::coding_assistant());
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";
        let v = e.scan(&format!("token {secret}")).expect("scan");
        match v {
            Verdict::Blocked { entities } => {
                for entity in &entities {
                    assert!(
                        !entity.contains(secret),
                        "the secret leaked into the verdict: {entity}"
                    );
                }
            }
            other => panic!("expected block, got {other:?}"),
        }
    }

    #[test]
    fn secrets_only_profile_passes_email_but_blocks_keys() {
        let e = engine(Profile::secrets_only());
        assert_eq!(
            e.scan("jane@example.com").expect("scan"),
            Verdict::Clean,
            "secrets-only must not touch personal data"
        );
        assert!(
            e.scan("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
                .expect("scan")
                .is_blocked()
        );
    }

    #[test]
    fn observe_only_changes_nothing_even_for_secrets() {
        let e = engine(Profile::observe_only());
        let v = e
            .scan("ghp_abcdefghijklmnopqrstuvwxyz0123456789")
            .expect("scan");
        assert_eq!(v, Verdict::Clean, "observe-only must not modify or block");
    }

    #[test]
    fn code_identifiers_are_not_mangled() {
        // The false-positive case that decides whether anyone leaves this
        // service enabled: ordinary source code must survive intact.
        let e = engine(Profile::coding_assistant());
        let code = "use serde_json::Value; // see https://docs.rs/serde_json";
        assert_eq!(
            e.scan(code).expect("scan"),
            Verdict::Clean,
            "code with a crate name and a docs URL must pass untouched"
        );
    }
}
