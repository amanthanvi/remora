import SwiftUI

/// Compact mascot for navigation and home chrome.
struct AnimatedLogo: View {
    var size: CGFloat = 44

    var body: some View {
        RemoraLogo(size: size)
            .accessibilityHidden(true)
    }
}
