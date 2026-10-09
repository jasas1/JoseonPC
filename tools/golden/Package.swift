// swift-tools-version:6.0
// Makes the reference files in crates/joseon-core/tests/golden from the macOS app's own code.
// See README.md in this directory.
import PackageDescription

let package = Package(
    name: "golden",
    platforms: [.macOS("14.4")],
    targets: [
        // Sources/JoseonCore, Sources/JoseonHeadphones and Sources/JoseonRender are links to the macOS repo. They are not in this repository.
        // Built for testing, so the tool can reach what the macOS tests reach
        // (the sleep switch of the engine).
        .target(name: "JoseonCore", swiftSettings: [.swiftLanguageMode(.v5), .unsafeFlags(["-enable-testing"])]),
        .target(name: "JoseonHeadphones", dependencies: ["JoseonCore"], swiftSettings: [.swiftLanguageMode(.v5)]),
        .target(name: "JoseonRender", dependencies: ["JoseonCore"], swiftSettings: [.swiftLanguageMode(.v5), .unsafeFlags(["-enable-testing"])]),
        .executableTarget(name: "golden", dependencies: ["JoseonCore", "JoseonHeadphones", "JoseonRender"], swiftSettings: [.swiftLanguageMode(.v5)]),
    ]
)
