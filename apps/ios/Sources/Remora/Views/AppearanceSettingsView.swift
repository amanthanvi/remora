import SwiftUI

struct AppearanceSettingsView: View {
    @Environment(ThemeManager.self) private var themeManager
    @State private var activeThemePicker: ThemePickerKind?
    @AppStorage("conversationTextSizeStep") private var textSizeStep = ConversationTextSize.large.rawValue

    var body: some View {
        ZStack {
            RemoraTheme.backgroundGradient.ignoresSafeArea()
            Form {
                appearanceModeSection
                fontSizeSection
                conversationPreviewSection
                lightThemeSection
                darkThemeSection
            }
            .scrollContentBackground(.hidden)
        }
        .navigationTitle("Appearance")
        .navigationBarTitleDisplayMode(.inline)
        .sheet(item: $activeThemePicker) { pickerKind in
            ThemePickerSheet(
                title: pickerKind.title,
                themes: themes(for: pickerKind),
                selectedSlug: selectedSlug(for: pickerKind)
            ) { slug in
                selectTheme(slug, for: pickerKind)
            }
            .presentationDetents([.medium, .large])
            .presentationDragIndicator(.visible)
        }
    }

    // MARK: - Appearance Mode

    private var appearanceModeSection: some View {
        Section {
            Picker(
                "Appearance",
                selection: Binding(
                    get: { themeManager.appearanceMode },
                    set: { themeManager.setAppearanceMode($0) }
                )
            ) {
                ForEach(RemoraAppearanceMode.allCases) { mode in
                    Text(mode.displayName).tag(mode)
                }
            }
            .pickerStyle(.segmented)
            .tint(RemoraTheme.accent)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Mode")
                .foregroundColor(RemoraTheme.textSecondary)
        } footer: {
            Text("Match the device setting, or keep Remora fixed in light or dark mode.")
                .foregroundColor(RemoraTheme.textMuted)
        }
    }

    // MARK: - Font Size

