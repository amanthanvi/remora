import Foundation
import Observation

enum RemoraFeature: String, CaseIterable, Identifiable {
    case realtimeVoice = "realtime_voice"
    case terminal = "terminal"

    var id: String { rawValue }

    var displayName: String {
        switch self {
        case .realtimeVoice: return "Realtime"
        case .terminal: return "Terminal"
        }
    }

    var description: String {
        switch self {
        case .realtimeVoice: return "Show the realtime voice launcher on the home screen."
        case .terminal: return "Show the remote terminal launcher on the home screen."
        }
    }

    var defaultEnabled: Bool {
        switch self {
        case .realtimeVoice: return true
        case .terminal: return false
        }
    }
}

@Observable
final class ExperimentalFeatures {
    static let shared = ExperimentalFeatures()

    @ObservationIgnored private let key = "remora.experimentalFeatures"
    private var overrides: [String: Bool]

    private init() {
        overrides = UserDefaults.standard.dictionary(forKey: key) as? [String: Bool] ?? [:]
    }

    private func persistOverrides() {
        UserDefaults.standard.set(overrides, forKey: key)
    }

    func isEnabled(_ feature: RemoraFeature) -> Bool {
        overrides[feature.rawValue] ?? feature.defaultEnabled
    }

    func setEnabled(_ feature: RemoraFeature, _ value: Bool) {
        var map = overrides
        if value == feature.defaultEnabled {
            map.removeValue(forKey: feature.rawValue)
        } else {
            map[feature.rawValue] = value
        }
        overrides = map
        persistOverrides()
    }

}
