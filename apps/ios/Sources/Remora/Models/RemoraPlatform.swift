import Foundation
import SwiftUI

enum RemoraPlatform {
#if targetEnvironment(macCatalyst)
    static let isCatalyst = true
#else
    static let isCatalyst = false
#endif

    /// AppKit-bridge workarounds apply both to Catalyst and to an iOS build
    /// running as a Mac app on Apple Silicon.
    static let rendersAsMacApp: Bool = {
        if isCatalyst { return true }
        return ProcessInfo.processInfo.isiOSAppOnMac
    }()

    static func isRegularSurface(horizontalSizeClass: UserInterfaceSizeClass?) -> Bool {
        isCatalyst || horizontalSizeClass == .regular
    }
}
