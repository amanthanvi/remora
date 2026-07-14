import SwiftUI
import UIKit

struct RateLimitBadgeView: View, Equatable {
    let label: String
    let percent: Int

    private var tint: Color {
        if percent <= 10 { return RemoraTheme.danger }
        if percent <= 30 { return RemoraTheme.warning }
        return RemoraTheme.textMuted
    }

    var body: some View {
        HStack(spacing: 3) {
            Text(label)
                .font(RemoraFont.monospaced(size: 9.5, weight: .semibold))
                .foregroundColor(RemoraTheme.textSecondary)
            ContextBadgeView(percent: percent, tint: tint)
        }
    }
}

struct ConversationLoadingIndicator: View {
    let label: String
    @State private var shimmerOffset: CGFloat = -1

    var body: some View {
        Text(label)
            .remoraFont(.body, weight: .medium)
            .foregroundStyle(
                LinearGradient(
                    colors: [
                        RemoraTheme.textSecondary.opacity(0.4),
                        RemoraTheme.textSecondary.opacity(0.7),
                        RemoraTheme.textSecondary.opacity(0.4),
                    ],
                    startPoint: UnitPoint(x: shimmerOffset - 0.3, y: 0.5),
                    endPoint: UnitPoint(x: shimmerOffset + 0.3, y: 0.5)
                )
            )
            .animation(.easeInOut(duration: 1.5).repeatForever(autoreverses: false), value: shimmerOffset)
            .onAppear {
                shimmerOffset = 2
            }
    }
}

struct MinigameLaunchButton: View {
    let action: () -> Void

    var body: some View {
        Button(action: action) {
            Image(systemName: "gamecontroller.fill")
                .font(.system(size: 14, weight: .semibold))
                .foregroundStyle(RemoraTheme.accent)
                .frame(width: 36, height: 36)
                .background(
                    Circle()
                        .fill(RemoraTheme.surface.opacity(0.9))
                        .overlay(
                            Circle()
                                .stroke(RemoraTheme.accent.opacity(0.3), lineWidth: 0.5)
                        )
                )
                .shadow(color: Color.black.opacity(0.15), radius: 4, x: 0, y: 2)
        }
        .buttonStyle(.plain)
        .accessibilityLabel("Play a minigame while waiting")
    }
}

struct TypingIndicator: View {
    @State private var shimmerOffset: CGFloat = -1

    var body: some View {
        Text("Thinking")
            .remoraFont(.body, weight: .medium)
            .foregroundStyle(
                LinearGradient(
                    colors: [
                        RemoraTheme.textSecondary.opacity(0.4),
                        RemoraTheme.accent,
                        RemoraTheme.textSecondary.opacity(0.4),
                    ],
                    startPoint: UnitPoint(x: shimmerOffset - 0.3, y: 0.5),
                    endPoint: UnitPoint(x: shimmerOffset + 0.3, y: 0.5)
                )
            )
            .animation(.easeInOut(duration: 1.5).repeatForever(autoreverses: false), value: shimmerOffset)
            .padding(.leading, 12)
            .onAppear {
                shimmerOffset = 2
            }
    }
}

struct CameraView: UIViewControllerRepresentable {
    @Binding var image: UIImage?
    @Environment(\.dismiss) private var dismiss

    func makeUIViewController(context: Context) -> UIImagePickerController {
        let picker = UIImagePickerController()
        picker.sourceType = .camera
        picker.delegate = context.coordinator
        return picker
    }

    func updateUIViewController(_ uiViewController: UIImagePickerController, context: Context) {}

    func makeCoordinator() -> Coordinator { Coordinator(self) }

    class Coordinator: NSObject, UIImagePickerControllerDelegate, UINavigationControllerDelegate {
        let parent: CameraView
        init(_ parent: CameraView) { self.parent = parent }

        func imagePickerController(_ picker: UIImagePickerController, didFinishPickingMediaWithInfo info: [UIImagePickerController.InfoKey: Any]) {
            if let img = info[.originalImage] as? UIImage {
                parent.image = img
            }
            parent.dismiss()
        }

