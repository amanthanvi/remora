import SwiftUI

struct RemoraLinkHostsSettingsView: View {
    let appModel: AppModel

    @State private var hosts: [AppRemoraLinkHostSummary] = []
    @State private var isLoading = true
    @State private var actionHostId: String?
    @State private var errorMessage: String?
    @State private var noticeMessage: String?
    @State private var confirmation: HostConfirmation?
    @State private var resumeHost: AppRemoraLinkHostSummary?
    @State private var showResumeSheet = false

    var body: some View {
        ZStack {
            Color(red: 2 / 255, green: 8 / 255, blue: 44 / 255).ignoresSafeArea()
            List {
                explanationSection
                hostsSection
            }
            .scrollContentBackground(.hidden)
            .refreshable { await loadHosts() }
        }
        .navigationTitle("Remora Link Hosts")
        .navigationBarTitleDisplayMode(.inline)
        .preferredColorScheme(.dark)
        .task { await loadHosts() }
        .sheet(isPresented: $showResumeSheet, onDismiss: {
            resumeHost = nil
            Task { await loadHosts() }
        }) {
            if let resumeHost {
                RemotePairingSheet(
                    appModel: appModel,
                    resumeHost: resumeHost,
                    onPaired: { _ in Task { await loadHosts() } }
                )
            }
        }
        .confirmationDialog(
            confirmation?.title ?? "Manage Host",
            isPresented: Binding(
                get: { confirmation != nil },
                set: { if !$0 { confirmation = nil } }
            ),
            titleVisibility: .visible
        ) {
            if let confirmation {
                switch confirmation.action {
                case .restart(let runtimeId):
                    Button("Restart \(runtimeId)", role: .destructive) {
                        self.confirmation = nil
                        Task { await restart(runtimeId, on: confirmation.host) }
                    }
                case .acknowledgeRestart:
                    Button("Acknowledge Checked Restart", role: .destructive) {
                        self.confirmation = nil
                        Task { await acknowledgeUnknownRestart(on: confirmation.host) }
                    }
                case .revoke:
                    Button("Revoke on Host", role: .destructive) {
                        self.confirmation = nil
                        Task { await revoke(confirmation.host) }
                    }
                case .forget:
                    Button("Forget Locally", role: .destructive) {
                        self.confirmation = nil
                        Task { await forget(confirmation.host) }
                    }
                }
            }
            Button("Cancel", role: .cancel) { confirmation = nil }
        } message: {
            if let confirmation {
                switch confirmation.action {
                case .restart(let runtimeId):
                    Text("Restarting \(runtimeId) can interrupt active work. Remora will send one durable restart request to this paired host.")
                case .acknowledgeRestart(let runtimeId, let commandSequence):
                    Text("Only acknowledge after checking \(runtimeId) on the host. This clears the unknown result for sequence \(commandSequence) and allows a later restart; it does not send a new restart.")
                case .revoke:
                    Text("Revoke asks the host to invalidate this device. The local record remains until cleanup finishes.")
                case .forget:
                    Text("Forget removes this host from this device. It does not guarantee that the host received a revocation.")
                }
            }
        }
        .alert("Remora Link", isPresented: Binding(
            get: { errorMessage != nil || noticeMessage != nil },
            set: {
                if !$0 {
                    errorMessage = nil
                    noticeMessage = nil
                }
            }
        )) {
            Button("OK") {
                errorMessage = nil
                noticeMessage = nil
            }
        } message: {
            Text(errorMessage ?? noticeMessage ?? "")
        }
    }

