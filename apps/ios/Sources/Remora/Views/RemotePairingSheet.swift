import AVFoundation
import SwiftUI
import UIKit

enum RemoraLinkVisualTokens {
    static var semanticSuccess: Color {
        RemoraPalette.success.color(for: .dark)
    }
}

struct RemotePairingSheet: View {
    static let pairCommand = "remora-link pair --qr --runtime codex"

    static func supportsQRScanning(rendersAsMacApp: Bool) -> Bool {
        !rendersAsMacApp
    }

    let appModel: AppModel
    let startScanningOnAppear: Bool
    let resumeHost: AppRemoraLinkHostSummary?
    let onPaired: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var model: RemoraLinkPairingModel
    @State private var showScanner = false
    @State private var didRequestInitialScan = false
    @State private var cameraDenied = false
    @State private var copiedCommand = false
    @State private var reportedSuccessHostId: String?

    init(
        appModel: AppModel,
        startScanningOnAppear: Bool = false,
        resumeHost: AppRemoraLinkHostSummary? = nil,
        onPaired: @escaping (String) -> Void = { _ in }
    ) {
        self.appModel = appModel
        self.startScanningOnAppear = startScanningOnAppear
        self.resumeHost = resumeHost
        self.onPaired = onPaired
        _model = State(initialValue: RemoraLinkPairingModel(client: appModel.client))
    }

