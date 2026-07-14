import SwiftUI

/// Compact animated logo — wraps AnimatedSplashView in a fixed-size frame
/// with no background or tagline. Just the Remora mark.
struct AnimatedLogo: View {
    var size: CGFloat = 44

    var body: some View {
        AnimatedSplashView(appReady: true, compact: true) {}
            .frame(width: size, height: size)
            .clipped()
            .accessibilityHidden(true)
    }
}
