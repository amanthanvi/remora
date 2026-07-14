import Foundation

/// Convert filesystem paths to short, user-facing strings.
///
/// Delegates to the existing `abbreviateHomePath`
/// which shortens `/Users/<user>/<subpath>` and `/home/<user>/<subpath>`
/// to `~/<subpath>`.
enum PathDisplay {
    static func display(_ raw: String, isLocal _: Bool) -> String {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return trimmed }
        return remoteDisplay(trimmed)
    }

    /// Inverse of `display`. Accepts user-entered display strings (`~/foo`,
    /// `/tmp/x`, or remote `~\foo`) and produces an absolute path for the
    /// selected host.
    static func expand(_ display: String, isLocal _: Bool, remoteHome: String? = nil) -> String {
        let trimmed = display.trimmingCharacters(in: .whitespacesAndNewlines)
        return expandRemoteDisplay(trimmed, remoteHome: remoteHome)
    }

    private static func remoteDisplay(_ trimmed: String) -> String {
        let remote = RemotePath.parse(path: trimmed)
        guard remote.isWindows() else {
            return abbreviateHomePath(trimmed)
        }
        return abbreviateWindowsHome(remote) ?? remote.asString()
    }

    private static func expandRemoteDisplay(_ display: String, remoteHome: String?) -> String {
        guard let home = remoteHome?.trimmingCharacters(in: .whitespacesAndNewlines),
              !home.isEmpty else {
            return display
        }
        let remoteHomePath = RemotePath.parse(path: home)
        let normalizedHome = remoteHomePath.asString()
        if remoteHomePath.isWindows() {
            guard display == "~" || display.hasPrefix("~\\") || display.hasPrefix("~/") else {
                return display
            }
            if display == "~" { return normalizedHome }
            let suffix = String(display.dropFirst(2)).replacingOccurrences(of: "/", with: "\\")
            return appendWindowsSuffix(suffix, to: normalizedHome)
        }
        guard display == "~" || display.hasPrefix("~/") else {
            return display
        }
        if display == "~" { return normalizedHome }
        let suffix = String(display.dropFirst(2))
        return normalizedHome.hasSuffix("/") ? normalizedHome + suffix : normalizedHome + "/" + suffix
    }

    private static func appendWindowsSuffix(_ suffix: String, to home: String) -> String {
        let trimmedHome = home.hasSuffix("\\") ? String(home.dropLast()) : home
        guard !suffix.isEmpty else { return trimmedHome }
        return trimmedHome + "\\" + suffix
    }

    private static func abbreviateWindowsHome(_ remote: RemotePath) -> String? {
        let segments = remote.segments()
        guard segments.count >= 3 else { return nil }
        guard segments[1].label.caseInsensitiveCompare("Users") == .orderedSame else {
            return nil
        }
        let remainder = segments.dropFirst(3).map(\.label)
        guard !remainder.isEmpty else { return "~" }
        return "~\\" + remainder.joined(separator: "\\")
    }
}
