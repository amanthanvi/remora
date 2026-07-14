import CoreFoundation
import Foundation

enum VoiceSessionControl {
    static let realtimeFeatureName = "realtime_conversation"
    static let defaultPrompt = "You are Codex in a live voice conversation inside Remora. Keep responses short, spoken, and conversational. Avoid markdown and code formatting unless explicitly asked."

    /// Build a voice prompt that includes awareness of available servers.
    static func buildPrompt(remoteServers: [(name: String, hostname: String)]) -> String {
        let serverList = remoteServers.isEmpty
            ? "- No additional connected servers"
            : remoteServers.map { "- \"\($0.name)\" (\($0.hostname))" }.joined(separator: "\n")
        return """
        \(defaultPrompt)

        Additional connected servers available for handoff:
        \(serverList)
        When using the codex tool for a handoff, specify one of those server names. \
        After a tool result, always give the user a short spoken summary of what you found.
        """
    }

    private static let appGroupSuite = RemoraPalette.appGroupSuite
    private static let endRequestKey = "voice_session.end_request_token"
    static let endRequestDarwinNotification = "com.remora.app.voice_session.end_request"

    static func requestEnd() {
        let token = UUID().uuidString
        UserDefaults(suiteName: appGroupSuite)?.set(token, forKey: endRequestKey)
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        let name = CFNotificationName(endRequestDarwinNotification as CFString)
        CFNotificationCenterPostNotification(center, name, nil, nil, true)
    }

    static func pendingEndRequestToken(after lastSeenToken: String?) -> String? {
        guard let token = UserDefaults(suiteName: appGroupSuite)?.string(forKey: endRequestKey),
              !token.isEmpty,
              token != lastSeenToken else {
            return nil
        }
        return token
    }
}
