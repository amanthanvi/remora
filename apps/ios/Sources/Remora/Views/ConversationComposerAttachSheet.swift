import SwiftUI

struct ConversationComposerAttachSheet: View {
    let onPickPhotoLibrary: () -> Void
    let onChooseFile: (() -> Void)?
    let onTakePhoto: (() -> Void)?

    var body: some View {
        VStack(spacing: 12) {
            Text("Attach")
                .remoraFont(.headline, weight: .semibold)
                .foregroundColor(RemoraTheme.textPrimary)
                .frame(maxWidth: .infinity, alignment: .leading)

            Button(action: onPickPhotoLibrary) {
                sheetButtonLabel("Photo Library", systemImage: "photo.on.rectangle")
            }

            if let onChooseFile {
                Button(action: onChooseFile) {
                    sheetButtonLabel("Choose File", systemImage: "folder")
                }
            }

            if let onTakePhoto {
                Button(action: onTakePhoto) {
                    sheetButtonLabel("Take Photo", systemImage: "camera")
                }
            }

            Spacer(minLength: 0)
        }
        .padding(.horizontal, 16)
        .padding(.top, 12)
        .padding(.bottom, 20)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
    }

    @ViewBuilder
    private func sheetButtonLabel(_ title: String, systemImage: String) -> some View {
        HStack(spacing: 10) {
            Image(systemName: systemImage)
                .remoraFont(.body, weight: .medium)
                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                .frame(width: 20)

            Text(title)
                .remoraFont(.body, weight: .medium)
                .foregroundColor(RemoraTheme.textPrimary)

            Spacer()
        }
        .padding(.horizontal, 16)
        .frame(height: 52)
        .modifier(GlassRoundedRectModifier(cornerRadius: 18))
    }
}
