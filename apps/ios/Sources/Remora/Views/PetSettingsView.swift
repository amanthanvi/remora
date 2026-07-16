import SwiftUI

struct PetSettingsView: View {
    @Environment(AppModel.self) private var appModel
    @State private var controller = PetOverlayController.shared
    @State private var selectedServerId = ""
    @State private var pets: [AppPetSummary] = []
    @State private var isLoading = false
    @State private var errorMessage: String?

    private var connectedServers: [AppServerSnapshot] {
        appModel.snapshot?.servers.filter(\.isConnected) ?? []
    }

    var body: some View {
        Form {
            Section {
                Toggle(isOn: Binding(
                    get: { controller.visible },
                    set: { controller.setVisible($0) }
                )) {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Show Pet")
                            .remoraFont(.subheadline)
                            .foregroundColor(RemoraTheme.textPrimary)
                        Text(controller.selectedPet?.displayName ?? "No pet selected")
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                }
                .tint(RemoraTheme.accent)
                .listRowBackground(RemoraTheme.surface.opacity(0.6))
            } header: {
                Text("Wake")
                    .foregroundColor(RemoraTheme.textSecondary)
            }

            Section {
                if connectedServers.isEmpty {
                    Text("Connect to a server first")
                        .remoraFont(.footnote)
                        .foregroundColor(RemoraTheme.textMuted)
                } else {
                    ForEach(connectedServers, id: \.serverId) { server in
                        Button {
                            selectedServerId = server.serverId
                            Task { await refreshPets() }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(server.displayName)
                                        .remoraFont(.subheadline)
                                        .foregroundColor(RemoraTheme.textPrimary)
                                    Text(server.connectionModeLabel)
                                        .remoraFont(.caption)
                                        .foregroundColor(RemoraTheme.textSecondary)
                                }
                                Spacer()
                                if server.serverId == selectedServerId {
                                    Image(systemName: "checkmark")
                                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                                }
                            }
                        }
                    }
                }
            } header: {
                Text("Server")
                    .foregroundColor(RemoraTheme.textSecondary)
            }

            Section {
                if selectedServerId.isEmpty {
                    Text("No server selected")
                        .foregroundColor(RemoraTheme.textMuted)
                } else if isLoading {
                    HStack {
                        ProgressView().tint(RemoraTheme.accent)
                        Text("Loading pets")
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                } else if let errorMessage {
                    Text(errorMessage)
                        .foregroundColor(RemoraTheme.danger)
                } else if pets.isEmpty {
                    Text("~/.codex/pets has no hatch-pet packages")
                        .foregroundColor(RemoraTheme.textMuted)
                } else {
                    ForEach(pets, id: \.id) { pet in
                        Button {
                            guard pet.hasValidSpritesheet else { return }
                            Task {
                                await controller.selectPet(
                                    appModel: appModel,
                                    serverId: selectedServerId,
                                    pet: pet
                                )
                            }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(pet.displayName)
                                        .remoraFont(.subheadline)
                                        .foregroundColor(pet.hasValidSpritesheet ? RemoraTheme.textPrimary : RemoraTheme.textMuted)
                                    Text(pet.validationError ?? pet.description ?? pet.sourcePath)
                                        .remoraFont(.caption)
                                        .foregroundColor(RemoraTheme.textSecondary)
                                        .lineLimit(2)
                                }
                                Spacer()
                                if controller.isLoading,
                                   controller.selectedPet?.id == pet.id,
                                   controller.selectedPet?.serverId == selectedServerId {
                                    ProgressView().tint(RemoraTheme.accent)
                                } else if controller.selectedPet?.id == pet.id,
                                          controller.selectedPet?.serverId == selectedServerId {
                                    Image(systemName: "checkmark")
                                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                                }
                            }
                        }
                        .disabled(!pet.hasValidSpritesheet)
                    }
                }

                if let message = controller.errorMessage {
                    Text(message)
                        .foregroundColor(RemoraTheme.danger)
                }
            } header: {
                HStack {
                    Text("Pets")
                    Spacer()
                    Button("Refresh") {
                        Task { await refreshPets() }
                    }
                    .disabled(selectedServerId.isEmpty || isLoading)
                }
                .foregroundColor(RemoraTheme.textSecondary)
            }
        }
        .scrollContentBackground(.hidden)
        .background(RemoraTheme.backgroundGradient.ignoresSafeArea())
        .navigationTitle("Pet")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            if selectedServerId.isEmpty {
                selectedServerId = controller.selectedPet?.serverId
                    ?? appModel.snapshot?.activeThread?.serverId
                    ?? connectedServers.first?.serverId
                    ?? ""
            }
            await refreshPets()
        }
    }

    @MainActor
    private func refreshPets() async {
        guard !selectedServerId.isEmpty else { return }
        isLoading = true
        errorMessage = nil
        do {
            pets = try await appModel.client.listPets(serverId: selectedServerId)
        } catch {
            pets = []
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }
}
