import SwiftUI

struct CollaborationModeSelectorSheet: View {
    let presets: [AppCollaborationModePreset]
    let selectedMode: AppModeKind
    let isLoading: Bool
    let onSelect: (AppModeKind) -> Void

    var body: some View {
        NavigationStack {
            List {
                if isLoading && presets.isEmpty {
                    HStack(spacing: 10) {
                        ProgressView()
                        Text("Loading modes…")
                            .remoraFont(.body)
                            .foregroundStyle(RemoraTheme.textSecondary)
                    }
                    .listRowBackground(RemoraTheme.surface)
                }

                ForEach(presets, id: \.kind) { preset in
                    Button(action: { onSelect(preset.kind) }) {
                        HStack(spacing: 12) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(preset.name)
                                    .remoraFont(.body, weight: .semibold)
                                    .foregroundStyle(RemoraTheme.textPrimary)
                                if let reasoningEffort = preset.reasoningEffort {
                                    Text(collaborationModeEffortLabel(reasoningEffort))
                                        .remoraFont(.caption)
                                        .foregroundStyle(RemoraTheme.textSecondary)
                                }
                            }
                            Spacer()
                            if preset.kind == selectedMode {
                                Image(systemName: "checkmark.circle.fill")
                                    .foregroundStyle(RemoraTheme.accentForegroundOnSurface)
                            }
                        }
                    }
                    .buttonStyle(.plain)
                    .listRowBackground(RemoraTheme.surface)
                }
            }
            .scrollContentBackground(.hidden)
            .background(RemoraTheme.surface)
            .navigationTitle("Collaboration Mode")
        }
    }
}

private func collaborationModeEffortLabel(_ effort: ReasoningEffort) -> String {
    switch effort {
    case .none:
        return "None"
    case .minimal:
        return "Minimal"
    case .low:
        return "Low"
    case .medium:
        return "Medium"
    case .high:
        return "High"
    case .xHigh:
        return "XHigh"
    case .max:
        return "Max"
    }
}

enum ComposerSlashCommand: CaseIterable {
    case plan
    case model
    case permissions
    case experimental
    case skills
    case review
    case goal
    case rename
    case new
    case fork
    case resume

    var rawValue: String {
        switch self {
        case .plan: return "plan"
        case .model: return "model"
        case .permissions: return "permissions"
        case .experimental: return "experimental"
        case .skills: return "skills"
        case .review: return "review"
        case .goal: return "goal"
        case .rename: return "rename"
        case .new: return "new"
        case .fork: return "fork"
        case .resume: return "resume"
        }
    }

    var description: String {
        switch self {
        case .plan: return "switch collaboration mode"
        case .model: return "choose what model and reasoning effort to use"
        case .permissions: return "choose what Codex is allowed to do"
        case .experimental: return "toggle experimental features"
        case .skills: return "use skills to improve how Codex performs specific tasks"
        case .review: return "review my current changes and find issues"
        case .goal: return "set or manage the current thread goal"
        case .rename: return "rename the current thread"
        case .new: return "start a new chat during a conversation"
        case .fork: return "fork the current conversation into a new session"
        case .resume: return "resume a saved chat"
        }
    }

    init?(rawCommand: String) {
        switch rawCommand.lowercased() {
        case "plan", "mode", "collab": self = .plan
        case "model": self = .model
        case "permissions": self = .permissions
        case "experimental": self = .experimental
        case "skills": self = .skills
        case "review": self = .review
        case "goal": self = .goal
        case "rename": self = .rename
        case "new": self = .new
        case "fork": self = .fork
        case "resume": self = .resume
        default: return nil
        }
    }
}

enum ComposerApprovalOption: CaseIterable, Identifiable {
    case `default`
    case untrusted
    case onFailure
    case onRequest
    case never

    var id: String { wireValue }

    var title: String {
        switch self {
        case .default: return "Default"
        case .untrusted: return "Untrusted"
        case .onFailure: return "On failure"
        case .onRequest: return "On request"
        case .never: return "Never"
        }
    }

    var description: String {
        switch self {
        case .default: return "Use the thread or server default"
        case .untrusted: return "Always ask before taking action"
        case .onFailure: return "Ask only when a command fails"
        case .onRequest: return "Ask when escalation is requested"
        case .never: return "Run without asking for approval"
        }
    }