    var body: some View {
        NavigationStack {
            ZStack {
                Color(red: 2 / 255, green: 8 / 255, blue: 44 / 255)
                    .ignoresSafeArea()
                ScrollView {
                    VStack(alignment: .leading, spacing: 18) {
                        header
                        stateContent
                    }
                    .padding(20)
                    .frame(maxWidth: 620)
                    .frame(maxWidth: .infinity)
                }
                .scrollIndicators(.hidden)
            }
            .navigationTitle("Remora Link")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    Button(closeButtonTitle) { dismiss() }
                        .foregroundStyle(linkCyan)
                        .frame(minWidth: 44, minHeight: 44)
                        .accessibilityHint(closeAccessibilityHint)
                }
            }
        }
        .preferredColorScheme(.dark)
        .onAppear {
            model.updateAvailability(AppRuntimeController.shared.remoraLinkStatus)
            if let resumeHost, let pending = resumeHost.pendingApproval {
                model.resume(hostId: resumeHost.hostId, pendingApproval: pending)
            } else {
                requestInitialScanIfNeeded()
            }
        }
        .onChange(of: AppRuntimeController.shared.remoraLinkStatus) { _, status in
            model.updateAvailability(status)
            requestInitialScanIfNeeded()
        }
        .onChange(of: model.state) { _, state in
            guard case .success(let success) = state,
                  reportedSuccessHostId != success.hostId else { return }
            reportedSuccessHostId = success.hostId
            onPaired(success.hostId)
        }
        .fullScreenCover(isPresented: $showScanner) {
            QRScannerScreen(
                onScan: { scanned in
                    showScanner = false
                    model.inspect(codeText: scanned)
                },
                onCancel: { showScanner = false },
                onPermissionDenied: {
                    showScanner = false
                    cameraDenied = true
                }
            )
        }
        .alert("Camera Access Needed", isPresented: $cameraDenied) {
            Button("Open Settings") { openAppSettings() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("Allow camera access in Settings to scan a Remora Link pairing code.")
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack(spacing: 12) {
                RemoraLogo(size: 48)
                VStack(alignment: .leading, spacing: 3) {
                    Text("Connect a host")
                        .font(.system(.title2, design: .monospaced, weight: .bold))
                        .foregroundStyle(linkText)
                    Text("Private, end-to-end Remora Link")
                        .font(.system(.footnote, design: .monospaced))
                        .foregroundStyle(linkText.opacity(0.68))
                }
            }
            commandCard
        }
    }

    private var commandCard: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("On the host, run")
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.68))
            HStack(spacing: 10) {
                Text(Self.pairCommand)
                    .font(.system(.footnote, design: .monospaced, weight: .semibold))
                    .foregroundStyle(linkText)
                    .textSelection(.enabled)
                Spacer(minLength: 8)
                Button {
                    UIPasteboard.general.string = Self.pairCommand
                    copiedCommand = true
                    Task { @MainActor in
                        try? await Task.sleep(for: .seconds(1.4))
                        copiedCommand = false
                    }
                } label: {
                    Image(systemName: copiedCommand ? "checkmark" : "doc.on.doc")
                        .frame(width: 44, height: 44)
                }
                .buttonStyle(.plain)
                .foregroundStyle(linkCyan)
                .accessibilityLabel(copiedCommand ? "Pairing command copied" : "Copy pairing command")
            }
            Text("Replace codex with an ID from remora-link agents, or repeat --runtime to authorize more than one harness.")
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.68))
        }
        .padding(16)
        .background(cardBackground)
    }

    @ViewBuilder
    private var stateContent: some View {
        switch model.state {
        case .availability(let availability):
            availabilityView(availability)
        case .ingress:
            ingressView
        case .inspecting:
            progressCard(title: "Checking code", detail: "Authenticating the host invitation…")
        case .offer(let offer):
            offerView(offer)
        case .accepting:
            progressCard(title: "Starting pairing", detail: "Creating this device's secure host credential…")
        case .awaiting(let pending):
            awaitingView(pending)
        case .cancelling:
            progressCard(title: "Cancelling", detail: "Stopping this pending enrollment…")
        case .outcomeUnknown(let message):
            outcomeUnknownView(message)
        case .success(let success):
            successView(success)
        case .failure(let message):
            failureView(message)
        }
    }

    private var ingressView: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Use either method")
                .font(.system(.headline, design: .monospaced))
                .foregroundStyle(linkText)
            Text(ingressExplanation)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.68))

            ingressButtons(
                scanTitle: "Scan QR",
                pasteTitle: "Paste Code",
                pasteAction: inspectClipboardCode
            )
        }
        .padding(18)
        .background(cardBackground)
    }

    private func availabilityView(_ availability: RemoraLinkPairingAvailability) -> some View {
        VStack(alignment: .leading, spacing: 12) {
            switch availability {
            case .configuring:
                ProgressView().tint(linkCyan)
                Text("Preparing secure device storage…")
            case .unavailable:
                Image(systemName: "lock.trianglebadge.exclamationmark")
                    .font(.title2)
                    .foregroundStyle(linkText)
                Text("Remora Link is unavailable")
                    .font(.system(.headline, design: .monospaced))
                Text("Secure device storage could not be configured. Reopen Remora or try again after unlocking this device.")
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.68))
            }
        }
        .foregroundStyle(linkText)
        .frame(maxWidth: .infinity, alignment: .leading)
        .padding(18)
        .background(cardBackground)
        .accessibilityElement(children: .combine)
    }

    private func offerView(_ offer: AppRemoraLinkOffer) -> some View {
        VStack(alignment: .leading, spacing: 18) {
            VStack(alignment: .leading, spacing: 4) {
                Text(offer.hostDisplayName)
                    .font(.system(.title3, design: .monospaced, weight: .bold))
                    .foregroundStyle(linkText)
                Text("Authenticated host offer")
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.65))
            }

            VStack(alignment: .leading, spacing: 7) {
                Text("THIS DEVICE")
                    .sectionCaptionStyle()
                TextField("Device name", text: $model.deviceDisplayName)
                    .font(.system(.body, design: .monospaced))
                    .textInputAutocapitalization(.words)
                    .autocorrectionDisabled()
                    .padding(.horizontal, 12)
                    .frame(minHeight: 48)
                    .background(Color.white.opacity(0.08), in: RoundedRectangle(cornerRadius: 10))
                    .accessibilityLabel("This device's display name")
                Text("\(model.deviceNameByteCount)/80 bytes")
                    .font(.system(.caption2, design: .monospaced))
                    .foregroundStyle(model.deviceNameByteCount <= 80 ? linkText.opacity(0.55) : linkText)
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("RUNTIMES")
                    .sectionCaptionStyle()
                ForEach(offer.runtimeOffers, id: \.runtimeId) { runtime in
                    selectionRow(
                        title: runtime.displayName,
                        subtitle: runtime.recommended ? "Recommended" : nil,
                        selected: model.selectedRuntimeIds.contains(runtime.runtimeId),
                        enabled: runtime.available
                    ) { model.toggleRuntime(runtime.runtimeId) }
                }
            }

            VStack(alignment: .leading, spacing: 8) {
                Text("PERMISSIONS")
                    .sectionCaptionStyle()
                ForEach(offer.maximumScopes, id: \.self) { scope in
                    selectionRow(
                        title: scope.displayName,
                        subtitle: offer.requiredScopes.contains(scope) ? "Required by host" : scope.explanation,
                        selected: model.selectedScopes.contains(scope),
                        enabled: !offer.requiredScopes.contains(scope)
                    ) { model.toggleScope(scope) }
                }
            }

            if offer.confirmationMode == .interactive {
                Label("The host will ask you to compare and approve a security code.", systemImage: "checkmark.shield")
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.72))
            }

            actionButton("Pair This Device", systemImage: "link") { model.acceptOffer() }
                .disabled(!model.canAcceptOffer)
                .opacity(model.canAcceptOffer ? 1 : 0.45)
        }
        .padding(18)
        .background(cardBackground)
    }

    private func awaitingView(_ pending: RemoraLinkPendingPairing) -> some View {
        VStack(alignment: .leading, spacing: 14) {
            Label("Waiting for host approval", systemImage: "person.badge.shield.checkmark")
                .font(.system(.headline, design: .monospaced))
                .foregroundStyle(linkText)
            Text("Compare this security code with the one shown on the host:")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.72))
            Text(pending.sas)
                .font(.system(.largeTitle, design: .monospaced, weight: .bold))
                .tracking(3)
                .foregroundStyle(linkCyan)
                .frame(maxWidth: .infinity)
                .padding(.vertical, 16)
                .background(Color.black.opacity(0.25), in: RoundedRectangle(cornerRadius: 12))
                .accessibilityLabel("Security code \(pending.sas)")
            ProgressView().tint(linkCyan).frame(maxWidth: .infinity)
            Text("You can close this sheet and continue later from Settings › Remora Link Hosts. Closing is not cancellation.")
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.72))
            Button("Cancel Pairing", role: .destructive) { model.cancelPairing() }
                .font(.system(.body, design: .monospaced, weight: .semibold))
                .frame(maxWidth: .infinity, minHeight: 44)
                .accessibilityHint("Cancels the pending enrollment; closing this sheet does not")
        }
        .padding(18)
        .background(cardBackground)
    }

    private func outcomeUnknownView(_ message: String) -> some View {
        messageCard(
            icon: "questionmark.diamond",
            title: "Outcome unknown",
            message: message,
            color: linkText,
            actionTitle: "Check Hosts"
        ) { dismiss() }
    }

    private func successView(_ success: RemoraLinkPairingSuccess) -> some View {
        messageCard(
            icon: "checkmark.seal.fill",
            title: success.wasAlreadyPaired ? "Already paired" : "Host paired",
            message: "\(success.selectedRuntimeIds.count) runtime\(success.selectedRuntimeIds.count == 1 ? "" : "s") available through Remora Link.",
            color: RemoraLinkVisualTokens.semanticSuccess,
            actionTitle: "Done"
        ) { dismiss() }
    }

    private func failureView(_ message: String) -> some View {
        messageCard(
            icon: "exclamationmark.triangle",
            title: "Pairing couldn't continue",
            message: message,
            color: linkText,
            actionTitle: "Try Another Code"
        ) { model.startOver() }
    }

    private func progressCard(title: String, detail: String) -> some View {
        VStack(spacing: 14) {
            ProgressView().tint(linkCyan).controlSize(.large)
            Text(title)
                .font(.system(.headline, design: .monospaced))
                .foregroundStyle(linkText)
            Text(detail)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.68))
                .multilineTextAlignment(.center)
        }
        .frame(maxWidth: .infinity)
        .padding(24)
        .background(cardBackground)
        .accessibilityElement(children: .combine)
    }

    private func messageCard(
        icon: String,
        title: String,
        message: String,
        color: Color,
        actionTitle: String,
        action: @escaping () -> Void
    ) -> some View {
        VStack(spacing: 14) {
            Image(systemName: icon).font(.system(size: 34)).foregroundStyle(color)
            Text(title)
                .font(.system(.headline, design: .monospaced))
                .foregroundStyle(linkText)
            Text(message)
                .font(.system(.footnote, design: .monospaced))
                .foregroundStyle(linkText.opacity(0.72))
                .multilineTextAlignment(.center)
            actionButton(actionTitle, systemImage: nil, action: action)
        }
        .frame(maxWidth: .infinity)
        .padding(20)
        .background(cardBackground)
        .accessibilityElement(children: .contain)
    }

    private func primaryIngressButton(
        title: String,
        systemImage: String,
        accessibilityHint: String,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            VStack(spacing: 8) {
                Image(systemName: systemImage).font(.title2)
                Text(title).font(.system(.subheadline, design: .monospaced, weight: .semibold))
            }
            .foregroundStyle(Color(red: 2 / 255, green: 8 / 255, blue: 44 / 255))
            .frame(maxWidth: .infinity, minHeight: 88)
            .background(linkCyan, in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
        .accessibilityHint(accessibilityHint)
    }

    @ViewBuilder
    private func ingressButtons(
        scanTitle: String,
        pasteTitle: String,
        pasteAction: @escaping () -> Void
    ) -> some View {
        if Self.supportsQRScanning(rendersAsMacApp: RemoraPlatform.rendersAsMacApp) {
            HStack(spacing: 12) {
                primaryIngressButton(
                    title: scanTitle,
                    systemImage: "qrcode.viewfinder",
                    accessibilityHint: "Opens the camera to scan a Remora Link pairing code"
                ) { requestCameraAndScan() }
                primaryIngressButton(
                    title: pasteTitle,
                    systemImage: "doc.on.clipboard",
                    accessibilityHint: "Inspects the Remora Link code currently on the clipboard",
                    action: pasteAction
                )
            }
        } else {
            VStack(alignment: .leading, spacing: 10) {
                Text("QR scanning isn't available in the Mac app. Paste the pairing code from the host instead.")
                    .font(.system(.footnote, design: .monospaced))
                    .foregroundStyle(linkText.opacity(0.72))
                    .fixedSize(horizontal: false, vertical: true)
                primaryIngressButton(
                    title: pasteTitle,
                    systemImage: "doc.on.clipboard",
                    accessibilityHint: "Inspects the Remora Link code currently on the clipboard",
                    action: pasteAction
                )
            }
        }
    }

    private func selectionRow(
        title: String,
        subtitle: String?,
        selected: Bool,
        enabled: Bool,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack(spacing: 12) {
                Image(systemName: selected ? "checkmark.square.fill" : "square")
                    .foregroundStyle(selected ? linkCyan : linkText.opacity(0.5))
                    .font(.title3)
                VStack(alignment: .leading, spacing: 2) {
                    Text(title)
                        .font(.system(.subheadline, design: .monospaced, weight: .semibold))
                    if let subtitle {
                        Text(subtitle)
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(linkText.opacity(0.62))
                    }
                }
                Spacer()
                if !enabled && !selected {
                    Text("Unavailable")
                        .font(.system(.caption2, design: .monospaced))
                        .foregroundStyle(linkText.opacity(0.5))
                }
            }
            .foregroundStyle(linkText)
            .frame(minHeight: 44)
            .contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .disabled(!enabled)
        .accessibilityValue(selected ? "Selected" : "Not selected")
    }

    private func actionButton(
        _ title: String,
        systemImage: String?,
        action: @escaping () -> Void
    ) -> some View {
        Button(action: action) {
            HStack {
                if let systemImage { Image(systemName: systemImage) }
                Text(title)
            }
            .font(.system(.body, design: .monospaced, weight: .bold))
            .foregroundStyle(Color(red: 2 / 255, green: 8 / 255, blue: 44 / 255))
            .frame(maxWidth: .infinity, minHeight: 48)
            .background(linkCyan, in: RoundedRectangle(cornerRadius: 12))
        }
        .buttonStyle(.plain)
    }

    private var closeButtonTitle: String {
        if case .awaiting = model.state { return "Close" }
        return "Cancel"
    }

    private var closeAccessibilityHint: String {
        if case .awaiting = model.state {
            return "Closes this sheet without cancelling the pending pairing"
        }
        return "Closes Remora Link pairing"
    }

    private var linkCyan: Color { Color(red: 13 / 255, green: 213 / 255, blue: 240 / 255) }
    private var linkText: Color { Color(red: 234 / 255, green: 251 / 255, blue: 255 / 255) }
    private var cardBackground: some ShapeStyle { Color.white.opacity(0.065) }

    private var ingressExplanation: String {
        if Self.supportsQRScanning(rendersAsMacApp: RemoraPlatform.rendersAsMacApp) {
            return "QR scanning and clipboard paste follow the same authenticated inspection path."
        }
        return "Paste the host's pairing code to follow the authenticated inspection path."
    }

    private func inspectClipboardCode() {
        model.startOver()
        guard let code = UIPasteboard.general.string else {
            model.inspect(codeText: "")
            return
        }
        model.inspect(codeText: code)
    }

    private func requestInitialScanIfNeeded() {
        guard !RemoraPlatform.rendersAsMacApp,
              startScanningOnAppear,
              !didRequestInitialScan,
              case .ingress = model.state else { return }
        didRequestInitialScan = true
        Task { @MainActor in
            await Task.yield()
            requestCameraAndScan()
        }
    }

    private func requestCameraAndScan() {
        guard !RemoraPlatform.rendersAsMacApp else { return }
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized:
            showScanner = true
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .video) { granted in
                Task { @MainActor in
                    if granted { showScanner = true } else { cameraDenied = true }
                }
            }
        case .denied, .restricted:
            cameraDenied = true
        @unknown default:
            cameraDenied = true
        }
    }

    private func openAppSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}

