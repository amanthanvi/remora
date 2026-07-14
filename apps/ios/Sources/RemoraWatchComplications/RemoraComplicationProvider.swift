import Foundation
import WidgetKit

/// `AppIntentTimelineProvider` shared by all three complications. Resolves
/// the configured `ServerSelectionIntent.server`:
///
/// - `nil` → use the aggregate `complication.snapshot.v1` (legacy/default).
/// - non-nil → look up that server's slice in
///   `complication.per-server.v1` and fall back to the aggregate if the
///   selected server has no entry yet.
///
/// The legacy `TimelineProvider`-shape behavior (one entry now + 30 ticks
/// while running) is preserved unchanged.
struct RemoraComplicationProvider: AppIntentTimelineProvider {
    typealias Intent = ServerSelectionIntent
    typealias Entry = RemoraComplicationEntry

    func placeholder(in context: Context) -> RemoraComplicationEntry {
        .placeholder
    }

    func snapshot(for configuration: ServerSelectionIntent, in context: Context) async -> RemoraComplicationEntry {
        resolveCurrent(for: configuration)
    }

    func timeline(for configuration: ServerSelectionIntent, in context: Context) async -> Timeline<RemoraComplicationEntry> {
        let base = resolveCurrent(for: configuration)
        return makeTimeline(base: base)
    }

    func recommendations() -> [AppIntentRecommendation<ServerSelectionIntent>] {
        []
    }

    // MARK: - Resolution

    private func resolveCurrent(for configuration: ServerSelectionIntent) -> RemoraComplicationEntry {
        if let serverId = configuration.server?.id,
           let payload = perServerPayload(for: serverId) {
            return RemoraComplicationStore.entry(from: payload)
        }
        return RemoraComplicationStore.current()
    }

    private func perServerPayload(for serverId: String) -> RemoraComplicationPayload? {
        let map = RemoraPerServerComplicationStore.current()
        guard let data = map[serverId] else { return nil }
        return try? JSONDecoder().decode(RemoraComplicationPayload.self, from: data)
    }

    // MARK: - Timeline shape

    private func makeTimeline(base: RemoraComplicationEntry) -> Timeline<RemoraComplicationEntry> {
        let now = Date()
        var entries: [RemoraComplicationEntry] = []

        if base.mode == .running {
            // Tick once a minute for the next 30m. Each entry carries the same
            // start epoch so the view recomputes elapsed against `entry.date`.
            for step in 0..<30 {
                entries.append(
                    RemoraComplicationEntry(
                        date: now.addingTimeInterval(TimeInterval(step) * 60),
                        mode: .running,
                        lastTurnStartMsEpoch: base.lastTurnStartMsEpoch,
                        taskId: base.taskId,
                        progress: min(1, base.progress + Double(step) * 0.01),
                        title: base.title,
                        toolLine: base.toolLine,
                        serverCount: base.serverCount
                    )
                )
            }
            return Timeline(entries: entries, policy: .after(now.addingTimeInterval(60 * 30)))
        } else {
            entries.append(base)
            return Timeline(entries: entries, policy: .after(now.addingTimeInterval(60 * 15)))
        }
    }
}