    private var explanationSection: some View {
        Section {
            Text("These are Remora Link trust relationships owned by the secure shared runtime. They are separate from saved SSH and Codex servers.")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.72))
        }
        .listRowBackground(Color.white.opacity(0.065))
    }

    private var hostsSection: some View {
        Section {
            if isLoading && hosts.isEmpty {
                HStack(spacing: 12) {
                    ProgressView().tint(linkCyan)
                    Text("Loading hosts…")
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(linkText.opacity(0.72))
                }
                .frame(minHeight: 44)
            } else if hosts.isEmpty {
                ContentUnavailableView(
                    "No Remora Link Hosts",
                    systemImage: "link.badge.plus",
                    description: Text("Pair a host from Add Server using its Remora Link code.")
                )
                .foregroundStyle(linkText)
                .listRowBackground(Color.clear)
            } else {
                ForEach(hosts, id: \.hostId) { host in
                    hostRow(host)
                }
            }
        } header: {
            Text("Trusted Hosts")
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.62))
        }
        .listRowBackground(Color.white.opacity(0.065))
    }

    private func hostRow(_ host: AppRemoraLinkHostSummary) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .top, spacing: 12) {
                Image(systemName: host.state.systemImage)
                    .foregroundStyle(host.state == .paired ? RemoraLinkVisualTokens.semanticSuccess : linkCyan)
                    .frame(width: 24, height: 24)
                    .accessibilityHidden(true)
                VStack(alignment: .leading, spacing: 3) {
                    Text(host.hostDisplayName)
                        .font(.system(.subheadline, design: .monospaced, weight: .bold))
                        .foregroundStyle(linkText)
                    Text(host.state.displayName)
                        .font(.system(.caption, design: .monospaced))
                        .foregroundStyle(linkText.opacity(0.62))
                    if !host.selectedRuntimeIds.isEmpty {
                        Text(host.selectedRuntimeIds.joined(separator: ", "))
                            .font(.system(.caption2, design: .monospaced))
                            .foregroundStyle(linkText.opacity(0.54))
                            .lineLimit(2)
                    }
                }
                Spacer()
                if actionHostId == host.hostId {
                    ProgressView().tint(linkCyan)
                }
            }

            if let pending = host.pendingApproval {
                VStack(alignment: .leading, spacing: 7) {
                    Text("Pending approval · security code \(pending.sas)")
                        .font(.system(.caption, design: .monospaced, weight: .semibold))
                        .foregroundStyle(linkCyan)
                    Button("Continue Approval") {
                        resumeHost = host
                        showResumeSheet = true
                    }
                    .font(.system(.footnote, design: .monospaced, weight: .semibold))
                    .foregroundStyle(linkCyan)
                    .frame(minHeight: 44)
                    .accessibilityHint("Reopens the pending pairing; closing it does not cancel")
                }
            }

            if host.hostRevocationStillRequired {
                Label("Host cleanup still required", systemImage: "exclamationmark.arrow.triangle.2.circlepath")
                    .font(.system(.caption, design: .monospaced, weight: .semibold))
                    .foregroundStyle(linkText)
                    .accessibilityLabel("Host cleanup is still required")
            }

            if let pendingRestart = host.pendingRestart {
                pendingRestartStatus(pendingRestart, host: host)
            }

            if host.state == .paired,
               host.grantedScopes.contains(.restartRuntime),
               !host.selectedRuntimeIds.isEmpty {
                runtimeRestartControls(host)
            }

            HStack(spacing: 18) {
                Button("Revoke") {
                    confirmation = HostConfirmation(action: .revoke, host: host)
                }
                .foregroundStyle(linkCyan)
                .disabled(actionHostId != nil)
                .accessibilityHint("Asks the host to invalidate this device")

                Button("Forget", role: .destructive) {
                    confirmation = HostConfirmation(action: .forget, host: host)
                }
                .disabled(actionHostId != nil)
                .accessibilityHint("Removes the local host record without guaranteeing host revocation")
            }
            .font(.system(.footnote, design: .monospaced, weight: .semibold))
            .frame(minHeight: 44)
        }
        .padding(.vertical, 6)
        .accessibilityElement(children: .contain)
    }

    private func pendingRestartStatus(
        _ pendingRestart: AppRemoraLinkPendingRestart,
        host: AppRemoraLinkHostSummary
    ) -> some View {
        VStack(alignment: .leading, spacing: 7) {
            Label(
                pendingRestart.outcomeUnknown ? "Restart outcome unknown" : "Restart request pending",
                systemImage: pendingRestart.outcomeUnknown
                    ? "exclamationmark.triangle.fill"
                    : "arrow.triangle.2.circlepath"
            )
            .font(.system(.caption, design: .monospaced, weight: .semibold))
            .foregroundStyle(linkWarning)

            Text("Runtime \(pendingRestart.runtimeId) · sequence \(pendingRestart.commandSequence)")
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.72))

            if pendingRestart.outcomeUnknown {
                Text("Check this runtime on the host before acknowledging. Do not retry while its result is unknown.")
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.72))

                Button("Acknowledge After Checking Host") {
                    confirmation = HostConfirmation(
                        action: .acknowledgeRestart(
                            runtimeId: pendingRestart.runtimeId,
                            commandSequence: pendingRestart.commandSequence
                        ),
                        host: host
                    )
                }
                .font(.system(.footnote, design: .monospaced, weight: .semibold))
                .foregroundStyle(linkCyan)
                .frame(minHeight: 44)
                .disabled(actionHostId != nil)
                .accessibilityHint("Clears the unknown restart result without sending another restart")
            } else {
                Text("Retry this runtime to resume the same durable request. Other runtimes remain blocked.")
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.72))
            }
        }
    }

    private func runtimeRestartControls(_ host: AppRemoraLinkHostSummary) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text("Runtime controls")
                .font(.system(.caption, design: .monospaced, weight: .semibold))
                .foregroundStyle(linkText.opacity(0.62))

            ForEach(Array(host.selectedRuntimeIds.enumerated()), id: \.offset) { _, runtimeId in
                Button {
                    confirmation = HostConfirmation(action: .restart(runtimeId: runtimeId), host: host)
                } label: {
                    HStack(spacing: 10) {
                        Label(runtimeId, systemImage: "arrow.triangle.2.circlepath")
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Spacer()
                        Text(restartButtonTitle(runtimeId, on: host))
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .font(.system(.footnote, design: .monospaced, weight: .semibold))
                .foregroundStyle(linkCyan)
                .frame(minHeight: 44)
                .disabled(restartIsDisabled(runtimeId, on: host))
                .accessibilityHint(restartAccessibilityHint(runtimeId, on: host))
            }
        }
    }

    private func restartButtonTitle(
        _ runtimeId: String,
        on host: AppRemoraLinkHostSummary
    ) -> String {
        guard let pendingRestart = host.pendingRestart,
              pendingRestart.runtimeId == runtimeId,
              !pendingRestart.outcomeUnknown else {
            return "Restart"
        }
        return "Retry"
    }

    private func restartIsDisabled(
        _ runtimeId: String,
        on host: AppRemoraLinkHostSummary
    ) -> Bool {
        guard actionHostId == nil else { return true }
        guard let pendingRestart = host.pendingRestart else { return false }
        return pendingRestart.outcomeUnknown || pendingRestart.runtimeId != runtimeId
    }

    private func restartAccessibilityHint(
        _ runtimeId: String,
        on host: AppRemoraLinkHostSummary
    ) -> String {
        guard let pendingRestart = host.pendingRestart else {
            return "Requires confirmation and may interrupt active work"
        }
        if pendingRestart.outcomeUnknown {
            return "Unavailable until the unknown restart result is checked and acknowledged"
        }
        if pendingRestart.runtimeId != runtimeId {
            return "Unavailable while another runtime has a pending restart"
        }
        return "Requires confirmation and retries the same durable restart request"
    }

    private func loadHosts() async {
        isLoading = true
        do {
            hosts = try await appModel.client.remoraLinkHosts()
                .filter { $0.state != .forgotten }
                .sorted {
                    $0.hostDisplayName.localizedCaseInsensitiveCompare($1.hostDisplayName) == .orderedAscending
                }
            errorMessage = nil
        } catch {
            errorMessage = hostActionMessage(error)
        }
        isLoading = false
    }

    private func restart(
        _ runtimeId: String,
        on host: AppRemoraLinkHostSummary
    ) async {
        actionHostId = host.hostId
        defer { actionHostId = nil }
        do {
            switch try await appModel.client.restartRemoraLinkRuntime(
                hostId: host.hostId,
                runtimeId: runtimeId
            ) {
            case .succeeded(let commandSequence):
                noticeMessage = "Restarted \(runtimeId) on \(host.hostDisplayName) · sequence \(commandSequence)."
            case .outcomeUnknown(let commandSequence):
                noticeMessage = "Restart outcome unknown for \(runtimeId) · sequence \(commandSequence). Check the host before acknowledging it."
            }
            await loadHosts()
        } catch {
            await loadHosts()
            errorMessage = hostActionMessage(error)
        }
    }

    private func acknowledgeUnknownRestart(on host: AppRemoraLinkHostSummary) async {
        actionHostId = host.hostId
        defer { actionHostId = nil }
        do {
            let commandSequence = try await appModel.client.acknowledgeRemoraLinkUnknownRestart(
                hostId: host.hostId
            )
            noticeMessage = "Acknowledged checked restart sequence \(commandSequence) on \(host.hostDisplayName). No new restart was sent."
            await loadHosts()
        } catch {
            await loadHosts()
            errorMessage = hostActionMessage(error)
        }
    }

    private func revoke(_ host: AppRemoraLinkHostSummary) async {
        actionHostId = host.hostId
        defer { actionHostId = nil }
        do {
            switch try await appModel.client.revokeRemoraLinkHost(hostId: host.hostId) {
            case .revoked:
                noticeMessage = "\(host.hostDisplayName) revoked this device."
            case .outcomeUnknown:
                noticeMessage = "The host's revocation result is unknown. Its cleanup status will remain visible until confirmed."
            }
            await loadHosts()
        } catch {
            errorMessage = hostActionMessage(error)
        }
    }

    private func forget(_ host: AppRemoraLinkHostSummary) async {
        actionHostId = host.hostId
        defer { actionHostId = nil }
        do {
            let result = try await appModel.client.forgetRemoraLinkHost(hostId: host.hostId)
            if result.hostRevocationStillRequired {
                noticeMessage = "Forgot locally. The host may still trust this device; revoke it from the host when possible."
            } else {
                noticeMessage = result.alreadyForgotten
                    ? "This host was already forgotten."
                    : "Forgot \(host.hostDisplayName) on this device."
            }
            await loadHosts()
        } catch {
            errorMessage = hostActionMessage(error)
        }
    }

    private func hostActionMessage(_ error: Error) -> String {
        if let error = error as? RemoraLinkError {
            switch error {
            case .NotConfigured: return "Remora Link secure storage is unavailable."
            case .HostUnavailable: return "The host is offline. You can forget it locally, or retry revocation when it is reachable."
            case .AuthorizationRequired: return "This pairing does not allow self-revocation. Revoke it on the host."
            case .OutcomeUnknown: return "The host may have completed the operation. Refresh the list before retrying."
            case .OperationInProgress: return "Another Remora Link operation is already in progress."
            default: return "The Remora Link host operation failed: \(String(describing: error))."
            }
        }
        return error.localizedDescription
    }

    private var linkCyan: Color { Color(red: 13 / 255, green: 213 / 255, blue: 240 / 255) }
    private var linkText: Color { Color(red: 234 / 255, green: 251 / 255, blue: 255 / 255) }
    private var linkWarning: Color { Color(red: 226 / 255, green: 166 / 255, blue: 68 / 255) }
}

