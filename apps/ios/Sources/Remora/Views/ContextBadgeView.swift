import SwiftUI

struct ContextBadgeView: View, Equatable {
    let percent: Int
    let tint: Color
    let metricLabel: String

    @Environment(\.textScale) private var textScale
    @ScaledMetric(relativeTo: .body) private var badgeWidth: CGFloat = 35
    @ScaledMetric(relativeTo: .body) private var badgeHeight: CGFloat = 16
    @ScaledMetric(relativeTo: .body) private var cornerRadius: CGFloat = 3.5
    @ScaledMetric(relativeTo: .body) private var strokeWidth: CGFloat = 1.2
    @ScaledMetric(relativeTo: .body) private var inset: CGFloat = 1.5

    init(percent: Int, tint: Color, metricLabel: String = "Context remaining") {
        self.percent = Self.clampedPercent(percent)
        self.tint = tint
        self.metricLabel = metricLabel
    }

    static func clampedPercent(_ percent: Int) -> Int {
        min(max(percent, 0), 100)
    }

    static func == (lhs: Self, rhs: Self) -> Bool {
        lhs.percent == rhs.percent
            && lhs.tint == rhs.tint
            && lhs.metricLabel == rhs.metricLabel
    }

    private var appScale: CGFloat { max(textScale, 0.1) }

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: cornerRadius * appScale)
                .stroke(tint.opacity(0.4), lineWidth: strokeWidth * appScale)

            GeometryReader { geo in
                let scaledInset = inset * appScale
                let scaledStrokeWidth = strokeWidth * appScale
                let inner = geo.size.width - (scaledInset + scaledStrokeWidth) * 2
                RoundedRectangle(cornerRadius: max(0, (cornerRadius - inset) * appScale))
                    .fill(tint.opacity(0.25))
                    .frame(width: max(0, inner * CGFloat(percent) / 100.0))
                    .padding(.leading, scaledInset + scaledStrokeWidth / 2)
                    .frame(maxHeight: .infinity, alignment: .center)
            }
            .padding(.vertical, (inset + strokeWidth / 2) * appScale)

            Text("\(percent)")
                .remoraMonoFont(size: 9.5, weight: .heavy)
                .foregroundColor(tint)
        }
        .frame(width: badgeWidth * appScale, height: badgeHeight * appScale)
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(metricLabel)
        .accessibilityValue("\(percent) percent")
    }
}