    var wireValue: String {
        switch self {
        case .default: return "inherit"
        case .untrusted: return "untrusted"
        case .onFailure: return "on-failure"
        case .onRequest: return "on-request"
        case .never: return "never"
        }
    }
}

enum ComposerSandboxOption: CaseIterable, Identifiable {
    case `default`
    case readOnly
    case workspaceWrite
    case fullAccess

    var id: String { wireValue }

    var title: String {
        switch self {
        case .default: return "Default"
        case .readOnly: return "Read only"
        case .workspaceWrite: return "Workspace write"
        case .fullAccess: return "Full access"
        }
    }

    var description: String {
        switch self {
        case .default: return "Use the thread or server default"
        case .readOnly: return "Can read files, but cannot edit them"
        case .workspaceWrite: return "Can edit files, but only in this workspace"
        case .fullAccess: return "Can edit files outside this workspace"
        }
    }

    var wireValue: String {
        switch self {
        case .default: return "inherit"
        case .readOnly: return "read-only"
        case .workspaceWrite: return "workspace-write"
        case .fullAccess: return "danger-full-access"
        }
    }
}

struct ComposerTokenRange: Equatable {
    let start: Int
    let end: Int
}

struct ComposerTokenContext: Equatable {
    let value: String
    let range: ComposerTokenRange
}

struct ComposerSlashQueryContext: Equatable {
    let query: String
    let range: ComposerTokenRange
}

func filterSlashCommands(_ query: String) -> [ComposerSlashCommand] {
    guard !query.isEmpty else { return Array(ComposerSlashCommand.allCases) }
    return ComposerSlashCommand.allCases
        .compactMap { command -> (ComposerSlashCommand, Int)? in
            guard let score = fuzzyScore(candidate: command.rawValue, query: query) else { return nil }
            return (command, score)
        }
        .sorted { lhs, rhs in
            if lhs.1 != rhs.1 {
                return lhs.1 > rhs.1
            }
            return lhs.0.rawValue < rhs.0.rawValue
        }
        .map(\.0)
}

func fuzzyScore(candidate: String, query: String) -> Int? {
    let normalizedCandidate = candidate.lowercased()
    let normalizedQuery = query.lowercased()

    if normalizedCandidate == normalizedQuery {
        return 1000
    }
    if normalizedCandidate.hasPrefix(normalizedQuery) {
        return 900 - (normalizedCandidate.count - normalizedQuery.count)
    }
    if normalizedCandidate.contains(normalizedQuery) {
        return 700 - (normalizedCandidate.count - normalizedQuery.count)
    }

    var score = 0
    var queryIndex = normalizedQuery.startIndex
    var candidateIndex = normalizedCandidate.startIndex

    while queryIndex < normalizedQuery.endIndex && candidateIndex < normalizedCandidate.endIndex {
        if normalizedQuery[queryIndex] == normalizedCandidate[candidateIndex] {
            score += 10
            queryIndex = normalizedQuery.index(after: queryIndex)
        }
        candidateIndex = normalizedCandidate.index(after: candidateIndex)
    }

    return queryIndex == normalizedQuery.endIndex ? score : nil
}

private let kDollarSign: UInt8 = 0x24
private let kUnderscore: UInt8 = 0x5F
private let kHyphen: UInt8 = 0x2D

private func isMentionNameByte(_ byte: UInt8) -> Bool {
    switch byte {
    case 0x61...0x7A, // a-z
        0x41...0x5A,  // A-Z
        0x30...0x39,  // 0-9
        kUnderscore,
        kHyphen:
        return true
    default:
        return false
    }
}

func isMentionQueryValid(_ query: String) -> Bool {
    guard !query.isEmpty else { return true }
    return query.utf8.allSatisfy(isMentionNameByte)
}

