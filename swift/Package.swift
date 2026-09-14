// swift-tools-version: 6.0
import PackageDescription

// Swift 6 language mode is the enforced bar for this template: complete
// concurrency checking is ON by default, so data races are compile errors,
// not TSan surprises. Every target opts in explicitly so a future edit that
// drops the package tools-version can't silently relax the bar.
let swift6: [SwiftSetting] = [.swiftLanguageMode(.v6)]

let package = Package(
    name: "Banshee",
    platforms: [.macOS(.v14)],
    products: [
        .library(name: "BansheeCore", targets: ["BansheeCore"]),
        .library(name: "DesignKit", targets: ["DesignKit"]),
    ],
    dependencies: [
        // Auto-update (banshee-u0a5-sparkle). Sparkle ships as a binary
        // xcframework over SPM; build-app.sh embeds Sparkle.framework into the
        // hand-assembled bundle and signs it with the hardened runtime, since a
        // SwiftPM build (no Xcode "embed frameworks" phase) does not do that.
        .package(url: "https://github.com/sparkle-project/Sparkle", from: "2.9.6"),
    ],
    targets: [
        // Design tokens — colors, spacing, typography. No app logic.
        .target(
            name: "DesignKit",
            path: "Sources/DesignKit",
            swiftSettings: swift6
        ),
        // Shared library — API client, wire models, server supervision, and
        // the PressureModel view model (here, not in the app target, so it is
        // unit-testable at the network boundary).
        .target(
            name: "BansheeCore",
            path: "Sources/BansheeCore",
            swiftSettings: swift6
        ),
        // macOS SwiftUI app.
        .executableTarget(
            name: "Banshee",
            dependencies: [
                "BansheeCore",
                "DesignKit",
                .product(name: "Sparkle", package: "Sparkle"),
            ],
            path: "Sources/Banshee",
            swiftSettings: swift6
        ),
        .testTarget(
            name: "BansheeCoreTests",
            dependencies: ["BansheeCore"],
            path: "Tests/BansheeCoreTests",
            swiftSettings: swift6
        ),
        .testTarget(
            name: "DesignKitTests",
            dependencies: ["DesignKit"],
            path: "Tests/DesignKitTests",
            swiftSettings: swift6
        ),
    ]
)
