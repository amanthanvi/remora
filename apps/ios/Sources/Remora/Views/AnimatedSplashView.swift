import SwiftUI

struct AnimatedSplashView: View {
    @State private var isPulsing = false

    let appReady: Bool
    var compact: Bool = false
    let onFinished: () -> Void

    var body: some View {
        ZStack {
            if !compact {
                RemoraTheme.backgroundGradient.ignoresSafeArea()
            }

            VStack(spacing: compact ? 0 : 18) {
                RemoraLogo(size: compact ? 92 : 164)
                    .scaleEffect(isPulsing ? 1.04 : 0.96)
                    .opacity(isPulsing ? 1 : 0.72)

                if !compact {
                    Text("Your agents, wherever you are")
                        .remoraMonoFont(size: 14, weight: .regular)
                        .foregroundStyle(RemoraTheme.textMuted)
                }
            }
        }
        .onAppear {
            withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: true)) {
                isPulsing = true
            }
            if appReady {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                    onFinished()
                }
            }
        }
    }
}

#if DEBUG
#Preview("Animated Splash") {
    AnimatedSplashView(appReady: true) {}
}
#endif
