import SwiftUI

struct ConversationComposerContextBarView: View {
    @Environment(\.horizontalSizeClass) private var horizontalSizeClass
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize
    let rateLimits: RateLimitSnapshot?
    let contextPercent: Int64?

    var body: some View {
        Group {
            if dynamicTypeSize.isAccessibilitySize {
                overflowSafeBadges
            } else {
                ViewThatFits(in: .horizontal) {
                    HStack(spacing: 4) {
                        badges
                    }
                    .fixedSize(horizontal: true, vertical: false)

                    VStack(alignment: .trailing, spacing: 4) {
                        badges
                    }

                    overflowSafeBadges
                }
            }
        }
        // Keep the composer chrome height stable even when no badges are available.
        .frame(maxWidth: .infinity, minHeight: 16, alignment: .trailing)
        .padding(.horizontal, 12)
        .padding(.top, -2)
        .padding(.trailing, RemoraPlatform.isRegularSurface(horizontalSizeClass: horizontalSizeClass) ? 12 : 40)
    }

    private var overflowSafeBadges: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                badges
            }
            .frame(maxWidth: .infinity, alignment: .trailing)
        }
        .scrollBounceBehavior(.basedOnSize, axes: .horizontal)
        .accessibilityLabel("Usage limits")
    }

    @ViewBuilder
    private var badges: some View {
        if let primary = rateLimits?.primary {
            RateLimitBadgeView(
                label: formatWindowLabel(primary),
                percent: normalizedPercent(primary.usedPercent)
            )
        }

        if let secondary = rateLimits?.secondary {
            RateLimitBadgeView(
                label: formatWindowLabel(secondary),
                percent: normalizedPercent(secondary.usedPercent)
            )
        }

        if let contextPercent {
            ContextBadgeView(
                percent: Int(contextPercent),
                tint: contextTint(percent: contextPercent)
            )
        }
    }

    private func normalizedPercent(_ raw: Int32) -> Int {
        let used = min(Int(raw), 100)
        return max(0, 100 - used)
    }

    private func formatWindowLabel(_ window: RateLimitWindow) -> String {
        guard let mins = window.windowDurationMins else { return "" }
        if mins >= 1440 { return "\(mins / 1440)d" }
        if mins >= 60 { return "\(mins / 60)h" }
        return "\(mins)m"
    }

    private func contextTint(percent: Int64) -> Color {
        switch percent {
        case ...15: return RemoraTheme.danger
        case ...35: return RemoraTheme.warning
        default: return RemoraTheme.success
        }
    }
}
