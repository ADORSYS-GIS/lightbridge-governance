//! The scoped halves of the tree: `copilot …`, `otel …`, `self …`.
//!
//! Split from [`super`] so neither file has to choose between fitting the
//! 200-line ceiling and carrying the help text that makes `--help` worth
//! reading. Each enum here is one scope's verbs and nothing else; the reason
//! these three are scopes and the rest of the tree is not lives in [`super`]'s
//! module doc, next to the decision.

use clap::Subcommand;

/// `governance-auth copilot …` -- the VS Code Copilot Chat telemetry path.
#[derive(Debug, Subcommand)]
pub enum CopilotCommand {
    /// Drain Copilot Chat's OTel spool file and export it to the collector
    /// over OTLP/HTTP.
    ///
    /// Fails closed: with no valid session it exits non-zero WITHOUT reading
    /// the spool, advancing the checkpoint, or discarding anything. Runs on
    /// the five-minute schedule `configure` installs; run it by hand to see
    /// why a wake is failing.
    Push {
        /// Parse and report what would be sent, then stop. Posts nothing and
        /// leaves the checkpoint alone, but still requires a valid session.
        //
        // There is deliberately no offline path that reads the spool -- see
        // `crate::copilot`'s module doc for why.
        #[arg(long)]
        dry_run: bool,
    },
}

/// `governance-auth otel …` -- what this binary emits for OTLP export.
#[derive(Debug, Subcommand)]
pub enum OtelCommand {
    /// Print OTLP export headers as a JSON object -- the format Claude Code's
    /// `otelHeadersHelper` requires.
    ///
    /// Same refresh-or-fail-closed behaviour as `token`; this is that token
    /// wrapped in the shape the hook expects, so telemetry auth refreshes
    /// automatically instead of depending on anyone rotating a long-lived key
    /// by hand. stdout carries the JSON object and nothing else, ever.
    Headers,
}

/// `governance-auth serve …` -- the long-running telemetry daemon.
///
/// One verb for now (#268's receiver); sibling stories (#S3 service install,
/// #S4 health reporting) add to this scope.
#[derive(Debug, Subcommand)]
pub enum ServeCommand {
    /// Receive OTLP on loopback and forward it to the governed collector.
    ///
    /// Listens on the fixed loopback port (ADR-0016), accepts OTLP/HTTP on
    /// any path, mints a fresh bearer per forward through the same `oauth`
    /// path `token` uses, and never hands a credential to a client. Fails
    /// closed: a refused mint or an unreachable collector means the bytes are
    /// retained in memory, never forwarded unauthenticated and never dropped.
    Otel,
}

/// `governance-auth self …` -- this binary acting on itself.
#[derive(Debug, Subcommand)]
pub enum SelfCommand {
    /// Replace this binary with the latest GitHub release for this platform.
    ///
    /// The download is checksummed against the release's own `.sha256`, which
    /// catches corruption but is NOT a signature.
    ///
    /// Reads no OAuth configuration at all, so it works on a machine that has
    /// none yet -- which is exactly the machine most likely to be updating.
    //
    // `crate::update`'s module doc carries the trust model, and why this says
    // "checksummed" rather than "verified".
    Update {
        /// Report whether an update exists and exit, changing nothing.
        //
        // Named for the same thing `copilot push --dry-run` is named for: one
        // word across this CLI for "tell me what you would do, do nothing". It
        // used to be `--check` here and `--dry-run` there.
        #[arg(long)]
        dry_run: bool,
    },
}
