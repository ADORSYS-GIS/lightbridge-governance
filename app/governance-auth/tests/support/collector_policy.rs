//! What the mock collector answers with, and the decision that turns one of
//! these into a status code.
//!
//! Split out of [`super::mock_collector`] so both halves stay under the
//! 200-LoC gate; the variants carry most of the prose because *why* a shape
//! exists is the part a later reader needs.

/// Held behind a mutex in [`super::mock_collector::MockCollector`] so a test
/// can change it *between* two `copilot-push` runs. That is the only way to
/// reproduce a transport that refuses a record on one wake and takes it on the
/// next -- the case a drain must not answer by discarding the record.
#[derive(Clone, Copy)]
pub enum Behavior {
    Accept,
    /// Accept, but hold the request open first. Concurrency bugs in the drain
    /// live in the window between reading the checkpoint and writing it back,
    /// and that window is dominated by the POST -- against an instant mock it
    /// is too narrow to hit reliably, so the race passes by luck. This makes
    /// it wide enough to be a test rather than a coin flip.
    AcceptSlowly {
        millis: u64,
    },
    /// Reject everything -- used to prove the checkpoint does not advance
    /// past a batch the collector never took.
    Reject(u16),
    /// Reject one signal path and accept the other. Real split deployments
    /// exist (a metrics backend and a log backend behind one gateway), and
    /// without this variant a "metrics accepted, logs rejected" run is
    /// unreachable from a test: `Reject` fails metrics first, so the partial
    /// case is never exercised.
    RejectPath {
        path: &'static str,
        status: u16,
    },
    /// Reject any payload whose body contains `needle`, accept everything
    /// else. This is what a validating collector does to ONE bad record in an
    /// otherwise fine batch -- the shape that turns into a permanent poison
    /// pill if the drain can neither split nor advance past it.
    RejectContaining {
        needle: &'static str,
        status: u16,
    },
    /// [`Self::RejectContaining`], but every answer is held open first. The
    /// two have to be one variant rather than composed, because the test that
    /// needs both -- killing a wake part way through a bisect -- needs the
    /// bisect (which only a refusal produces) *and* a window wide enough to
    /// land a kill inside it. Against an instant mock the whole wake is over
    /// in single-digit milliseconds and the kill is a coin flip.
    RejectContainingSlowly {
        needle: &'static str,
        status: u16,
        millis: u64,
    },
}

impl Behavior {
    /// `None` = 200. `path` and `body` are the request being answered.
    pub fn verdict(self, path: &str, body: &str) -> Option<u16> {
        match self {
            Self::Accept | Self::AcceptSlowly { .. } => None,
            Self::Reject(status) => Some(status),
            Self::RejectPath {
                path: rejected,
                status,
            } => (path == rejected).then_some(status),
            Self::RejectContaining { needle, status }
            | Self::RejectContainingSlowly { needle, status, .. } => {
                body.contains(needle).then_some(status)
            }
        }
    }

    pub fn delay_millis(self) -> u64 {
        match self {
            Self::AcceptSlowly { millis } | Self::RejectContainingSlowly { millis, .. } => millis,
            _ => 0,
        }
    }
}
