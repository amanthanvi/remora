import Foundation
import Observation
import SwiftUI

enum PetAvatarState: Int {
    case idle = 0
    case runningRight = 1
    case runningLeft = 2
    case waving = 3
    case jumping = 4
    case failed = 5
    case waiting = 6
    case running = 7
    case review = 8
}

struct CachedPetPackage: Equatable {
    let serverId: String
    let id: String
    let displayName: String
    let spritesheetBytes: Data
}

/// The coarse runtime signals that can change the pet overlay.
///
/// This intentionally excludes thread content so streaming text does not
/// invalidate the app chrome when the pet's visible state is unchanged.
struct PetOverlayRuntimeSnapshot: Equatable {
    let hasPendingApprovals: Bool
    let hasPendingUserInputs: Bool
    let activeThreadFailed: Bool
    let activeThreadRunning: Bool
    let anyThreadRunning: Bool
    let anyThreadFailed: Bool
    let hasConnectedServer: Bool

    init(snapshot: AppSnapshotRecord) {
        let activeThread = snapshot.activeThread.flatMap { key in
            snapshot.threads.first(where: { $0.key == key })
        }
        hasPendingApprovals = !snapshot.pendingApprovals.isEmpty
        hasPendingUserInputs = !snapshot.pendingUserInputs.isEmpty
        activeThreadFailed = activeThread?.info.status == .systemError
        activeThreadRunning = activeThread?.hasActiveTurn == true
        anyThreadRunning = snapshot.threads.contains(where: \.hasActiveTurn)
        anyThreadFailed = snapshot.threads.contains { $0.info.status == .systemError }
        hasConnectedServer = snapshot.servers.contains(where: \.isConnected)
    }
}

@MainActor
@Observable
final class PetOverlayController {
    static let shared = PetOverlayController()

    static let minPetScale: CGFloat = 0.25
    static let maxPetScale: CGFloat = 5.0
    static let defaultPetScale: CGFloat = 1.0

    private let visibleKey = "remora.petOverlay.visible"
    private let serverIdKey = "remora.petOverlay.serverId"
    private let petIdKey = "remora.petOverlay.petId"
    private let petNameKey = "remora.petOverlay.petName"
    private let petScaleKey = "remora.petOverlay.petScale"

    private(set) var visible = false
    private(set) var selectedPet: CachedPetPackage?
    private(set) var isLoading = false
    private(set) var errorMessage: String?
    var dragOffset = CGSize(width: 24, height: 96)
    private(set) var isDragging = false
    private(set) var petScale: CGFloat = PetOverlayController.defaultPetScale
    private(set) var isPinching = false
    private var pinchInitialScale: CGFloat = PetOverlayController.defaultPetScale
    private var dragDirection = PetAvatarState.runningRight

    private init() {
        visible = UserDefaults.standard.bool(forKey: visibleKey)
        let savedScale = UserDefaults.standard.object(forKey: petScaleKey) as? Double
            ?? Double(Self.defaultPetScale)
        petScale = Self.clampScale(CGFloat(savedScale))
        guard let serverId = UserDefaults.standard.string(forKey: serverIdKey),
              let petId = UserDefaults.standard.string(forKey: petIdKey),
              let name = UserDefaults.standard.string(forKey: petNameKey),
              let data = try? Data(contentsOf: cacheURL(serverId: serverId, petId: petId))
        else { return }
        selectedPet = CachedPetPackage(
            serverId: serverId,
            id: petId,
            displayName: name,
            spritesheetBytes: data
        )
    }

    private static func clampScale(_ value: CGFloat) -> CGFloat {
        min(maxPetScale, max(minPetScale, value))
    }

    func setVisible(_ next: Bool) {
        visible = next
        UserDefaults.standard.set(next, forKey: visibleKey)
    }

