import SwiftUI

struct ComposerRecoveryMenu: View {
    let store: ComposerRecoveryStore
    let context: ComposerDraftContext
    let onRecover: (UUID) -> Void
    @State private var discardTarget: ComposerRecoveryStore.Entry?
    @State private var discardError: String?

    var body: some View {
        let entries = store.saved(in: context)
        VStack(spacing: 0) {
            if let error = store.persistenceError ?? discardError {
                Text(error)
                    .remoraFont(.caption)
                    .foregroundStyle(RemoraTheme.warning)
                    .padding(10)
            }
            if !entries.isEmpty {
                Menu {
                    ForEach(entries) { entry in
                        Menu {
                            if let error = entry.error {
                                Text(error)
                            }
                            Button {
                                onRecover(entry.id)
                            } label: {
                                Label("Recover", systemImage: "arrow.uturn.backward")
                            }
                            Button(role: .destructive) {
                                discardError = nil
                                discardTarget = entry
                            } label: {
                                Label("Delete Saved Draft", systemImage: "trash")
                            }
                        } label: {
                            Label(entry.draft.text.isEmpty ? "Attachments" : String(entry.draft.text.prefix(80)),
                                  systemImage: "arrow.uturn.backward")
                        }
                    }
                } label: {
                    Label("Saved drafts (\(entries.count))", systemImage: "tray.full")
                        .remoraFont(.caption)
                        .padding(10)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .alert(item: $discardTarget) { entry in
                    Alert(
                        title: Text("Delete Saved Draft?"),
                        message: Text("This cannot be undone. Your current draft will not change."),
                        primaryButton: .destructive(Text("Delete")) {
                            Task {
                                do {
                                    try await store.discard(entry.id, in: entry.context)
                                } catch {
                                    discardError = error.localizedDescription
                                }
                            }
                        },
                        secondaryButton: .cancel()
                    )
                }
            }
        }
        .task { try? await store.load() }
    }
}
