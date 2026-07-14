import SwiftUI
import UIKit

private enum RemoraTransmissionFrames {
    static let names = [
        "remora_transmission_01",
        "remora_transmission_02",
        "remora_transmission_03",
        "remora_transmission_04",
        "remora_transmission_05",
        "remora_transmission_06",
    ]

    static let frameDurationMs: UInt64 = 82
    static let holdDelaySeconds: Double = 0.5
    static let holdMaxDistance: CGFloat = 12
}

struct RemoraTransmissionPressView<Content: View>: View {
    @State private var transmissionActive = false

    @ViewBuilder var content: () -> Content

    var body: some View {
        ZStack {
            if transmissionActive {
                RemoraTransmissionFramePlayer()
            } else {
                content()
            }
        }
        .contentShape(Rectangle())
        .onLongPressGesture(
            minimumDuration: RemoraTransmissionFrames.holdDelaySeconds,
            maximumDistance: RemoraTransmissionFrames.holdMaxDistance,
            pressing: { isPressing in
                if !isPressing {
                    stopHold()
                }
            },
            perform: {
                transmissionActive = true
            }
        )
        .onDisappear {
            stopHold()
        }
    }

    private func stopHold() {
        transmissionActive = false
    }
}

private struct RemoraTransmissionFramePlayer: View {
    @State private var frameIndex = 0

    var body: some View {
        ZStack {
            if let image = UIImage(named: RemoraTransmissionFrames.names[frameIndex]) {
                Image(uiImage: image)
                    .resizable()
                    .interpolation(.none)
                    .scaledToFill()
            }
        }
        .clipped()
        .task {
            frameIndex = 0
            while !Task.isCancelled {
                try? await Task.sleep(for: .milliseconds(RemoraTransmissionFrames.frameDurationMs))
                frameIndex = (frameIndex + 1) % RemoraTransmissionFrames.names.count
            }
        }
    }
}
