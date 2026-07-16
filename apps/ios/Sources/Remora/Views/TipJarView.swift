import SwiftUI
import StoreKit

struct TipJarView: View {
    private var store: TipJarStore { TipJarStore.shared }

    var body: some View {
        ZStack {
            RemoraTheme.backgroundGradient.ignoresSafeArea()

            Form {
                headerSection

                if store.isLoading {
                    Section {
                        ProgressView()
                            .frame(maxWidth: .infinity)
                            .listRowBackground(RemoraTheme.surface.opacity(0.6))
                    }
                } else {
                    tipsSection
                    if !store.purchasedTiers.isEmpty {
                        headerBadgeSelectionSection
                    }
                    restoreSection
                }

                if store.purchaseState == .purchased {
                    thankYouSection
                }

                if case .failed(let message) = store.purchaseState {
                    Section {
                        Text(message)
                            .remoraFont(.caption)
                            .foregroundColor(RemoraTheme.danger)
                            .listRowBackground(RemoraTheme.surface.opacity(0.6))
                    }
                }
            }
            .scrollContentBackground(.hidden)

            if store.purchaseState == .purchasing {
                Color.black.opacity(0.3).ignoresSafeArea()
                ProgressView()
                    .tint(RemoraTheme.accent)
                    .scaleEffect(1.2)
            }
        }
        .navigationTitle("Support Remora")
        .navigationBarTitleDisplayMode(.inline)
        .task {
            await store.loadProducts()
        }
    }

    private var headerSection: some View {
        Section {
            VStack(spacing: 8) {
                if let tier = store.supporterTier {
                    SupportBadgeIcon(name: tier.icon, size: 120)
                    Text("You're a supporter! Thank you.")
                        .remoraFont(.subheadline, weight: .semibold)
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                } else {
                    Image(systemName: "heart.fill")
                        .font(.system(size: 28))
                        .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                }
                Text("If you enjoy Remora, consider leaving a tip. Tips help support ongoing development and are entirely optional.")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textSecondary)
                    .multilineTextAlignment(.center)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 8)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        }
    }

    private var tipsSection: some View {
        Section {
            ForEach(store.tiers) { tier in
                if tier.isPurchased {
                    HStack(spacing: 12) {
                        SupportBadgeIcon(name: tier.icon, size: 48)
                        Text(tier.displayName)
                            .remoraFont(.subheadline)
                            .foregroundColor(RemoraTheme.textPrimary)
                        Spacer()
                        Image(systemName: "checkmark.circle.fill")
                            .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                    }
                    .padding(.vertical, 4)
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
                } else {
                    Button {
                        Task { await store.purchase(tier) }
                    } label: {
                        HStack(spacing: 12) {
                            SupportBadgeIcon(name: tier.icon, size: 48)
                            Text(tier.displayName)
                                .remoraFont(.subheadline)
                                .foregroundColor(RemoraTheme.textPrimary)
                            Spacer()
                            Text(tier.displayPrice)
                                .remoraFont(.subheadline, weight: .semibold)
                                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        }
                    }
                    .padding(.vertical, 4)
                    .disabled(store.purchaseState == .purchasing)
                    .listRowBackground(RemoraTheme.surface.opacity(0.6))
                }
            }
        } header: {
            Text("Support Remora")
                .foregroundColor(RemoraTheme.textSecondary)
        }
    }

    private var headerBadgeSelectionSection: some View {
        Section {
            ForEach(store.purchasedTiers) { tier in
                Button {
                    store.setHeaderBadge(tier, selected: !store.isHeaderBadgeSelected(tier))
                } label: {
                    HStack(spacing: 12) {
                        SupportBadgeIcon(name: tier.icon, size: 44)
                        VStack(alignment: .leading, spacing: 2) {
                            Text(tier.displayName)
                                .remoraFont(.subheadline)
                                .foregroundColor(RemoraTheme.textPrimary)
                            Text(store.isHeaderBadgeSelected(tier) ? "Shown on home" : "Hidden from home")
                                .remoraFont(.caption)
                                .foregroundColor(RemoraTheme.textSecondary)
                        }
                        Spacer()
                        Image(systemName: store.isHeaderBadgeSelected(tier) ? "checkmark.circle.fill" : "circle")
                            .font(.system(size: 20, weight: .semibold))
                            .foregroundColor(
                                store.isHeaderBadgeSelected(tier)
                                    ? RemoraTheme.accentForegroundOnSurface
                                    : RemoraTheme.textMuted
                            )
                    }
                }
                .buttonStyle(.plain)
                .padding(.vertical, 4)
                .listRowBackground(RemoraTheme.surface.opacity(0.6))
            }
        } header: {
            Text("Home Header")
                .foregroundColor(RemoraTheme.textSecondary)
        } footer: {
            Text("Pick which support badges appear around the home logo.")
                .foregroundColor(RemoraTheme.textMuted)
        }
    }

    private var restoreSection: some View {
        Section {
            Button {
                Task { await store.restorePurchases() }
            } label: {
                Text("Restore Purchases")
                    .remoraFont(.subheadline)
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                    .frame(maxWidth: .infinity)
            }
            .disabled(store.purchaseState == .purchasing)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        }
    }

    private var thankYouSection: some View {
        Section {
            VStack(spacing: 6) {
                Text("Thank you!")
                    .remoraFont(.subheadline, weight: .semibold)
                    .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                Text("Your support means a lot.")
                    .remoraFont(.caption)
                    .foregroundColor(RemoraTheme.textSecondary)
            }
            .frame(maxWidth: .infinity)
            .padding(.vertical, 4)
            .listRowBackground(RemoraTheme.surface.opacity(0.6))
        }
        .transition(.opacity)
    }
}