private extension Text {
    func sectionCaptionStyle() -> some View {
        font(.system(.caption, design: .monospaced, weight: .bold))
            .foregroundStyle(Color(red: 234 / 255, green: 251 / 255, blue: 255 / 255).opacity(0.6))
            .tracking(1.2)
    }
}

private extension AppRemoraLinkScope {
    var displayName: String {
        switch self {
        case .inspectRuntimes: return "View runtimes"
        case .connectRuntime: return "Connect to runtimes"
        case .restartRuntime: return "Restart runtimes"
        case .selfRevoke: return "Revoke this device"
        }
    }

    var explanation: String {
        switch self {
        case .inspectRuntimes: return "See runtimes offered by this host"
        case .connectRuntime: return "Open sessions on selected runtimes"
        case .restartRuntime: return "Restart an unavailable runtime"
        case .selfRevoke: return "Ask the host to invalidate this device"
        }
    }
}

// MARK: - QR Scanner

private struct QRScannerScreen: View {
    let onScan: (String) -> Void
    let onCancel: () -> Void
    let onPermissionDenied: () -> Void

    @State private var copied = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            QRCaptureSheet(onScan: onScan, onCancel: onCancel, onPermissionDenied: onPermissionDenied)
                .ignoresSafeArea()
            LinearGradient(
                colors: [Color.black.opacity(0.62), .clear],
                startPoint: .top,
                endPoint: .bottom
            )
            .frame(height: 340)
            .frame(maxHeight: .infinity, alignment: .top)
            .ignoresSafeArea()
            .allowsHitTesting(false)

