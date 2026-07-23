import Foundation
import OSLog

enum LLog {
    enum RenderingPolicy {
        case diagnostic
        case release
    }

    private static let subsystemRoot = Bundle.main.bundleIdentifier ?? "com.remora.app"
    private static let safeIdentifierCharacters = CharacterSet(
        charactersIn: "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789._-"
    )
    private nonisolated(unsafe) static var bootstrapped = false

    static func bootstrap() {
        guard !bootstrapped else { return }
        bootstrapped = true

        let codexHome = resolveCodexHome()
        setenv("CODEX_HOME", codexHome.path, 1)
    }

    static func trace(_ subsystem: String, _ message: String, fields: [String: Any] = [:], payloadJson: String? = nil) {
        emit(level: .debug, subsystem: subsystem, message: message, fields: fields, payloadJson: payloadJson)
    }

    static func debug(_ subsystem: String, _ message: String, fields: [String: Any] = [:], payloadJson: String? = nil) {
        emit(level: .debug, subsystem: subsystem, message: message, fields: fields, payloadJson: payloadJson)
    }

    static func info(_ subsystem: String, _ message: String, fields: [String: Any] = [:], payloadJson: String? = nil) {
        emit(level: .info, subsystem: subsystem, message: message, fields: fields, payloadJson: payloadJson)
    }

    static func warn(_ subsystem: String, _ message: String, fields: [String: Any] = [:], payloadJson: String? = nil) {
        emit(level: .default, subsystem: subsystem, message: message, fields: fields, payloadJson: payloadJson)
    }

    static func error(_ subsystem: String, _ message: String, error: Error? = nil, fields: [String: Any] = [:], payloadJson: String? = nil) {
        var allFields = fields
        if let error {
            let nsError = error as NSError
            allFields["error_domain"] = nsError.domain
            allFields["error_code"] = nsError.code
            #if DEBUG
            allFields["error_description"] = error.localizedDescription
            #endif
        }
        emit(level: .error, subsystem: subsystem, message: message, fields: allFields, payloadJson: payloadJson)
    }

    /// Production-safe error metadata for operational failures. Keep raw
    /// localized descriptions out of public OSLog output.
    static func operationalFailureFields(operation: String, error: Error) -> [String: Any] {
        let nsError = error as NSError
        var fields: [String: Any] = [
            "operation": operation,
            "error_domain": nsError.domain,
            "error_code": nsError.code,
        ]
        #if DEBUG
        fields["error_description"] = error.localizedDescription
        #endif
        return fields
    }

    private static func emit(level: OSLogType, subsystem: String, message: String, fields: [String: Any], payloadJson: String?) {
        let logger = Logger(subsystem: subsystemRoot, category: subsystem)
        #if DEBUG
        let rendered = render(
            message: message,
            fields: fields,
            payloadJson: payloadJson,
            policy: .diagnostic
        )
        mirrorToStderr(level: level, subsystem: subsystem, rendered: rendered)
        #else
        let rendered = render(
            message: message,
            fields: fields,
            payloadJson: payloadJson,
            policy: .release
        )
        #endif

        switch level {
        case .debug:
            logger.debug("\(rendered, privacy: .public)")
        case .info:
            logger.info("\(rendered, privacy: .public)")
        case .error, .fault:
            logger.error("\(rendered, privacy: .public)")
        default:
            logger.log(level: level, "\(rendered, privacy: .public)")
        }
    }

    #if DEBUG
    private static func mirrorToStderr(level: OSLogType, subsystem: String, rendered: String) {
        let levelName: String = switch level {
        case .debug:
            "DEBUG"
        case .info:
            "INFO"
        case .error:
            "ERROR"
        case .fault:
            "FAULT"
        default:
            "LOG"
        }
        fputs("[LLog][\(levelName)][\(subsystem)] \(rendered)\n", stderr)
    }
    #endif

    static func render(
        message: String,
        fields: [String: Any],
        payloadJson: String?,
        policy: RenderingPolicy
    ) -> String {
        var parts = [message]
        let renderedFields = switch policy {
        case .diagnostic:
            fields
        case .release:
            releaseSafeFields(from: fields)
        }
        if let fieldsJson = jsonString(from: renderedFields) {
            parts.append("fields=\(fieldsJson)")
        }
        if policy == .diagnostic, let payloadJson, !payloadJson.isEmpty {
            parts.append("payload=\(payloadJson)")
        }
        return parts.joined(separator: " ")
    }

    private static func releaseSafeFields(from fields: [String: Any]) -> [String: Any] {
        // Release fields are rendered public. Keep this allowlist limited to
        // bounded, hard-coded operational metadata.
        var safeFields: [String: Any] = [:]

        if let operation = safeIdentifier(fields["operation"]) {
            safeFields["operation"] = operation
        }
        if let errorDomain = safeIdentifier(fields["error_domain"]) {
            safeFields["error_domain"] = errorDomain
        }
        if let errorCode = fields["error_code"] as? Int {
            safeFields["error_code"] = errorCode
        }
        if let status = fields["status"] as? Int {
            safeFields["status"] = status
        }

        return safeFields
    }

    private static func safeIdentifier(_ value: Any?) -> String? {
        guard let value = value as? String,
              !value.isEmpty,
              value.count <= 80,
              value.unicodeScalars.allSatisfy({ safeIdentifierCharacters.contains($0) }) else {
            return nil
        }
        return value
    }

    private static func resolveCodexHome() -> URL {
        let base =
            FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory(), isDirectory: true)
        let codexHome = base.appendingPathComponent("codex", isDirectory: true)
        try? FileManager.default.createDirectory(at: codexHome, withIntermediateDirectories: true)
        return codexHome
    }

    private static func jsonString(from fields: [String: Any]) -> String? {
        guard !fields.isEmpty, JSONSerialization.isValidJSONObject(fields) else { return nil }
        guard let data = try? JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys]) else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }
}