    private var fontSizeSection: some View {
        Section {
            VStack(spacing: 12) {
                HStack {
                    Text("Font Size")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    Spacer()
                    Text(ConversationTextSize.clamped(rawValue: textSizeStep).label)
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textSecondary)
                }

                HStack(spacing: 6) {
                    Text("A")
                        .font(.system(size: 12, weight: .medium))
                        .foregroundColor(RemoraTheme.textMuted)

                    Slider(
                        value: Binding(
                            get: { Double(textSizeStep) },
                            set: { textSizeStep = Int($0.rounded()) }
                        ),
                        in: Double(ConversationTextSize.tiny.rawValue)...Double(ConversationTextSize.huge.rawValue),
                        step: 1
                    )
                    .tint(RemoraTheme.accent)

                    Text("A")
                        .font(.system(size: 20, weight: .medium))
                        .foregroundColor(RemoraTheme.textMuted)
                }
            }
            .padding(.vertical, 4)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        } header: {
            Text("Font Size")
                .foregroundColor(RemoraTheme.textSecondary)
        } footer: {
            Text("Pinch in conversations to adjust, or use this slider. Applies across the app.")
                .foregroundColor(RemoraTheme.textMuted)
        }
    }

    // MARK: - Conversation Preview

    private var conversationPreviewSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                UserBubble(text: "Hey clanker, why is prod on fire", compact: true)

                ToolCallCardView(model: ToolCallCardModel(
                    kind: .commandExecution,
                    title: "Command",
                    summary: "rg 'TODO: fix later' --count",
                    status: .completed,
                    duration: "0.3s",
                    sections: []
                ))

                AssistantBubble(
                    text: """
                    Found the issue. Someone deployed this:

                    ```python
                    if is_friday():
                        yolo_deploy(skip_tests=True)
                    ```
                    I'm not mad, just disappointed.
                    """,
                    compact: true
                )

                UserBubble(text: "That was you, clanker", compact: true)
            }
            .padding(.vertical, 6)
            .environment(\.textScale, ConversationTextSize.clamped(rawValue: textSizeStep).scale)
            .id(themeManager.themeVersion)
            .listRowBackground(RemoraTheme.backgroundGradient)
            .listRowInsets(EdgeInsets(top: 0, leading: 12, bottom: 0, trailing: 12))
        } header: {
            Text("Preview")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Light Theme

    private var lightThemeSection: some View {
        Section {
            themePicker(
                themes: themeManager.lightThemes,
                selectedSlug: themeManager.selectedLightSlug,
                pickerKind: .light
            )
        } header: {
            Text("Light theme")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Dark Theme

    private var darkThemeSection: some View {
        Section {
            themePicker(
                themes: themeManager.darkThemes,
                selectedSlug: themeManager.selectedDarkSlug,
                pickerKind: .dark
            )
        } header: {
            Text("Dark theme")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    // MARK: - Theme Picker

    private func themePicker(
        themes: [ThemeIndexEntry],
        selectedSlug: String,
        pickerKind: ThemePickerKind
    ) -> some View {
        let selected = themes.first(where: { $0.slug == selectedSlug }) ?? themes.first
        return Button {
            activeThemePicker = pickerKind
        } label: {
            ThemePickerRow(entry: selected, trailingAccessory: .chevron)
        }
        .buttonStyle(.plain)
        .listRowBackground(RemoraTheme.surface.opacity(0.6))
    }

    private func themes(for pickerKind: ThemePickerKind) -> [ThemeIndexEntry] {
        switch pickerKind {
        case .light:
            themeManager.lightThemes
        case .dark:
            themeManager.darkThemes
        }
    }

    private func selectedSlug(for pickerKind: ThemePickerKind) -> String {
        switch pickerKind {
        case .light:
            themeManager.selectedLightSlug
        case .dark:
            themeManager.selectedDarkSlug
        }
    }

    private func selectTheme(_ slug: String, for pickerKind: ThemePickerKind) {
        switch pickerKind {
        case .light:
            themeManager.selectLightTheme(slug)
        case .dark:
            themeManager.selectDarkTheme(slug)
        }
    }
}

private enum ThemePickerKind: String, Identifiable {
    case light
    case dark

    var id: String { rawValue }

    var title: String {
        switch self {
        case .light:
            "Light Theme"
        case .dark:
            "Dark Theme"
        }
    }
}

private enum ThemePickerTrailingAccessory {
    case none
    case chevron
    case checkmark
}

private struct ThemePickerRow: View {
    let entry: ThemeIndexEntry?
    let trailingAccessory: ThemePickerTrailingAccessory

    var body: some View {
        HStack(spacing: 10) {
            ThemePreviewBadge(
                backgroundHex: entry?.backgroundHex ?? "#000000",
                foregroundHex: entry?.foregroundHex ?? "#FFFFFF",
                accentHex: entry?.accentHex ?? "#0DD5F0"
            )

            Text(entry?.name ?? "Unknown Theme")
                .remoraFont(.subheadline)
                .foregroundColor(RemoraTheme.textPrimary)
                .lineLimit(1)

            Spacer(minLength: 12)

            switch trailingAccessory {
            case .none:
                EmptyView()
            case .chevron:
                Image(systemName: "chevron.up.chevron.down")
                    .remoraFont(size: 11)
                    .foregroundColor(RemoraTheme.textMuted)
            case .checkmark:
                Image(systemName: "checkmark")
                    .remoraFont(size: 12, weight: .semibold)
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
            }
        }
        .contentShape(Rectangle())
    }
}

private struct ThemePickerSheet: View {
    let title: String
    let themes: [ThemeIndexEntry]
    let selectedSlug: String
    let onSelect: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var searchQuery = ""

    private var trimmedSearchQuery: String {
        searchQuery.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private var filteredThemes: [ThemeIndexEntry] {
        guard !trimmedSearchQuery.isEmpty else { return themes }
        return themes.filter { entry in
            entry.name.localizedCaseInsensitiveContains(trimmedSearchQuery) ||
            entry.slug.localizedCaseInsensitiveContains(trimmedSearchQuery)
        }
    }

    var body: some View {
        NavigationStack {
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()

                VStack(spacing: 12) {
                    searchField

                    if filteredThemes.isEmpty {
                        emptyState
                    } else {
                        ScrollView {
                            LazyVStack(spacing: 10) {
                                ForEach(filteredThemes) { entry in
                                    Button {
                                        onSelect(entry.slug)
                                        dismiss()
                                    } label: {
                                        ThemePickerRow(
                                            entry: entry,
                                            trailingAccessory: entry.slug == selectedSlug ? .checkmark : .none
                                        )
                                        .padding(.horizontal, 12)
                                        .padding(.vertical, 11)
                                        .frame(maxWidth: .infinity, alignment: .leading)
                                        .background(RemoraTheme.surface.opacity(0.72))
                                        .overlay {
                                            RoundedRectangle(cornerRadius: 12, style: .continuous)
                                                .stroke(
                                                    entry.slug == selectedSlug
                                                        ? RemoraTheme.accent.opacity(0.6)
                                                        : RemoraTheme.border.opacity(0.85),
                                                    lineWidth: 1
                                                )
                                        }
                                        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
                                    }
                                    .buttonStyle(.plain)
                                }
                            }
                            .padding(.horizontal, 16)
                            .padding(.bottom, 16)
                        }
                    }
                }
                .padding(.top, 8)
            }
            .navigationTitle(title)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") {
                        dismiss()
                    }
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
            }
        }
    }

    private var searchField: some View {
        HStack(spacing: 8) {
            Image(systemName: "magnifyingglass")
                .foregroundColor(RemoraTheme.textMuted)
                .remoraFont(size: 14, weight: .medium)

            TextField("Search themes", text: $searchQuery)
                .remoraFont(.subheadline)
                .foregroundColor(RemoraTheme.textPrimary)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled(true)

            if !searchQuery.isEmpty {
                Button {
                    searchQuery = ""
                } label: {
                    Image(systemName: "xmark.circle.fill")
                        .foregroundColor(RemoraTheme.textMuted)
                        .remoraFont(size: 14)
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 10)
        .background(RemoraTheme.surface.opacity(0.55))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(RemoraTheme.border.opacity(0.85), lineWidth: 1)
        }
        .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
        .padding(.horizontal, 16)
    }

    private var emptyState: some View {
        VStack(spacing: 10) {
            Image(systemName: "magnifyingglass")
                .remoraFont(size: 18, weight: .medium)
                .foregroundColor(RemoraTheme.textMuted)

            Text("No matching themes")
                .remoraFont(.subheadline)
                .foregroundColor(RemoraTheme.textPrimary)

            if !trimmedSearchQuery.isEmpty {
                Text(trimmedSearchQuery)
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textSecondary)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .padding(.top, 48)
        .padding(.horizontal, 24)
    }
}

// MARK: - Theme Preview Badge

struct ThemePreviewBadge: View {
    let backgroundHex: String
    let foregroundHex: String
    let accentHex: String

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            Text("Aa")
                .font(.system(size: 12, weight: .bold, design: .monospaced))
                .foregroundColor(Color(hex: foregroundHex))
                .frame(width: 28, height: 22)
                .background(
                    RoundedRectangle(cornerRadius: 5, style: .continuous)
                        .fill(Color(hex: backgroundHex))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 5, style: .continuous)
                        .stroke(Color.gray.opacity(0.3), lineWidth: 0.5)
                )
            Circle()
                .fill(Color(hex: accentHex))
                .frame(width: 6, height: 6)
                .offset(x: 1, y: 1)
        }
    }

    @MainActor
    static func renderToImage(backgroundHex: String, foregroundHex: String, accentHex: String) -> UIImage {
        let badge = ThemePreviewBadge(backgroundHex: backgroundHex, foregroundHex: foregroundHex, accentHex: accentHex)
        let renderer = ImageRenderer(content: badge)
        renderer.scale = UIScreen.main.scale
        guard let cgImage = renderer.cgImage else { return UIImage() }
        return UIImage(cgImage: cgImage).withRenderingMode(.alwaysOriginal)
    }
}

#if DEBUG
#Preview("Appearance") {
    RemoraPreviewScene(includeBackground: false) {
        NavigationStack {
            AppearanceSettingsView()
        }
    }
}
#endif