        func imagePickerControllerDidCancel(_ picker: UIImagePickerController) {
            parent.dismiss()
        }
    }
}

struct SubagentBreadcrumbBar: View {
    let thread: AppThreadSnapshot
    let topInset: CGFloat
    let onNavigateToParent: () -> Void

    var body: some View {
        HStack(spacing: 8) {
            Button(action: onNavigateToParent) {
                HStack(spacing: 4) {
                    Image(systemName: "chevron.left")
                        .remoraFont(size: 10, weight: .semibold)
                    Text("Parent")
                        .remoraFont(.caption, weight: .medium)
                }
                .foregroundColor(RemoraTheme.accent)
            }
            .buttonStyle(.plain)

            Divider()
                .frame(height: 14)
                .background(RemoraTheme.border)

            HStack(spacing: 4) {
                Image(systemName: "person.fill")
                    .remoraFont(size: 10, weight: .semibold)
                    .foregroundColor(RemoraTheme.success)
                Text(thread.agentDisplayLabel ?? "Agent")
                    .remoraFont(.caption, weight: .medium)
                    .foregroundColor(RemoraTheme.textPrimary)
                    .lineLimit(1)
            }

            Spacer()
        }
        .padding(.horizontal, 16)
        .padding(.vertical, 6)
        .padding(.top, topInset + 8)
        .background(
            RemoraTheme.surface.opacity(0.85)
                .background(.ultraThinMaterial)
                .ignoresSafeArea()
        )
    }
}

// MARK: - Debug Overlay

struct ConversationDebugButton: View {
    let topInset: CGFloat
    let activeThreadKey: ThreadKey
    @Environment(AppModel.self) private var appModel
    @State private var showPopover = false

    var body: some View {
        HStack(spacing: 6) {
            Button {
                showPopover.toggle()
            } label: {
                Image(systemName: "ant")
                    .remoraFont(size: 12, weight: .semibold)
                    .foregroundColor(RemoraTheme.accent)
                    .padding(6)
                    .background(
                        Circle()
                            .fill(RemoraTheme.surface.opacity(0.85))
                            .background(Circle().fill(.ultraThinMaterial))
                    )
            }
            .buttonStyle(.plain)

            if DebugSettings.shared.enabled {
                if MessageRecorder.shared.isRecording {
                    Circle()
                        .fill(Color.red)
                        .frame(width: 8, height: 8)
                        .modifier(PulseModifier())
                }
                if MessageRecorder.shared.isReplaying {
                    Image(systemName: "play.fill")
                        .remoraFont(size: 8, weight: .semibold)
                        .foregroundColor(RemoraTheme.accent)
                }
            }
        }
        .padding(.leading, 16)
        .padding(.top, topInset + 12)
        .popover(isPresented: $showPopover) {
            DebugPopoverContent(activeThreadKey: activeThreadKey)
                .environment(appModel)
                .presentationCompactAdaptation(.popover)
        }
    }
}

private struct PulseModifier: ViewModifier {
    @State private var pulse = false
    func body(content: Content) -> some View {
        content
            .opacity(pulse ? 0.3 : 1.0)
            .animation(.easeInOut(duration: 0.8).repeatForever(autoreverses: true), value: pulse)
            .onAppear { pulse = true }
    }
}

private struct DebugPopoverContent: View {
    @Environment(AppModel.self) private var appModel
    let activeThreadKey: ThreadKey
    @State private var debugSettings = DebugSettings.shared
    @State private var recorder = MessageRecorder.shared
    @State private var recordings: [URL] = []

