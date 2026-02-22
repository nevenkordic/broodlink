/*
 * Broodlink - Multi-agent AI orchestration system
 * Copyright (C) 2025–2026 Neven Kordic <neven@broodlink.ai>
 * SPDX-License-Identifier: AGPL-3.0-or-later
 */

//! Shared runtime utilities for Broodlink services.
//!
//! Provides common building blocks that are duplicated across services:
//! - [`CircuitBreaker`]: failure tracking with half-open recovery
//! - [`shutdown_signal`]: graceful SIGINT/SIGTERM handler
//! - [`connect_nats`]: cluster-aware NATS connection

#![deny(clippy::unwrap_used)]
#![deny(clippy::expect_used)]
#![warn(clippy::pedantic)]

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::{error, info};

// ---------------------------------------------------------------------------
// Circuit breaker
// ---------------------------------------------------------------------------

/// Thread-safe circuit breaker with three states: CLOSED → OPEN → HALF-OPEN.
///
/// After `threshold` consecutive failures, the circuit opens and rejects all
/// calls for `half_open_secs`. After that window, one probe attempt is allowed
/// through (half-open). A success resets the breaker; another failure re-opens.
pub struct CircuitBreaker {
    name: String,
    failure_count: AtomicU32,
    last_failure_epoch_ms: AtomicU64,
    half_open_probe_in_flight: AtomicBool,
    threshold: u32,
    half_open_secs: u64,
}

impl CircuitBreaker {
    /// Create a new circuit breaker.
    ///
    /// - `name`: used in error messages
    /// - `threshold`: number of failures before opening
    /// - `half_open_secs`: seconds before allowing a probe
    #[must_use]
    pub fn new(name: &str, threshold: u32, half_open_secs: u64) -> Self {
        Self {
            name: name.to_string(),
            failure_count: AtomicU32::new(0),
            last_failure_epoch_ms: AtomicU64::new(0),
            half_open_probe_in_flight: AtomicBool::new(false),
            threshold,
            half_open_secs,
        }
    }

    /// Returns `true` if the circuit is open (all calls should be rejected).
    ///
    /// When the half-open window is reached, exactly one probe is allowed
    /// through via an atomic compare-exchange. All other concurrent callers
    /// still see the circuit as open until the probe succeeds or fails.
    #[must_use]
    pub fn is_open(&self) -> bool {
        let failures = self.failure_count.load(Ordering::Relaxed);
        if failures < self.threshold {
            return false;
        }
        let last_ms = self.last_failure_epoch_ms.load(Ordering::Relaxed);
        let now_ms = now_epoch_ms();
        let elapsed_secs = (now_ms.saturating_sub(last_ms)) / 1000;
        if elapsed_secs >= self.half_open_secs {
            // Half-open window: allow exactly one probe through
            if self
                .half_open_probe_in_flight
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return false; // this caller is the probe
            }
            // Another probe is already in flight — remain open
        }
        true
    }

    /// Record a successful operation — resets the failure counter and probe flag.
    pub fn record_success(&self) {
        self.failure_count.store(0, Ordering::Relaxed);
        self.half_open_probe_in_flight.store(false, Ordering::Release);
    }

    /// Record a failed operation — increments counter, updates timestamp,
    /// and resets the probe flag so the next half-open window can try again.
    pub fn record_failure(&self) {
        self.failure_count.fetch_add(1, Ordering::Relaxed);
        self.last_failure_epoch_ms
            .store(now_epoch_ms(), Ordering::Relaxed);
        self.half_open_probe_in_flight.store(false, Ordering::Release);
    }

    /// Returns `Ok(())` if the circuit is closed/half-open, or `Err(name)` if open.
    ///
    /// # Errors
    ///
    /// Returns the breaker name as a `String` when the circuit is open.
    pub fn check(&self) -> Result<(), String> {
        if self.is_open() {
            return Err(self.name.clone());
        }
        Ok(())
    }

    /// Name of this circuit breaker (for error messages).
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }
}

fn now_epoch_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

// ---------------------------------------------------------------------------
// Shutdown signal
// ---------------------------------------------------------------------------

/// Wait for SIGINT (ctrl-c) or SIGTERM, then return.
///
/// Use with `tokio::select!` or `axum::serve(...).with_graceful_shutdown(...)`.
pub async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .unwrap_or_else(|e| error!(error = %e, "ctrl-c handler failed"));
    };

    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut sig) => {
                sig.recv().await;
            }
            Err(e) => {
                error!(error = %e, "SIGTERM handler unavailable, relying on ctrl-c");
                std::future::pending::<()>().await;
            }
        }
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => info!("received ctrl-c"),
        () = terminate => info!("received SIGTERM"),
    }
}

// ---------------------------------------------------------------------------
// NATS connection (cluster-aware)
// ---------------------------------------------------------------------------

/// Connect to NATS, using cluster URLs if configured.
///
/// # Errors
///
/// Returns `async_nats::ConnectError` if the connection fails.
pub async fn connect_nats(
    config: &broodlink_config::NatsConfig,
) -> Result<async_nats::Client, async_nats::ConnectError> {
    let client = if config.cluster_urls.is_empty() {
        async_nats::connect(&config.url).await?
    } else {
        let mut addrs: Vec<String> = vec![config.url.clone()];
        addrs.extend(config.cluster_urls.clone());
        async_nats::connect(addrs.as_slice()).await?
    };
    info!(
        url = %config.url,
        cluster_size = config.cluster_urls.len(),
        "nats connected"
    );
    Ok(client)
}

// ===========================================================================
// Tests
// ===========================================================================

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_circuit_breaker_starts_closed() {
        let cb = CircuitBreaker::new("test", 3, 30);
        assert!(!cb.is_open());
        assert!(cb.check().is_ok());
    }

    #[test]
    fn test_circuit_breaker_opens_after_threshold() {
        let cb = CircuitBreaker::new("test", 3, 30);
        cb.record_failure();
        cb.record_failure();
        assert!(!cb.is_open(), "should still be closed at 2 failures");
        cb.record_failure();
        assert!(cb.is_open(), "should be open at 3 failures");
        assert!(cb.check().is_err());
    }

    #[test]
    fn test_circuit_breaker_resets_on_success() {
        let cb = CircuitBreaker::new("test", 2, 30);
        cb.record_failure();
        cb.record_failure();
        assert!(cb.is_open());
        cb.record_success();
        assert!(!cb.is_open());
        assert!(cb.check().is_ok());
    }

    #[test]
    fn test_circuit_breaker_half_open_after_timeout() {
        let cb = CircuitBreaker::new("test", 1, 0); // 0-second recovery
        cb.record_failure();
        // With 0-second recovery, should immediately be half-open
        assert!(!cb.is_open(), "should be half-open after recovery period");
    }

    #[test]
    fn test_circuit_breaker_check_returns_name() {
        let cb = CircuitBreaker::new("qdrant", 1, 9999);
        cb.record_failure();
        let err = cb.check().unwrap_err();
        assert_eq!(err, "qdrant");
    }
}
