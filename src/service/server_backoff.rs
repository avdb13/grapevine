use std::{
    collections::HashMap,
    ops::Range,
    sync::{Arc, RwLock},
    time::{Duration, Instant},
};

use rand::{thread_rng, Rng};
use ruma::{OwnedServerName, ServerName};
use tracing::debug;

use crate::{Error, Result};

/// Service to handle backing off requests to offline servers.
///
/// Matrix is full of servers that are either temporarily or permanently
/// offline. It's important not to flood offline servers with federation
/// traffic, since this can consume resources on both ends.
///
/// To limit traffic to offline servers, we track a global exponential backoff
/// state for federation requests to each server name. This mechanism is *only*
/// intended to handle offline servers. Rate limiting and backoff retries for
/// specific requests have different considerations and need to be handled
/// elsewhere.
///
/// Exponential backoff is typically used in a retry loop for a single request.
/// Because the state of this backoff is global, and requests may be issued
/// concurrently, we do a couple of unusual things:
///
/// First, we wait for a certain number of consecutive failed requests before we
/// start delaying further requests. This is to avoid delaying requests to a
/// server that is not offline but fails on a small fraction of requests.
///
/// Second, we only increment the failure counter once for every batch of
/// concurrent requests, instead of on every failed request. This avoids rapidly
/// increasing the counter, proportional to the rate of outgoing requests, when
/// the server is only briefly offline.
pub(crate) struct Service {
    servers: RwLock<HashMap<OwnedServerName, Arc<RwLock<BackoffState>>>>,
}

// After the first 5 consecutive failed requests, increase delay exponentially
// from 5s to 24h over the next 24 failures.

// TODO: consider making these configurable
// TODO: are these reasonable parameters? The 24h max delay was pulled from the
// previous backoff logic for device keys, but it seems quite high to me.

/// Minimum number of consecutive failures for a server before starting to delay
/// requests.
const FAILURE_THRESHOLD: u8 = 5;
/// Initial delay between requests after the number of consecutive failures
/// to a server first exceeds [`FAILURE_THRESHOLD`].
const BASE_DELAY: Duration = Duration::from_secs(5);
/// Factor to increase delay by after each additional consecutive failure.
const MULTIPLIER: f64 = 1.5;
/// Maximum delay between requests to a server.
const MAX_DELAY: Duration = Duration::from_secs(60 * 60 * 24);
/// Range of random multipliers to request delay.
const JITTER_RANGE: Range<f64> = 0.5..1.5;

/// Guard to record the result of an attempted request to a server.
///
/// TODO: consider a `BackoffRequestResult` enum, and a single `record_result`
/// function.
///
/// If the request succeeds, call [`BackoffGuard::success`]. If the request
/// fails in a way that indicates the server is unavailble, call
/// [`BackoffGuard::hard_failure`]. If the request fails in a way that doesn't
/// necessarily indicate that the server is unavailable, call
/// [`BackffGuard::soft_failure`]. Note
#[must_use]
pub(crate) struct BackoffGuard {
    backoff: Arc<RwLock<BackoffState>>,
    /// Store the last failure timestamp observed when this request started. If
    /// there was another failure recorded since the request started, do not
    /// increment the failure count. This ensures that only one failure will
    /// be recorded for every batch of concurrent requests, as discussed in
    /// the doccumentation of [`Service`].
    last_failure: Option<Instant>,
}

/// State of exponential backoff for a specific server.
#[derive(Copy, Clone, Debug, Default)]
struct BackoffState {
    /// Count of consecutive failed requests to this server.
    failure_count: u8,
    /// Timestamp of the last failed request to this server.
    last_failure: Option<Instant>,
    /// Random multiplier to request delay.
    ///
    /// This is updated to a new random value after each batch of concurrent
    /// requests containing a failure.
    jitter_coeff: f64,
}

impl Service {
    pub(crate) fn build() -> Arc<Service> {
        Arc::new(Service {
            servers: RwLock::default(),
        })
    }

    /// If ready to attempt another request to a server, returns a guard to
    /// record the result.
    ///
    /// If still in the backoff period for this server, returns `Err`.
    pub(crate) fn server_ready(
        &self,
        server_name: &ServerName,
    ) -> Result<BackoffGuard> {
        let state = self.server_state(server_name);

        let last_failure = {
            let state_lock = state.read().unwrap();

            if let Some(remaining_delay) = state_lock.remaining_delay() {
                debug!(failures = %state_lock.failure_count, ?remaining_delay, "backing off from server");
                return Err(Error::BadServerResponse(
                    "too many errors from server, backing off",
                ));
            }

            state_lock.last_failure
        };

        Ok(BackoffGuard {
            backoff: state,
            last_failure,
        })
    }

    fn server_state(
        &self,
        server_name: &ServerName,
    ) -> Arc<RwLock<BackoffState>> {
        if let Some(state) = self.servers.read().unwrap().get(server_name) {
            Arc::clone(state)
        } else {
            let state = Arc::new(RwLock::new(BackoffState::default()));
            self.servers
                .write()
                .unwrap()
                .insert(server_name.to_owned(), Arc::clone(&state));
            state
        }
    }
}

impl BackoffState {
    /// Returns the remaining time before ready to attempt another request to
    /// this server.
    fn remaining_delay(&self) -> Option<Duration> {
        if let Some(last_failure) = self.last_failure {
            if self.failure_count > FAILURE_THRESHOLD {
                let excess_failure_count =
                    self.failure_count - FAILURE_THRESHOLD;
                // Converting to float is fine because we don't expect max_delay
                // to be large enough that the loss of precision matters. The
                // largest typical value is 24h, with a precision of 0.01ns.
                let base_delay_secs = BASE_DELAY.as_secs_f64();
                let max_delay_secs = MAX_DELAY.as_secs_f64();
                let delay_secs = max_delay_secs.max(
                    base_delay_secs
                        * MULTIPLIER.powi(i32::from(excess_failure_count)),
                ) * self.jitter_coeff;
                let delay = Duration::from_secs_f64(delay_secs);
                delay.checked_sub(last_failure.elapsed())
            } else {
                None
            }
        } else {
            None
        }
    }
}

impl BackoffGuard {
    /// Record a successful request.
    pub(crate) fn success(self) {
        self.backoff.write().unwrap().failure_count = 0;
    }

    /// Record a failed request indicating that the server may be unavailable.
    ///
    /// Examples of failures in this category are a timeout, a 500 status, or
    /// a 404 from an endpoint that is not specced to return 404.
    pub(crate) fn hard_failure(self) {
        let mut state = self.backoff.write().unwrap();
        state.last_failure = Some(Instant::now());
        if state.last_failure == self.last_failure {
            state.failure_count = state.failure_count.saturating_add(1);
            state.jitter_coeff = thread_rng().gen_range(JITTER_RANGE);
        }
    }

    /// Record a request that failed, but where the failure is likely to occur
    /// in normal operation even if the server is not unavailable.
    ///
    /// An example of a failure in this category is 404 from querying a user
    /// profile. This might occur if the server no longer exists, but will also
    /// occur if the userid doesn't exist.
    // Taking `self` here is intentional, to allow callers to destroy the guard
    // without triggering the `must_use` warning.
    #[allow(clippy::unused_self)]
    pub(crate) fn soft_failure(self) {}
}