            VStack(spacing: 16) {
                HStack {
                    Spacer()
                    Button("Cancel", action: onCancel)
                        .font(.system(.body, design: .monospaced, weight: .semibold))
                        .foregroundStyle(.white)
                        .frame(minWidth: 64, minHeight: 44)
                        .background(.black.opacity(0.5), in: Capsule())
                }
                instructionsCard
                Spacer()
                Text("Hold steady — the QR code is detected automatically.")
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.white.opacity(0.8))
                    .multilineTextAlignment(.center)
                    .padding(10)
                    .background(.black.opacity(0.5), in: Capsule())
            }
            .padding(16)
        }
    }

    private var instructionsCard: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Pair with Remora Link")
                .font(.system(.headline, design: .monospaced, weight: .bold))
            Text("Run this on the host, then scan its QR code:")
                .font(.system(.caption, design: .monospaced))
                .foregroundStyle(.white.opacity(0.78))
            HStack(spacing: 10) {
                Text(RemotePairingSheet.pairCommand)
                    .font(.system(.footnote, design: .monospaced, weight: .semibold))
                    .frame(maxWidth: .infinity, alignment: .leading)
                Button {
                    UIPasteboard.general.string = RemotePairingSheet.pairCommand
                    copied = true
                } label: {
                    Image(systemName: copied ? "checkmark" : "doc.on.doc")
                        .frame(width: 44, height: 44)
                }
                .accessibilityLabel(copied ? "Pairing command copied" : "Copy pairing command")
            }
            Text("Replace codex with an ID from remora-link agents, or repeat --runtime for multiple harnesses.")
                .font(.system(.caption2, design: .monospaced))
                .foregroundStyle(.white.opacity(0.78))
        }
        .foregroundStyle(.white)
        .padding(16)
        .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 14))
    }
}