func extractMentionNames(_ text: String) -> [String] {
    let bytes = Array(text.utf8)
    guard !bytes.isEmpty else { return [] }

    var mentions: [String] = []
    var index = 0
    while index < bytes.count {
        guard bytes[index] == kDollarSign else {
            index += 1
            continue
        }

        if index > 0, isMentionNameByte(bytes[index - 1]) {
            index += 1
            continue
        }

        let nameStart = index + 1
        guard nameStart < bytes.count, isMentionNameByte(bytes[nameStart]) else {
            index += 1
            continue
        }

        var nameEnd = nameStart + 1
        while nameEnd < bytes.count, isMentionNameByte(bytes[nameEnd]) {
            nameEnd += 1
        }

        if let name = String(bytes: bytes[nameStart..<nameEnd], encoding: .utf8) {
            mentions.append(name)
        }
        index = nameEnd
    }

    return mentions
}

func currentPrefixedToken(
    text: String,
    cursor: Int,
    prefix: Character,
    allowEmpty: Bool
) -> ComposerTokenContext? {
    guard let tokenRange = tokenRangeAroundCursor(text: text, cursor: cursor) else { return nil }
    guard let tokenText = substring(text, within: tokenRange), tokenText.first == prefix else { return nil }
    let value = String(tokenText.dropFirst())
    if value.isEmpty && !allowEmpty {
        return nil
    }
    return ComposerTokenContext(value: value, range: tokenRange)
}

func currentSlashQueryContext(
    text: String,
    cursor: Int
) -> ComposerSlashQueryContext? {
    let safeCursor = max(0, min(cursor, text.count))
    let firstLineEnd = text.firstIndex(of: "\n").map { text.distance(from: text.startIndex, to: $0) } ?? text.count
    if safeCursor > firstLineEnd || firstLineEnd <= 0 {
        return nil
    }

    let firstLine = String(text.prefix(firstLineEnd))
    guard firstLine.hasPrefix("/") else { return nil }

    var commandEnd = 1
    let chars = Array(firstLine)
    while commandEnd < chars.count && !chars[commandEnd].isWhitespace {
        commandEnd += 1
    }
    if safeCursor > commandEnd {
        return nil
    }

    let query = commandEnd > 1 ? String(chars[1..<commandEnd]) : ""
    let rest = commandEnd < chars.count ? String(chars[commandEnd...]).trimmingCharacters(in: .whitespacesAndNewlines) : ""

    if query.isEmpty {
        if !rest.isEmpty {
            return nil
        }
    } else if query.contains("/") {
        return nil
    }

    return ComposerSlashQueryContext(query: query, range: ComposerTokenRange(start: 0, end: commandEnd))
}

private func tokenRangeAroundCursor(
    text: String,
    cursor: Int
) -> ComposerTokenRange? {
    guard !text.isEmpty else { return nil }

    let safeCursor = max(0, min(cursor, text.count))
    let chars = Array(text)

    if safeCursor < chars.count, chars[safeCursor].isWhitespace {
        var index = safeCursor
        while index < chars.count && chars[index].isWhitespace {
            index += 1
        }
        if index < chars.count {
            var end = index
            while end < chars.count && !chars[end].isWhitespace {
                end += 1
            }
            return ComposerTokenRange(start: index, end: end)
        }
    }

    var start = safeCursor
    while start > 0 && !chars[start - 1].isWhitespace {
        start -= 1
    }

    var end = safeCursor
    while end < chars.count && !chars[end].isWhitespace {
        end += 1
    }

    if end <= start {
        return nil
    }
    return ComposerTokenRange(start: start, end: end)
}

func replacingRange(
    in text: String,
    with range: ComposerTokenRange,
    replacement: String
) -> String? {
    guard range.start >= 0, range.end <= text.count, range.start <= range.end else { return nil }
    guard let lower = index(in: text, offset: range.start),
          let upper = index(in: text, offset: range.end) else { return nil }
    var copy = text
    copy.replaceSubrange(lower..<upper, with: replacement)
    return copy
}

private func substring(_ text: String, within range: ComposerTokenRange) -> String? {
    guard range.start >= 0, range.end <= text.count, range.start <= range.end else { return nil }
    guard let lower = index(in: text, offset: range.start),
          let upper = index(in: text, offset: range.end) else { return nil }
    return String(text[lower..<upper])
}

private func index(in text: String, offset: Int) -> String.Index? {
    guard offset >= 0, offset <= text.count else { return nil }
    return text.index(text.startIndex, offsetBy: offset)
}
