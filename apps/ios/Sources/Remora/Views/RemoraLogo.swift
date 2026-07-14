import SwiftUI

struct RemoraLogo: View {
    var size: CGFloat

    var body: some View {
        Image(systemName: "waveform.path.ecg")
            .font(.system(size: size * 0.58, weight: .bold))
            .foregroundStyle(RemoraTheme.accent)
            .frame(width: size, height: size)
            .accessibilityHidden(true)
    }
}

#if DEBUG
#Preview("Brand Logo") {
    ZStack {
        RemoraTheme.backgroundGradient.ignoresSafeArea()
        RemoraLogo(size: 128)
    }
}
#endif
