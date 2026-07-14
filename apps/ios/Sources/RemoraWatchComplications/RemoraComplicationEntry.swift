import Foundation
import WidgetKit

/// Timeline entry shared by all three complications. Designed to round-trip
/// through the `group.com.remora.app` App Group — the iOS app writes
/// the current running-task snapshot into `UserDefaults` and complications
/// read it on each reload.
struct RemoraComplicationEntry: TimelineEntry {
    enum Mode: String, Codable {
        case idle, running, offline
    }

    let date: Date
    let mode: Mode
    /// Wall-clock epoch ms when the running task's current turn started.
    /// Used to compute live runtime against `entry.date` in the timeline.
    let lastTurnStartMsEpoch: Int64?
    /// Stable task identifier (e.g. "macbook-pro:t1") used for `widgetURL` deep links.
    let taskId: String?
    /// Progress [0, 1] of the running task — used for circular arc + pips.
    let progress: Double
    /// Short human label shown on rectangular / corner faces.
    let title: String
    /// Current tool-call line, shown in the rectangular family only.
    let toolLine: String
    /// Count of connected servers (idle mode only).
    let serverCount: Int

    /// Runtime label as `m:ss` (or `mm:ss` past 10 minutes), capped at 99:59.
    func runtimeLabel(at now: Date) -> String {
        guard let startMs = lastTurnStartMsEpoch else { return "0:00" }
        let startSeconds = TimeInterval(startMs) / 1000.0
        let elapsed = max(0, Int(now.timeIntervalSince1970 - startSeconds))
        let capped = min(elapsed, 99 * 60 + 59)
        let m = capped / 60
        let s = capped % 60
        return String(format: "%d:%02d", m, s)
    }

    static let placeholder = RemoraComplicationEntry(
        date: .now,
        mode: .running,
        lastTurnStartMsEpoch: Int64((Date.now.timeIntervalSince1970 - 42) * 1000),
        taskId: "preview:t1",
        progress: 0.4,
        title: "fix auth token expiry",
        toolLine: "edit_file src/auth.go",
        serverCount: 3
    )

    static let idlePlaceholder = RemoraComplicationEntry(
        date: .now,
        mode: .idle,
        lastTurnStartMsEpoch: nil,
        taskId: nil,
        progress: 1,
        title: "3 servers ready",
        toolLine: "tap to open",
        serverCount: 3
    )
}

/// Wire-format payload shared between the iOS writer
/// (`WatchCompanionBridge.currentComplicationSnapshot`) and the
/// complication readers. Kept Codable so the aggregate and per-server
/// slices use the exact same decoder.
struct RemoraComplicationPayload: Codable, Equatable {
    let mode: RemoraComplicationEntry.Mode
    let lastTurnStartMsEpoch: Int64?
    let taskId: String?
    let progress: Double
    let title: String
    let toolLine: String
    let serverCount: Int
}

/// Reads complication data out of the shared App Group.
enum RemoraComplicationStore {
    static let appGroup = "group.com.remora.app"
    private static let key = "complication.snapshot.v1"

    static func current() -> RemoraComplicationEntry {
        guard let payload = currentPayload() else { return .placeholder }
        return entry(from: payload)
    }

    static func currentPayload() -> RemoraComplicationPayload? {
        guard
            let defaults = UserDefaults(suiteName: appGroup),
            let data = defaults.data(forKey: key)
        else { return nil }
        return try? JSONDecoder().decode(RemoraComplicationPayload.self, from: data)
    }

    static func entry(from payload: RemoraComplicationPayload, at date: Date = .now) -> RemoraComplicationEntry {
        RemoraComplicationEntry(
            date: date,
            mode: payload.mode,
            lastTurnStartMsEpoch: payload.lastTurnStartMsEpoch,
            taskId: payload.taskId,
            progress: payload.progress,
            title: payload.title,
            toolLine: payload.toolLine,
            serverCount: payload.serverCount
        )
    }

    /// Write a snapshot from the iOS container app. Called opportunistically
    /// on task start/step change/task end.
    static func write(_ entry: RemoraComplicationEntry) {
        guard let defaults = UserDefaults(suiteName: appGroup) else { return }
        let payload = RemoraComplicationPayload(
            mode: entry.mode,
            lastTurnStartMsEpoch: entry.lastTurnStartMsEpoch,
            taskId: entry.taskId,
            progress: entry.progress,
            title: entry.title,
            toolLine: entry.toolLine,
            serverCount: entry.serverCount
        )
        guard let data = try? JSONEncoder().encode(payload) else { return }
        defaults.set(data, forKey: key)
    }
}
