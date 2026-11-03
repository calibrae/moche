// swift-tools-version: 6.0
import PackageDescription

// Moche.xcframework is built by tools/ios/build-xcframework.sh (on the Apple build
// host), which copies it next to this file. It is not committed (see .gitignore).
let package = Package(
    name: "Moche",
    platforms: [.iOS("26.0")],
    products: [
        .library(name: "Moche", targets: ["Moche"]),
    ],
    targets: [
        // The C API (moche.h + module map `MocheFFI`) and the Rust static library.
        .binaryTarget(name: "MocheFFI", path: "Moche.xcframework"),
        // Thin Swift wrapper over the C API.
        .target(
            name: "Moche",
            dependencies: ["MocheFFI"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
        .testTarget(
            name: "MocheTests",
            dependencies: ["Moche"],
            swiftSettings: [.swiftLanguageMode(.v6)]
        ),
    ],
    swiftLanguageModes: [.v6]
)