    func selectPet(appModel: AppModel, serverId: String, pet: AppPetSummary) async {
        isLoading = true
        errorMessage = nil
        do {
            let package = try await appModel.client.loadPet(serverId: serverId, petId: pet.id)
            let cached = CachedPetPackage(
                serverId: serverId,
                id: package.summary.id,
                displayName: package.summary.displayName,
                spritesheetBytes: Data(package.spritesheetBytes)
            )
            try FileManager.default.createDirectory(
                at: cacheDirectory,
                withIntermediateDirectories: true
            )
            try cached.spritesheetBytes.write(to: cacheURL(serverId: serverId, petId: cached.id))
            UserDefaults.standard.set(serverId, forKey: serverIdKey)
            UserDefaults.standard.set(cached.id, forKey: petIdKey)
            UserDefaults.standard.set(cached.displayName, forKey: petNameKey)
            UserDefaults.standard.set(true, forKey: visibleKey)
            selectedPet = cached
            visible = true
        } catch {
            errorMessage = error.localizedDescription
        }
        isLoading = false
    }

    func startDrag() {
        isDragging = true
    }

    func dragBy(_ translation: CGSize) {
        dragOffset.width += translation.width
        dragOffset.height += translation.height
        if translation.width > 0.5 { dragDirection = .runningRight }
        if translation.width < -0.5 { dragDirection = .runningLeft }
    }

    func endDrag() {
        isDragging = false
    }

    func startPinch() {
        guard !isPinching else { return }
        pinchInitialScale = petScale
        isPinching = true
    }

    func pinchBy(_ factor: CGFloat) {
        guard factor.isFinite, factor > 0 else { return }
        petScale = Self.clampScale(pinchInitialScale * factor)
    }

    func endPinch() {
        isPinching = false
        UserDefaults.standard.set(Double(petScale), forKey: petScaleKey)
    }

    func setScale(_ value: CGFloat) {
        petScale = Self.clampScale(value)
        UserDefaults.standard.set(Double(petScale), forKey: petScaleKey)
    }

    func avatarState(snapshot: AppSnapshotRecord?) -> PetAvatarState {
        avatarState(runtime: snapshot.map { PetOverlayRuntimeSnapshot(snapshot: $0) })
    }

    func avatarState(runtime: PetOverlayRuntimeSnapshot?) -> PetAvatarState {
        if isLoading { return .waiting }
        if isDragging { return dragDirection }
        guard let runtime else { return .idle }
        if runtime.hasPendingApprovals || runtime.hasPendingUserInputs {
            return .review
        }
        if runtime.activeThreadFailed { return .failed }
        if runtime.activeThreadRunning || runtime.anyThreadRunning { return .running }
        if runtime.anyThreadFailed { return .failed }
        return runtime.hasConnectedServer ? .idle : .waiting
    }

    func avatarMessage(snapshot: AppSnapshotRecord?) -> String? {
        avatarMessage(runtime: snapshot.map { PetOverlayRuntimeSnapshot(snapshot: $0) })
    }

    func avatarMessage(runtime: PetOverlayRuntimeSnapshot?) -> String? {
        if isLoading { return "Fetching pet..." }
        if isDragging { return nil }
        guard let runtime else { return nil }
        if runtime.hasPendingApprovals { return "Review needed" }
        if runtime.hasPendingUserInputs { return "Input needed" }
        if runtime.activeThreadFailed { return "Run failed" }
        if runtime.activeThreadRunning || runtime.anyThreadRunning { return "Working..." }
        if runtime.anyThreadFailed { return "Thread failed" }
        return runtime.hasConnectedServer ? nil : "Waiting for server"
    }

    private var cacheDirectory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        return base.appendingPathComponent("Pets", isDirectory: true)
    }

    private func cacheURL(serverId: String, petId: String) -> URL {
        cacheDirectory.appendingPathComponent("\(safe(serverId))_\(safe(petId)).webp")
    }

    private func safe(_ value: String) -> String {
        value.map { char in
            char.isLetter || char.isNumber || char == "-" || char == "_" || char == "." ? char : "_"
        }.map(String.init).joined()
    }
}
