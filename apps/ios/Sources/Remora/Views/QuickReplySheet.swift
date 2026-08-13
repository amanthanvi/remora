import SwiftUI

/// Minimal reply composer shown when the user swipes right on a home
/// session row. Sends a turn on the targeted thread and dismisses.
struct QuickReplySheet: View {
    let thread: HomeDashboardRecentSession
    let onSend: @MainActor (ThreadKey, String) async throws -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var text: String = ""
    @State private var isSending = false
    @State private var errorMessage: String?
    @FocusState private var isFocused: Bool

    private var canSend: Bool {
        !isSending && !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }

    var body: some View {
        NavigationStack {
            VStack(alignment: .leading, spacing: 12) {
                Text(thread.sessionTitle)
                    .remoraFont(.subheadline, weight: .semibold)
                    .foregroundStyle(RemoraTheme.textPrimary)
                    .lineLimit(2)

                Text(thread.serverDisplayName + " · " + (HomeDashboardSupport.workspaceLabel(for: thread.cwd) ?? PathDisplay.display(thread.cwd, isLocal: thread.isLocal)))
                    .remoraFont(.caption)
                    .foregroundStyle(RemoraTheme.textMuted)
                    .lineLimit(1)

                Divider().background(RemoraTheme.separator)

                TextField(
                    "Reply…",
                    text: $text,
                    axis: .vertical
                )
                .focused($isFocused)
                .lineLimit(1...8)
                .submitLabel(.send)
                .remoraFont(.body)
                .foregroundStyle(RemoraTheme.textPrimary)
                .padding(10)
                .background(RemoraTheme.surface, in: RoundedRectangle(cornerRadius: 10))
                .overlay(
                    RoundedRectangle(cornerRadius: 10)
                        .stroke(RemoraTheme.border, lineWidth: 0.5)
                )

                if let errorMessage {
                    Text(errorMessage)
                        .remoraFont(.caption)
                        .foregroundStyle(RemoraTheme.danger)
                }

                HStack {
                    Spacer()
                    Button {
                        Task { await submit() }
                    } label: {
                        HStack(spacing: 6) {
                            if isSending {
                                ProgressView().controlSize(.small).tint(.black)
                            }
                            Image(systemName: "arrow.up.circle.fill")
                                .font(.system(size: 18, weight: .semibold))
                            Text("Send")
                                .remoraFont(.subheadline, weight: .semibold)
                        }
                        .foregroundStyle(canSend ? Color.black : RemoraTheme.textMuted)
                        .padding(.horizontal, 14)
                        .padding(.vertical, 8)
                        .background(canSend ? RemoraTheme.accent : RemoraTheme.surfaceLight, in: Capsule())
                    }
                    .buttonStyle(.plain)
                    .disabled(!canSend)
                }

                Spacer()
            }
            .padding(16)
            .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
            .navigationTitle("Reply")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Cancel") { dismiss() }
                        .tint(RemoraTheme.textSecondary)
                }
            }
            .task {
                // Pop the keyboard once the sheet has settled.
                try? await Task.sleep(nanoseconds: 150_000_000)
                isFocused = true
            }
        }
    }

    private func submit() async {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, !isSending else { return }
        isSending = true
        errorMessage = nil
        do {
            try await onSend(thread.key, trimmed)
            isSending = false
            dismiss()
        } catch {
            errorMessage = error.localizedDescription
            isSending = false
        }
    }
}
