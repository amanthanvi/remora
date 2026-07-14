import SwiftUI

struct InlineVoiceStatusStrip: View {
    let session: VoiceSessionState
    let onToggleSpeaker: () -> Void

    private var inputLevel: Float {
        session.isListening ? max(0.08, session.scaledInputLevel) : max(0, session.scaledInputLevel)
    }

    private var outputLevel: Float {
        session.isSpeaking ? max(0.08, session.scaledOutputLevel) : max(0, session.scaledOutputLevel)
    }

    var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 5) {
                Circle()
                    .fill(session.isListening ? RemoraTheme.accent : RemoraTheme.textMuted.opacity(0.4))
                    .frame(width: 5, height: 5)
                Text("YOU")
                    .font(RemoraFont.monospaced(.caption2, weight: .bold))
                    .foregroundColor(session.isListening ? RemoraTheme.textPrimary : RemoraTheme.textMuted)
                AudioWaveformView(level: inputLevel, tint: RemoraTheme.accent)
                    .frame(width: 48, height: 14)
            }

            HStack(spacing: 5) {
                Circle()
                    .fill(session.isSpeaking ? RemoraTheme.warning : RemoraTheme.textMuted.opacity(0.4))
                    .frame(width: 5, height: 5)
                Text("CODEX")
                    .font(RemoraFont.monospaced(.caption2, weight: .bold))
                    .foregroundColor(session.isSpeaking ? RemoraTheme.textPrimary : RemoraTheme.textMuted)
                AudioWaveformView(level: outputLevel, tint: RemoraTheme.warning)
                    .frame(width: 48, height: 14)
            }

            Spacer()

            Button(action: onToggleSpeaker) {
                HStack(spacing: 4) {
                    Image(systemName: session.route.iconName)
                        .font(.system(size: 10, weight: .semibold))
                    Text(session.route.label)
                        .font(RemoraFont.styled(.caption2, weight: .semibold))
                }
                .foregroundColor(session.route.supportsSpeakerToggle ? RemoraTheme.textPrimary : RemoraTheme.textMuted)
            }
            .buttonStyle(.plain)
            .disabled(!session.route.supportsSpeakerToggle)

            Text(session.phase.displayTitle)
                .font(RemoraFont.monospaced(.caption2, weight: .medium))
                .foregroundColor(phaseColor(session.phase))
        }
        .padding(.horizontal, 14)
        .padding(.vertical, 6)
        .background(RemoraTheme.surface.opacity(0.6))
    }

    private func phaseColor(_ phase: VoiceSessionPhase) -> Color {
        switch phase {
        case .connecting, .thinking, .handoff:
            return RemoraTheme.warning
        case .listening, .speaking:
            return RemoraTheme.accent
        case .error:
            return RemoraTheme.danger
        }
    }
}
