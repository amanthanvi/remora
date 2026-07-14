import SwiftUI

struct AccountView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(\.dismiss) private var dismiss

    private var server: AppServerSnapshot? {
        // Account management (ChatGPT login / API key) is local-only, always.
        // If the local Codex bridge hasn't spun up there's no login target, and
        // the caller falls through to `AccountDisconnectedView`.
        appModel.snapshot?.servers.first(where: \.isLocal)
    }

    var body: some View {
        if let server {
            AccountConnectionView(server: server, dismiss: dismiss)
        } else {
            AccountDisconnectedView(dismiss: dismiss)
        }
    }
}

private struct AccountConnectionView: View {
    @Environment(AppModel.self) private var appModel
    let server: AppServerSnapshot
    let dismiss: DismissAction

    @State private var apiKey = ""
    @State private var isWorking = false
    @State private var authError: String?
    @State private var hasStoredApiKey = OpenAIApiKeyStore.shared.hasStoredKey

    var body: some View {
        NavigationStack {
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()
                ScrollView {
                    VStack(alignment: .leading, spacing: 24) {
                        currentAccountSection
                        Divider().background(RemoraTheme.surfaceLight)
                        loginSection
                        if let err = authError {
                            Text(err)
                                .font(.caption)
                                .foregroundColor(.red)
                                .padding(.horizontal, 20)
                        }
                    }
                    .padding(.top, 20)
                }
            }
            .navigationTitle("Account")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .foregroundColor(RemoraTheme.accent)
                }
            }
            .task(id: server.serverId) {
                await refreshAccount()
                hasStoredApiKey = OpenAIApiKeyStore.shared.hasStoredKey
            }
        }
    }

    private var currentAccountSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("CURRENT ACCOUNT")
                .remoraFont(.caption)
                .foregroundColor(RemoraTheme.textMuted)
                .padding(.horizontal, 20)

            HStack(spacing: 12) {
                Circle()
                    .fill(authColor)
                    .frame(width: 10, height: 10)
                VStack(alignment: .leading, spacing: 2) {
                    Text(authTitle)
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    if let sub = authSubtitle {
                        Text(sub)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                    }
                }
                Spacer()
                if server.isLocal, server.account != nil {
                    Button("Logout") {
                        Task { await logout() }
                    }
                    .remoraFont(.footnote)
                    .foregroundColor(RemoraTheme.danger)
                }
            }
            .padding(.horizontal, 20)
            .padding(.vertical, 14)
            .background(.ultraThinMaterial)
            .cornerRadius(10)
            .padding(.horizontal, 16)

            if server.isLocal, hasStoredApiKey {
                Text("Local OpenAI API key is saved.")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.accent)
                    .padding(.horizontal, 20)
            }
        }
    }

    private var loginSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("LOGIN")
                .remoraFont(.caption)
                .foregroundColor(RemoraTheme.textMuted)
                .padding(.horizontal, 20)

            if server.isLocal, !isChatGPTAccount {
                Button {
                    Task {
                        isWorking = true
                        await loginWithChatGPT()
                        isWorking = false
                    }
                } label: {
                    HStack {
                        if isWorking {
                            ProgressView().tint(RemoraTheme.textOnAccent).scaleEffect(0.8)
                        }
                        Image(systemName: "person.crop.circle.badge.checkmark")
                        Text("Login with ChatGPT")
                            .remoraFont(.subheadline)
                    }
                    .foregroundColor(RemoraTheme.textOnAccent)
                    .frame(maxWidth: .infinity)
                    .padding(.vertical, 14)
                    .background(RemoraTheme.accent)
                    .cornerRadius(10)
                }
                .padding(.horizontal, 16)
                .disabled(isWorking)
            }

            if server.isLocal, allowsLocalEnvApiKey {
                Text("— or save an API key for the local environment —")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textMuted)
                    .frame(maxWidth: .infinity)

                VStack(alignment: .leading, spacing: 8) {
                    if hasStoredApiKey {
                        Text("OpenAI API key saved in the local environment.")
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                            .padding(.horizontal, 16)
                    } else if isChatGPTAccount {
                        Text("Save an OpenAI API key in the local Codex environment.")
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.textSecondary)
                            .padding(.horizontal, 16)
                    }

                    SecureField("sk-...", text: $apiKey)
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .padding(12)
                        .background(RemoraTheme.surface)
                        .cornerRadius(8)
                        .padding(.horizontal, 16)

                    Button {
                        let key = apiKey.trimmingCharacters(in: .whitespaces)
                        guard !key.isEmpty else { return }
                        Task {
                            isWorking = true
                            await saveApiKey(key)
                            isWorking = false
                        }
                    } label: {
                        Text(hasStoredApiKey ? "Update API Key" : "Save API Key")
                            .remoraFont(.subheadline)
                            .foregroundColor(RemoraTheme.textPrimary)
                            .frame(maxWidth: .infinity)
                            .padding(12)
                            .background(RemoraTheme.surface)
                            .cornerRadius(8)
                            .padding(.horizontal, 16)
                    }
                    .disabled(apiKey.trimmingCharacters(in: .whitespaces).isEmpty || isWorking)
                }
            }
        }
    }

    private var allowsLocalEnvApiKey: Bool {
        server.isLocal
    }

    private var isChatGPTAccount: Bool {
        if case .chatgpt? = server.account {
            return true
        }
        return false
    }
    private var authColor: Color {
        switch server.account {
        case .chatgpt?:
            return RemoraTheme.accent
        case .apiKey?:
            return Color(hex: "#00AAFF")
        case nil:
            return RemoraTheme.textMuted
        }
    }

    private var authTitle: String {
        switch server.account {
        case .chatgpt(let email, _)?:
            return email.isEmpty ? "ChatGPT" : email
        case .apiKey?:
            return "API Key"
        case nil:
            return "Not logged in"
        }
    }

    private var authSubtitle: String? {
        switch server.account {
        case .chatgpt?:
            return "ChatGPT account"
        case .apiKey?:
            return "OpenAI API key"
        case nil:
            return nil
        }
    }

    private func refreshAccount() async {
        do {
            _ = try await appModel.client.refreshAccount(
                serverId: server.serverId,
                params: AppRefreshAccountRequest(refreshToken: false)
            )
            await appModel.refreshSnapshot()
            authError = nil
        } catch {
            authError = error.localizedDescription
        }
    }

    private func loginWithChatGPT() async {
        guard server.isLocal else {
            authError = "Account login is only available for the local server."
            return
        }
        do {
            authError = nil
            try await appModel.loginLocalChatGPTAccount(serverId: server.serverId)
        } catch ChatGPTOAuthError.cancelled {
            return
        } catch {
            authError = error.localizedDescription
        }
    }

    private func saveApiKey(_ key: String) async {
        guard server.isLocal else {
            authError = "API keys can only be saved for the local server."
            return
        }
        do {
            authError = nil
            try OpenAIApiKeyStore.shared.save(key)
            if case .apiKey? = server.account {
                _ = try await appModel.client.logoutAccount(serverId: server.serverId)
            }
            try await appModel.restartLocalServer()
            hasStoredApiKey = OpenAIApiKeyStore.shared.hasStoredKey
            guard hasStoredApiKey else {
                authError = "API key did not persist locally."
                return
            }
            dismiss()
        } catch {
            authError = error.localizedDescription
        }
    }

    private func logout() async {
        guard server.isLocal else {
            authError = "Account logout is only available for the local server."
            return
        }
        do {
            try? ChatGPTOAuthTokenStore.shared.clear()
            try? OpenAIApiKeyStore.shared.clear()
            _ = try await appModel.client.logoutAccount(serverId: server.serverId)
            try await appModel.restartLocalServer()
            authError = nil
        } catch {
            authError = error.localizedDescription
        }
    }
}

private struct AccountDisconnectedView: View {
    let dismiss: DismissAction

    var body: some View {
        NavigationStack {
            ZStack {
                RemoraTheme.backgroundGradient.ignoresSafeArea()
                VStack(spacing: 16) {
                    Text("Local Codex isn't running")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    Text("ChatGPT login and API key entry require the local Codex bridge.")
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textSecondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal, 24)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            }
            .navigationTitle("Account")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button("Done") { dismiss() }
                        .foregroundColor(RemoraTheme.accent)
                }
            }
        }
    }
}

#if DEBUG
#Preview("Account") {
    RemoraPreviewScene(includeBackground: false) {
        AccountView()
    }
}
#endif