private struct QRCaptureSheet: UIViewControllerRepresentable {
    let onScan: (String) -> Void
    let onCancel: () -> Void
    let onPermissionDenied: () -> Void

    func makeUIViewController(context: Context) -> QRScannerViewController {
        let controller = QRScannerViewController()
        controller.onScan = onScan
        controller.onPermissionDenied = onPermissionDenied
        return controller
    }

    func updateUIViewController(_ uiViewController: QRScannerViewController, context: Context) {}
}

private final class QRScannerViewController: UIViewController, AVCaptureMetadataOutputObjectsDelegate {
    var onScan: ((String) -> Void)?
    var onPermissionDenied: (() -> Void)?

    private let captureSession = AVCaptureSession()
    private var previewLayer: AVCaptureVideoPreviewLayer?
    private let metadataQueue = DispatchQueue(label: "com.remora.remora-link.qr-scanner")
    private var didReportScan = false

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = .black
        configureSession()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        guard !captureSession.isRunning else { return }
        DispatchQueue.global(qos: .userInitiated).async { [weak self] in
            self?.captureSession.startRunning()
        }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        if captureSession.isRunning { captureSession.stopRunning() }
    }

    override func viewDidLayoutSubviews() {
        super.viewDidLayoutSubviews()
        previewLayer?.frame = view.layer.bounds
    }

    private func configureSession() {
        guard let device = AVCaptureDevice.default(for: .video),
              let input = try? AVCaptureDeviceInput(device: device),
              captureSession.canAddInput(input) else {
            onPermissionDenied?()
            return
        }
        captureSession.addInput(input)

        let output = AVCaptureMetadataOutput()
        guard captureSession.canAddOutput(output) else {
            onPermissionDenied?()
            return
        }
        captureSession.addOutput(output)
        output.setMetadataObjectsDelegate(self, queue: metadataQueue)
        if output.availableMetadataObjectTypes.contains(.qr) { output.metadataObjectTypes = [.qr] }

        let preview = AVCaptureVideoPreviewLayer(session: captureSession)
        preview.videoGravity = .resizeAspectFill
        view.layer.addSublayer(preview)
        previewLayer = preview
    }

    func metadataOutput(
        _ output: AVCaptureMetadataOutput,
        didOutput metadataObjects: [AVMetadataObject],
        from connection: AVCaptureConnection
    ) {
        guard !didReportScan,
              let payload = metadataObjects
                .compactMap({ $0 as? AVMetadataMachineReadableCodeObject })
                .first(where: { $0.type == .qr })?.stringValue else { return }
        didReportScan = true
        DispatchQueue.main.async { [weak self] in
            self?.captureSession.stopRunning()
            self?.onScan?(payload)
        }
    }
}
