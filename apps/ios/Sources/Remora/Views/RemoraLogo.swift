import SwiftUI
import UIKit

struct RemoraLogo: View {
    var size: CGFloat

    private var bundledLogo: UIImage? {
        UIImage(named: "remora_logo")
    }

    var body: some View {
        if let bundledLogo {
            Image(uiImage: bundledLogo)
                .resizable()
                .interpolation(.high)
                .scaledToFit()
                .frame(width: size, height: size)
                .accessibilityHidden(true)
        } else {
            Text("remora")
                .remoraMonoFont(size: size * 0.32, weight: .bold)
                .foregroundColor(RemoraTheme.accent)
        }
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
