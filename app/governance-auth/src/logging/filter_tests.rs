//! The property `file_filter` has to hold: `LEVEL_ENV` moves `CRATE_TARGET`
//! and nothing else, no matter how high.
//!
//! See this module's parent doc, "`GOVERNANCE_AUTH_LOG` only raises OUR
//! level" -- a bare `GOVERNANCE_AUTH_LOG=trace` used to be a directive with
//! no target, which `EnvFilter` applies to every target, `h2`/`hyper`
//! included. Measured cost of that: ~40 KB of `h2`/`hyper`'s own output per
//! request, next to this crate's ~150-byte line for the same request.

use std::sync::{Arc, Mutex};

use tracing::{Level, level_filters::LevelFilter};
use tracing_subscriber::layer::{Context, Layer, SubscriberExt};

use super::file_filter;

/// Records every event's target that reaches it, so the test can inspect
/// what got through the filter without a real file or a real dependency.
#[derive(Clone, Default)]
struct Seen(Arc<Mutex<Vec<String>>>);

impl<S: tracing::Subscriber> Layer<S> for Seen {
    fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event.metadata().target().to_owned());
    }
}

/// Falsified by reverting `file_filter` to its old
/// `EnvFilter::parse_lossy(level.to_string())` form (a bare, untargeted
/// directive): the `h2`/`hyper` events below would then also reach `seen`,
/// and the first assertion would fail.
#[test]
fn a_dependencys_target_never_rises_above_info_no_matter_what_level_env_asks_for() {
    let seen = Seen::default();
    let subscriber = tracing_subscriber::registry()
        .with(seen.clone().with_filter(file_filter(LevelFilter::TRACE)));

    tracing::subscriber::with_default(subscriber, || {
        // Stand-ins for h2/hyper's own instrumentation -- same targets those
        // crates actually use, without needing a real connection to trigger
        // it.
        tracing::event!(target: "h2::codec::framed_write", Level::TRACE, "send frame");
        tracing::event!(target: "hyper::proto::h1::io", Level::DEBUG, "flushed 512 bytes");
        // Our own crate, at the same TRACE level the daemon actually logs at
        // (otel_daemon::mod's "received OTLP" line).
        tracing::event!(target: "governance_auth::otel_daemon", Level::TRACE, "received OTLP");
    });

    let seen = seen
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        !seen
            .iter()
            .any(|target| target.starts_with("h2") || target.starts_with("hyper")),
        "a dependency's own trace/debug event reached the file filter even \
         at LevelFilter::TRACE, got {seen:?}"
    );
    assert!(
        seen.iter()
            .any(|target| target.starts_with("governance_auth")),
        "our own crate's trace event should still pass through at trace, got {seen:?}"
    );
}

/// The companion claim: turning [`file_level`](super::file_level)'s result
/// down must still turn OUR OWN events down too -- this is a scope fix, not
/// a "never listen" fix.
#[test]
fn our_own_target_is_still_silenced_below_its_requested_level() {
    let seen = Seen::default();
    let subscriber = tracing_subscriber::registry()
        .with(seen.clone().with_filter(file_filter(LevelFilter::INFO)));

    tracing::subscriber::with_default(subscriber, || {
        tracing::event!(target: "governance_auth::otel_daemon", Level::TRACE, "received OTLP");
    });

    let seen = seen
        .0
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        seen.is_empty(),
        "a TRACE event on our own target must not pass an INFO file level, got {seen:?}"
    );
}
