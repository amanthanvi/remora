// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "Remora",
    platforms: [
        .iOS(.v26)
    ],
    products: [
        .library(name: "Remora", targets: ["Remora"])
    ],
    targets: [
        .binaryTarget(
            name: "codex_mobile_client",
            path: "apps/ios/Frameworks/codex_mobile_client.xcframework"
        ),
        .target(
            name: "Remora",
            dependencies: ["codex_mobile_client"],
            path: "apps/ios/Sources/Remora",
            publicHeadersPath: "Bridge"
        )
    ]
)