    var body: some View {
        ScrollView {
        VStack(alignment: .leading, spacing: 12) {
            Text("Debug")
                .remoraFont(.subheadline, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)

            Toggle(isOn: Binding(
                get: { debugSettings.disableMarkdown },
                set: { debugSettings.disableMarkdown = $0 }
            )) {
                Text("Disable Markdown")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textPrimary)
            }
            .tint(RemoraTheme.accent)

            Toggle(isOn: Binding(
                get: { debugSettings.showTurnMetrics },
                set: { debugSettings.showTurnMetrics = $0 }
            )) {
                Text("Turn Metrics")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textPrimary)
            }
            .tint(RemoraTheme.accent)

            if debugSettings.enabled {
                Divider().background(RemoraTheme.border)

                // MARK: Recording controls
                Text("Recording")
                    .remoraFont(.caption, weight: .semibold)
                    .foregroundColor(RemoraTheme.textPrimary)

                HStack(spacing: 8) {
                    if recorder.isRecording {
                        Button {
                            recorder.stopRecording(store: appModel.store)
                            recordings = recorder.listRecordings()
                        } label: {
                            Label("Stop", systemImage: "stop.fill")
                                .remoraFont(.caption, weight: .medium)
                                .foregroundColor(.red)
                        }
                        .buttonStyle(.plain)
                    } else if recorder.isReplaying {
                        Button {
                            recorder.stopReplay()
                        } label: {
                            Label("Stop", systemImage: "stop.fill")
                                .remoraFont(.caption, weight: .medium)
                                .foregroundColor(.orange)
                        }
                        .buttonStyle(.plain)
                    } else {
                        Button {
                            recorder.startRecording(store: appModel.store)
                        } label: {
                            Label("Record", systemImage: "record.circle")
                                .remoraFont(.caption, weight: .medium)
                                .foregroundColor(.red)
                        }
                        .buttonStyle(.plain)
                    }
                }

                if !recordings.isEmpty && !recorder.isRecording && !recorder.isReplaying {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(recordings, id: \.absoluteString) { url in
                            HStack {
                                Button {
                                    recorder.startReplay(url: url, store: appModel.store, targetKey: activeThreadKey)
                                } label: {
                                    HStack(spacing: 4) {
                                        Image(systemName: "play.fill")
                                            .remoraFont(size: 9, weight: .semibold)
                                        Text(url.deletingPathExtension().lastPathComponent)
                                            .remoraFont(.caption2)
                                            .lineLimit(1)
                                    }
                                    .foregroundColor(RemoraTheme.accent)
                                }
                                .buttonStyle(.plain)

                                Spacer()

                                Button {
                                    recorder.deleteRecording(url: url)
                                    recordings = recorder.listRecordings()
                                } label: {
                                    Image(systemName: "xmark")
                                        .remoraFont(size: 9, weight: .semibold)
                                        .foregroundColor(RemoraTheme.textSecondary)
                                }
                                .buttonStyle(.plain)
                            }
                        }
                    }
                }
            }
        }
        .padding(16)
        }
        .frame(width: 260)
        .frame(maxHeight: 500)
        .background(RemoraTheme.surface)
        .onAppear { recordings = recorder.listRecordings() }
    }
}

private struct TurnDebugOverlay: ViewModifier {
    let turnId: String

    // Debug is already gated at the call site via `.turnDebugOverlay(turnId:)`,
    // so this modifier only runs when debug overlays should actually render —
    // no inner if/else branch means SwiftUI no longer has to diff a
    // `_ConditionalContent<Modified, Content>` per turn on every body eval.
    func body(content: Content) -> some View {
        content
            .overlay(
                GeometryReader { geo in
                    VStack(alignment: .leading) {
                        Text("\(turnId.prefix(8)) h=\(Int(geo.size.height)) y=\(Int(geo.frame(in: .global).minY))")
                            .font(.system(size: 9, weight: .bold, design: .monospaced))
                            .foregroundColor(.red)
                            .padding(2)
                            .background(.black.opacity(0.7))
                        Spacer()
                    }
                }
            )
            .border(Color.red.opacity(0.3), width: 1)
    }
}

extension View {
    /// Applies `TurnDebugOverlay` only when debug settings opt into turn
    /// metrics. Reading the flag here — rather than inside the modifier's
    /// body — means the overlay node doesn't participate in the view tree
    /// at all for the common (debug-off) case, saving per-turn per-diff
    /// modifier evaluation cost.
    @ViewBuilder
    func turnDebugOverlay(turnId: String) -> some View {
        if DebugSettings.shared.enabled && DebugSettings.shared.showTurnMetrics {
            self.modifier(TurnDebugOverlay(turnId: turnId))
        } else {
            self
        }
    }
}
