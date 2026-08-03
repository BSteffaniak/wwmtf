//! Secret-safe application observability counters and structured operational signals.

use std::sync::{
    LazyLock,
    atomic::{AtomicU64, Ordering},
};

/// Point-in-time application metrics suitable for a renderer/runtime exporter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppMetricsSnapshot {
    pub authentication_failures: u64,
    pub command_conflicts: u64,
    pub projection_rebuilds: u64,
    pub live_subscribers: u64,
    pub database_failures: u64,
}

#[derive(Debug, Default)]
struct AppMetrics {
    authentication_failures: AtomicU64,
    command_conflicts: AtomicU64,
    projection_rebuilds: AtomicU64,
    live_subscribers: AtomicU64,
    database_failures: AtomicU64,
}

static METRICS: LazyLock<AppMetrics> = LazyLock::new(AppMetrics::default);

pub fn record_authentication_failure(reason: &'static str) {
    METRICS
        .authentication_failures
        .fetch_add(1, Ordering::Relaxed);
    log::warn!(target: "wwmtf::auth", "authentication_failed reason={reason}");
}

pub fn record_command_conflict(expected: u64, actual: u64) {
    METRICS.command_conflicts.fetch_add(1, Ordering::Relaxed);
    log::warn!(
        target: "wwmtf::gameplay",
        "command_conflict expected_revision={expected} actual_revision={actual}"
    );
}

pub fn record_projection_rebuild(revision: u64) {
    METRICS.projection_rebuilds.fetch_add(1, Ordering::Relaxed);
    log::debug!(
        target: "wwmtf::projection",
        "projection_rebuilt revision={revision}"
    );
}

pub fn set_live_subscribers(count: usize) {
    let count = u64::try_from(count).unwrap_or(u64::MAX);
    METRICS.live_subscribers.store(count, Ordering::Relaxed);
    log::debug!(
        target: "wwmtf::live",
        "live_subscribers count={count}"
    );
}

pub fn record_database_failure(operation: &'static str) {
    METRICS.database_failures.fetch_add(1, Ordering::Relaxed);
    log::error!(
        target: "wwmtf::database",
        "database_operation_failed operation={operation}"
    );
}

/// Returns secret-free operational counters without resetting them.
#[must_use]
pub fn app_metrics_snapshot() -> AppMetricsSnapshot {
    AppMetricsSnapshot {
        authentication_failures: METRICS.authentication_failures.load(Ordering::Relaxed),
        command_conflicts: METRICS.command_conflicts.load(Ordering::Relaxed),
        projection_rebuilds: METRICS.projection_rebuilds.load(Ordering::Relaxed),
        live_subscribers: METRICS.live_subscribers.load(Ordering::Relaxed),
        database_failures: METRICS.database_failures.load(Ordering::Relaxed),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observability_api_accepts_only_safe_static_labels_and_numeric_aggregates() {
        fn assert_static(_: &'static str) {}
        assert_static("invalid_session");
        assert_static("resolve_session");

        let source = include_str!("observability.rs")
            .split("#[cfg(test)]")
            .next()
            .expect("production source exists");
        for forbidden in [
            "password",
            "session_token",
            "invitation_token",
            "rack",
            "bag",
            "canonical_event",
        ] {
            assert!(!source.contains(forbidden), "unsafe log field: {forbidden}");
        }
        assert!(!source.contains(&['{', ':', '?', '}'].iter().collect::<String>()));
        assert!(!source.contains("payload"));
    }

    #[test]
    fn snapshots_expose_only_aggregate_counters() {
        let before = app_metrics_snapshot();
        record_authentication_failure("invalid_session");
        record_command_conflict(2, 3);
        record_projection_rebuild(4);
        set_live_subscribers(5);
        record_database_failure("test_operation");
        let after = app_metrics_snapshot();

        assert_eq!(
            after.authentication_failures,
            before.authentication_failures + 1
        );
        assert_eq!(after.command_conflicts, before.command_conflicts + 1);
        assert_eq!(after.projection_rebuilds, before.projection_rebuilds + 1);
        assert_eq!(after.live_subscribers, 5);
        assert_eq!(after.database_failures, before.database_failures + 1);
    }
}
