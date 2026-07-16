import SwiftUI

struct AnimatedSplashView: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var isPulsing = false

    let appReady: Bool
    var compact: Bool = false
    let onFinished: () -> Void

    var body: some View {
        ZStack {
            if !compact {
                RemoraTheme.background.ignoresSafeArea()
            }

            VStack(spacing: compact ? 0 : 18) {
                RemoraLogo(size: compact ? 92 : 164)
                    .scaleEffect(reduceMotion ? 1 : (isPulsing ? 1.04 : 0.96))
                    .opacity(reduceMotion ? 1 : (isPulsing ? 1 : 0.72))

                if !compact {
                    Text("Your agents, wherever you are")
                        .remoraMonoFont(size: 14, weight: .regular)
                        .foregroundStyle(RemoraTheme.textMuted)
                }
            }
        }
        .onAppear {
            if reduceMotion {
                isPulsing = false
            } else {
                withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: true)) {
                    isPulsing = true
                }
            }
            if appReady {
                DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) {
                    onFinished()
                }
            }
        }
        .onChange(of: reduceMotion) { _, shouldReduceMotion in
            if shouldReduceMotion {
                isPulsing = false
            } else {
                withAnimation(.easeInOut(duration: 1.1).repeatForever(autoreverses: true)) {
                    isPulsing = true
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
