import SwiftUI

struct RemoraLogo: View {
    var size: CGFloat

    var body: some View {
        Image("remora_mascot")
            .resizable()
            .interpolation(.high)
            .antialiased(true)
            .scaledToFit()
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