private struct HostConfirmation {
    enum Action {
        case restart(runtimeId: String)
        case acknowledgeRestart(runtimeId: String, commandSequence: UInt64)
        case revoke
        case forget
    }

    let action: Action
    let host: AppRemoraLinkHostSummary

    var title: String {
        switch action {
        case .restart(let runtimeId): return "Restart \(runtimeId) on \(host.hostDisplayName)?"
        case .acknowledgeRestart: return "Acknowledge unknown restart?"
        case .revoke: return "Revoke \(host.hostDisplayName)?"
        case .forget: return "Forget \(host.hostDisplayName)?"
        }
    }
}

private extension AppRemoraLinkHostState {
    var displayName: String {
        switch self {
        case .inspecting: return "Inspecting"
        case .ready: return "Ready to pair"
        case .pairing: return "Pairing"
        case .awaitingHostApproval: return "Awaiting host approval"
        case .paired: return "Paired"
        case .revoking: return "Revoking"
        case .revoked: return "Revoked"
        case .forgetting: return "Forgetting"
        case .forgotten: return "Forgotten"
        case .needsRepair: return "Needs repair"
        }
    }

    var systemImage: String {
        switch self {
        case .paired: return "checkmark.shield.fill"
        case .awaitingHostApproval: return "person.badge.clock"
        case .needsRepair: return "wrench.and.screwdriver"
        case .revoked, .forgotten: return "link.badge.minus"
        default: return "link"
        }
    }
}
