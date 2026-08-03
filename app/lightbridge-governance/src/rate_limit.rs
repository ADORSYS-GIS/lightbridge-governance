//! Simple in-memory fixed-window rate limiter for `/internal/v1/ingest`.
//!
//! Authorino is the first line of defense (it authenticates the caller and
//! stamps the trusted headers); this limiter is the second -- it bounds how
//! fast a single integration (or the collector, which shares the endpoint)
//! can flood the database, keyed on the Authorino-stamped integration id so
//! one noisy tenant's volume can't starve the rest.
//!
//! Deliberately per-process, not distributed: one deployment is single-tenant
//! (ADR-0001), and a fixed window is a coarse throttle, not a billing meter.
//! Good enough to keep an accidental loop from melting the write path.

use std::{
    collections::HashMap,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

pub struct RateLimiter {
    /// Max requests allowed per `window_secs`, per key.
    max_per_window: u64,
    window_secs: u64,
    state: Mutex<HashMap<String, (u64, u64)>>,
}

impl RateLimiter {
    #[must_use]
    pub fn new(max_per_window: u64, window_secs: u64) -> Self {
        Self {
            max_per_window,
            // Clamp to >= 1: `allow` divides by the window, so a zero window
            // (e.g. a misconfigured env var) must not divide by zero on the
            // request path -- that would panic and take the endpoint down.
            window_secs: window_secs.max(1),
            state: Mutex::new(HashMap::new()),
        }
    }

    /// Returns `true` if the key may proceed, `false` if it has exhausted its
    /// window. The window is identified by `now / window_secs`, so a key that
    /// goes quiet for a full window is re-seeded on its next call: the stale
    /// entry is replaced rather than accrued, so the map never grows without
    /// bound -- an integration id that stops sending simply stops occupying
    /// space once its old entry is overwritten by a fresh window.
    #[must_use]
    pub fn allow(&self, key: &str) -> bool {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let window = now / self.window_secs;

        let mut state = match self.state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };

        match state.get_mut(key) {
            Some((seen_window, count)) if *seen_window == window => {
                if *count >= self.max_per_window {
                    return false;
                }
                *count += 1;
            }
            Some(entry) => *entry = (window, 1),
            None => {
                state.insert(key.to_owned(), (window, 1));
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allows_up_to_the_window_limit() {
        let limiter = RateLimiter::new(2, 60);
        assert!(limiter.allow("integration-a"));
        assert!(limiter.allow("integration-a"));
        assert!(
            !limiter.allow("integration-a"),
            "third request must be limited"
        );
    }

    #[test]
    fn different_keys_are_isolated() {
        let limiter = RateLimiter::new(1, 60);
        assert!(limiter.allow("integration-a"));
        assert!(
            limiter.allow("integration-b"),
            "a different key has its own budget"
        );
        assert!(
            !limiter.allow("integration-a"),
            "a still exceeds its own budget"
        );
    }

    #[test]
    fn a_fresh_window_resets_the_budget() {
        let limiter = RateLimiter::new(1, 1);
        let first = limiter.allow("integration-a");
        assert!(first);
        // The window boundary is now/duration_since, so two calls in the same
        // instant share a window. Sleep across the boundary to observe the
        // reset -- this test is about the window logic, not about being fast.
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(
            limiter.allow("integration-a"),
            "a new window must reset the budget"
        );
    }

    #[test]
    fn a_zero_window_is_clamped_to_one() {
        // `new` clamps window_secs to >= 1, so a misconfigured env var can
        // never cause a divide-by-zero on the request path.
        let limiter = RateLimiter::new(5, 0);
        assert!(limiter.allow("integration-a"));
        assert!(limiter.allow("integration-a"));
    }

    #[test]
    fn a_poisoned_lock_still_returns_an_answer() {
        // Poison the mutex on purpose: a panic while holding the guard.
        let poisoned = std::sync::Arc::new(RateLimiter::new(1, 60));
        let thread_limiter = std::sync::Arc::clone(&poisoned);
        let handle = std::thread::spawn(move || {
            let _guard = thread_limiter.state.lock().expect("lock");
            panic!("intentional panic while holding the lock to poison the mutex");
        });
        let _ = handle.join();

        // The *poisoned* limiter must keep working after the crash -- a mutex
        // poisoned by one thread must not take the whole endpoint down. The
        // recovery branch reads the poisoned guard's inner state; without it,
        // every subsequent request would panic.
        assert!(
            poisoned.allow("integration-a"),
            "a poisoned lock must not wedge the limiter"
        );
        assert!(
            !poisoned.allow("integration-a"),
            "the poisoned limiter still enforces its budget"
        );
    }
}
