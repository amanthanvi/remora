import SwiftUI

struct RemoraTransmissionPressView<Content: View>: View {
    @State private var transmissionActive = false
    @ViewBuilder var content: () -> Content

    var body: some View {
        ZStack {
            if transmissionActive {
                TimelineView(.animation) { context in
                    let phase = context.date.timeIntervalSinceReferenceDate
                    RemoraLogo(size: 72)
                        .scaleEffect(0.9 + 0.08 * sin(phase * 5))
                        .opacity(0.78 + 0.18 * sin(phase * 4))
                }
            } else {
                content()
            }
        }
        .contentShape(Rectangle())
        .onLongPressGesture(
            minimumDuration: 0.5,
            maximumDistance: 12,
            pressing: { isPressing in
                if !isPressing { transmissionActive = false }
            },
            perform: { transmissionActive = true }
        )
        .onDisappear { transmissionActive = false }
    }
}
