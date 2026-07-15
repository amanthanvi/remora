use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Default)]
pub struct RelayMetrics {
    pub ingest_accepted: AtomicU64,
    pub ingest_replayed: AtomicU64,
    pub ingest_conflicts: AtomicU64,
    pub cursor_resets: AtomicU64,
    pub registrations_created: AtomicU64,
    pub registrations_tombstoned: AtomicU64,
    pub outbox_coalesced: AtomicU64,
    pub outbox_leases_recovered: AtomicU64,
    pub push_accepted: AtomicU64,
    pub push_retried: AtomicU64,
    pub push_invalid_tokens: AtomicU64,
    pub push_suppressed: AtomicU64,
    pub push_dead_lettered: AtomicU64,
    pub maintenance_expired_events: AtomicU64,
}

impl std::fmt::Debug for RelayMetrics {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RelayMetrics { aggregate_counters: [redacted] }")
    }
}

impl RelayMetrics {
    pub fn render_prometheus(&self) -> String {
        let metric = |name: &str, help: &str, value: &AtomicU64| {
            format!(
                "# HELP {name} {help}\n# TYPE {name} counter\n{name} {}\n",
                value.load(Ordering::Relaxed)
            )
        };

        [
            metric(
                "remora_relay_ingest_accepted_total",
                "New durable events accepted.",
                &self.ingest_accepted,
            ),
            metric(
                "remora_relay_ingest_replayed_total",
                "Exact idempotent event replays.",
                &self.ingest_replayed,
            ),
            metric(
                "remora_relay_ingest_conflicts_total",
                "Conflicting event-id reuse attempts.",
                &self.ingest_conflicts,
            ),
            metric(
                "remora_relay_cursor_resets_total",
                "Fetches requiring snapshot or host reconciliation.",
                &self.cursor_resets,
            ),
            metric(
                "remora_relay_registrations_created_total",
                "Push registrations created or rotated.",
                &self.registrations_created,
            ),
            metric(
                "remora_relay_registrations_tombstoned_total",
                "Push registrations durably tombstoned.",
                &self.registrations_tombstoned,
            ),
            metric(
                "remora_relay_outbox_coalesced_total",
                "Wake intents coalesced onto a newer cursor.",
                &self.outbox_coalesced,
            ),
            metric(
                "remora_relay_outbox_leases_recovered_total",
                "Expired delivery leases safely recovered.",
                &self.outbox_leases_recovered,
            ),
            metric(
                "remora_relay_push_accepted_total",
                "Wake hints accepted by providers.",
                &self.push_accepted,
            ),
            metric(
                "remora_relay_push_retried_total",
                "Wake hints rescheduled after transient provider outcomes.",
                &self.push_retried,
            ),
            metric(
                "remora_relay_push_invalid_tokens_total",
                "Provider tokens tombstoned after invalid-token outcomes.",
                &self.push_invalid_tokens,
            ),
            metric(
                "remora_relay_push_suppressed_total",
                "Wake hints suppressed in disabled mode.",
                &self.push_suppressed,
            ),
            metric(
                "remora_relay_push_dead_lettered_total",
                "Wake intents terminally failed without deleting events.",
                &self.push_dead_lettered,
            ),
            metric(
                "remora_relay_expired_events_total",
                "Expired events removed by retention maintenance.",
                &self.maintenance_expired_events,
            ),
        ]
        .concat()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_have_no_high_cardinality_labels() {
        let metrics = RelayMetrics::default();
        metrics.ingest_accepted.fetch_add(1, Ordering::Relaxed);
        let rendered = metrics.render_prometheus();
        assert!(rendered.contains("remora_relay_ingest_accepted_total 1"));
        assert!(!rendered.contains('{'));
    }
}
