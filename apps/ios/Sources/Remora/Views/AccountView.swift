import SwiftUI

struct AccountView: View {
    @Environment(AppModel.self) private var appModel
    @Environment(\.dismiss) private var dismiss

    private var server: AppServerSnapshot? {
        guard let snapshot = appModel.snapshot else { return nil }
        if let activeServerId = snapshot.activeThread?.serverId,
           let activeServer = snapshot.serverSnapshot(for: activeServerId),
           activeServer.isConnected,
           !activeServer.isLocal {
            return activeServer
        }
        return snapshot.servers.first(where: { $0.isConnected && !$0.isLocal })
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

    @State private var isWorking = false
    @State private var authError: String?

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
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
            }
            .task(id: server.serverId) {
                await refreshAccount()
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
                    Text(server.displayName)
                        .remoraFont(.caption)
                        .foregroundColor(RemoraTheme.textMuted)
                }
                Spacer()
                if server.account != nil {
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
        }
    }

    private var loginSection: some View {
        VStack(alignment: .leading, spacing: 16) {
            Text("LOGIN")
                .remoraFont(.caption)
                .foregroundColor(RemoraTheme.textMuted)
                .padding(.horizontal, 20)

            if !isChatGPTAccount {
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
        }
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
        do {
            authError = nil
            try await appModel.loginChatGPTAccount(serverId: server.serverId)
        } catch ChatGPTOAuthError.cancelled {
            return
        } catch {
            authError = error.localizedDescription
        }
    }

    private func logout() async {
        do {
            _ = try await appModel.client.logoutAccount(serverId: server.serverId)
            await appModel.refreshSnapshot()
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
                    Text("No server connected")
                        .remoraFont(.subheadline)
                        .foregroundColor(RemoraTheme.textPrimary)
                    Text("Connect a remote server to manage its ChatGPT account.")
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
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
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
