import SwiftUI

struct ExperimentalFeaturesView: View {
    @State private var experimentalFeatures = ExperimentalFeatures.shared
    @State private var debugSettings = DebugSettings.shared

    var body: some View {
        ZStack {
            RemoraTheme.backgroundGradient.ignoresSafeArea()
            Form {
                Section {
                    ForEach(RemoraFeature.allCases) { feature in
                        Toggle(isOn: binding(for: feature)) {
                            VStack(alignment: .leading, spacing: 4) {
                                Text(feature.displayName)
                                    .remoraFont(.subheadline)
                                    .foregroundColor(RemoraTheme.textPrimary)
                                Text(feature.description)
                                    .remoraFont(.caption)
                                    .foregroundColor(RemoraTheme.textSecondary)
                            }
                        }
                        .tint(RemoraTheme.accentStrong)
                        .listRowBackground(RemoraTheme.surface.opacity(0.6))
                    }
                } header: {
                    Text("Features")
                        .foregroundColor(RemoraTheme.textSecondary)
                } footer: {
                    Text("Experimental features may be unstable or change without notice.")
                        .foregroundColor(RemoraTheme.textMuted)
                }

                Section {
                    Toggle(isOn: Binding(
                        get: { debugSettings.enabled },
                        set: { debugSettings.enabled = $0 }
                    )) {
                        HStack(spacing: 10) {
                            Image(systemName: "ant")
                                .foregroundColor(RemoraTheme.accent)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Debug Mode")
                                    .remoraFont(.subheadline)
                                    .foregroundColor(RemoraTheme.textPrimary)
                                Text("Show debug controls in conversations")
                                    .remoraFont(.caption)
                                    .foregroundColor(RemoraTheme.textSecondary)
                            }
                        }
                    }
                    .tint(RemoraTheme.accent)
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))

                    #if DEBUG
                    NavigationLink {
                        ProximityPairView()
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "wave.3.right")
                                .foregroundColor(RemoraTheme.accent)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text("Pair")
                                    .remoraFont(.subheadline)
                                    .foregroundColor(RemoraTheme.textPrimary)
                                Text("Walk-up pairing with proximity + haptics")
                                    .remoraFont(.caption)
                                    .foregroundColor(RemoraTheme.textSecondary)
                            }
                        }
                    }
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
                    #endif

                    #if !targetEnvironment(macCatalyst) && DEBUG
                    NavigationLink {
                        UWBDebugView()
                    } label: {
                        HStack(spacing: 10) {
                            Image(systemName: "dot.radiowaves.left.and.right")
                                .foregroundColor(RemoraTheme.accent)
                                .frame(width: 20)
                            VStack(alignment: .leading, spacing: 2) {
                                Text("UWB Debug")
                                    .remoraFont(.subheadline)
                                    .foregroundColor(RemoraTheme.textPrimary)
                                Text("Live distance & direction to a paired Mac")
                                    .remoraFont(.caption)
                                    .foregroundColor(RemoraTheme.textSecondary)
                            }
                        }
                    }
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
                    #endif
                } header: {
                    Text("Debug")
                        .foregroundColor(RemoraTheme.textSecondary)
                }
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle("Experimental")
        .navigationBarTitleDisplayMode(.inline)
    }

    private func binding(for feature: RemoraFeature) -> Binding<Bool> {
        Binding(
            get: { experimentalFeatures.isEnabled(feature) },
            set: { newValue in
                experimentalFeatures.setEnabled(feature, newValue)
            }
        )
    }
}

#if DEBUG
#Preview("Experimental Features") {
    NavigationStack {
        ExperimentalFeaturesView()
    }
}
#endif
