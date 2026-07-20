import SwiftUI

struct CommandPaletteView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var actionCenter = RemoraActionCenter.shared
    @State private var query = ""

    private var filteredItems: [RemoraActionItem] {
        RemoraActionCatalog.filteredItems(actionCenter.items, query: query)
    }

    private var visibleGroups: [RemoraActionGroup] {
        RemoraActionGroup.allCases.filter { group in
            filteredItems.contains { $0.definition.group == group }
        }
    }

    var body: some View {
        NavigationStack {
            List {
                if let message = actionCenter.executionErrorMessage {
                    Label(message, systemImage: "exclamationmark.triangle.fill")
                        .remoraFont(.footnote)
                        .foregroundStyle(RemoraTheme.warning)
                        .listRowBackground(RemoraTheme.surface)
                        .accessibilityLabel("Command error: \(message)")
                }

                ForEach(visibleGroups) { group in
                    Section(group.title) {
                        ForEach(filteredItems.filter { $0.definition.group == group }) { item in
                            commandRow(item)
                        }
                    }
                }
            }
            .listStyle(.plain)
            .accessibilityIdentifier("commandPalette.list")
            .scrollContentBackground(.hidden)
            .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
            .navigationTitle("Commands")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $query, prompt: "Find an action")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .onDisappear {
            actionCenter.executionErrorMessage = nil
        }
    }

    private func commandRow(_ item: RemoraActionItem) -> some View {
        Button {
            _ = actionCenter.perform(item.id, source: .palette)
        } label: {
            HStack(alignment: .center, spacing: 12) {
                Image(systemName: item.definition.systemImage)
                    .font(.system(size: 16, weight: .semibold))
                    .foregroundStyle(item.availability.isEnabled ? RemoraTheme.accentForegroundOnSurface : RemoraTheme.textMuted)
                    .frame(width: 24)
                    .accessibilityHidden(true)

                VStack(alignment: .leading, spacing: 3) {
                    Text(item.definition.title)
                        .remoraFont(.body, weight: .medium)
                        .foregroundStyle(
                            item.availability.isEnabled
                                ? RemoraTheme.textPrimary
                                : RemoraTheme.textMuted
                        )
                    Text(item.availability.disabledReason ?? item.definition.detail)
                        .remoraFont(.caption)
                        .foregroundStyle(RemoraTheme.textSecondary)
                        .lineLimit(2)
                }

                Spacer(minLength: 8)

                if actionCenter.executingActionID == item.id {
                    ProgressView()
                        .controlSize(.small)
                        .tint(RemoraTheme.accent)
                        .accessibilityLabel("Running")
                } else if let shortcut = item.definition.shortcut {
                    Text(shortcut.display)
                        .remoraMonoFont(size: 12, weight: .medium)
                        .foregroundStyle(RemoraTheme.textSecondary)
                        .accessibilityLabel("Shortcut \(shortcut.display)")
                }
            }
            .padding(.vertical, 5)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!item.availability.isEnabled || actionCenter.executingActionID != nil)
        .accessibilityIdentifier("commandPalette.action.\(item.id.rawValue)")
        .accessibilityHint(item.availability.disabledReason ?? item.definition.detail)
    }
}