struct SupporterBadge: View {
    @State private var showTipJar = false

    var body: some View {
        let store = TipJarStore.shared
        Button { showTipJar = true } label: {
            if let tier = store.supporterTier {
                SupportBadgeIcon(name: tier.icon, size: 36)
                    .frame(
                        width: RemoraAccessibilityMetrics.minimumHitTarget,
                        height: RemoraAccessibilityMetrics.minimumHitTarget
                    )
            } else {
                Image(systemName: "heart.fill")
                    .font(.system(size: 14))
                    .foregroundColor(RemoraTheme.textMuted)
                    .frame(
                        width: RemoraAccessibilityMetrics.minimumHitTarget,
                        height: RemoraAccessibilityMetrics.minimumHitTarget
                    )
            }
        }
        .accessibilityLabel("Open tip jar")
        .accessibilityValue(store.supporterTier?.displayName ?? "No supporter badge")
        .task { await store.loadProducts() }
        .sheet(isPresented: $showTipJar) {
            NavigationStack {
                TipJarView()
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Done") { showTipJar = false }
                                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        }
                    }
            }
        }
    }
}

/// Renders support badges for a tier range (e.g. 0..<2 = lower tiers,
/// 2..<4 = higher tiers) next to the home logo. Collapses to nothing for
/// ranges with no purchased tiers. `loadProducts` is called by the host
/// screen so this stays a pure read.
enum SupporterBadgesPresentation {
    case expanded
    case compact
}

struct SupporterBadges: View {
    let tierIndices: Range<Int>
    var presentation: SupporterBadgesPresentation = .expanded
    @State private var showTipJar = false

    var body: some View {
        let store = TipJarStore.shared
        let purchased = store.tiers.enumerated()
            .filter { tierIndices.contains($0.offset) && store.isHeaderBadgeSelected($0.element) }
            .map(\.element)

        Group {
            if !purchased.isEmpty {
                switch presentation {
                case .expanded:
                    HStack(spacing: 2) {
                        ForEach(purchased, id: \.id) { tier in
                            Button { showTipJar = true } label: {
                                SupportBadgeIcon(name: tier.icon, size: 28)
                                    .frame(
                                        width: RemoraAccessibilityMetrics.minimumHitTarget,
                                        height: RemoraAccessibilityMetrics.minimumHitTarget
                                    )
                                    .contentShape(Rectangle())
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel("Open tip jar")
                            .accessibilityValue(tier.displayName)
                        }
                    }
                case .compact:
                    if let highestTier = purchased.last {
                        Button { showTipJar = true } label: {
                            ZStack(alignment: .bottomTrailing) {
                                SupportBadgeIcon(name: highestTier.icon, size: 28)
                                if purchased.count > 1 {
                                    Text("\(purchased.count)")
                                        .font(.caption2.bold())
                                        .foregroundStyle(RemoraTheme.textOnAccent)
                                        .frame(width: 17, height: 17)
                                        .background(Circle().fill(RemoraTheme.accent))
                                        .accessibilityHidden(true)
                                }
                            }
                            .frame(
                                width: RemoraAccessibilityMetrics.minimumHitTarget,
                                height: RemoraAccessibilityMetrics.minimumHitTarget
                            )
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Open tip jar")
                        .accessibilityValue(
                            purchased.count == 1
                                ? highestTier.displayName
                                : "\(purchased.count) selected support badges"
                        )
                    }
                }
            }
        }
        .sheet(isPresented: $showTipJar) {
            NavigationStack {
                TipJarView()
                    .toolbar {
                        ToolbarItem(placement: .topBarTrailing) {
                            Button("Done") { showTipJar = false }
                                .foregroundColor(RemoraTheme.accentForegroundOnSurface)
                        }
                    }
            }
        }
    }
}

private struct SupportBadgeIcon: View {
    let name: String
    let size: CGFloat

    var body: some View {
        Image(systemName: name)
            .font(.system(size: size * 0.46, weight: .semibold))
            .foregroundStyle(RemoraTheme.accentForegroundOnSurface)
            .frame(width: size * 0.9, height: size * 0.9)
            .frame(width: size, height: size)
            .modifier(GlassCircleModifier())
    }
}
