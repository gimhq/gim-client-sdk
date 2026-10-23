// swift-tools-version: 6.0
import PackageDescription

// gim-core's UniFFI bindings for iOS and macOS, as built by
// bindings/apple/build-xcframework.sh (version in GIM_CORE_VERSION). Copy the
// build output directory (this file, GIMCore.xcframework.zip and Sources/) into
// the app and add it as a local package. `GIMCoreFFI` is the C module inside
// the static-library xcframework; `GIMCore` is the generated Swift API.
let package = Package(
    name: "GIMCore",
    platforms: [.iOS(.v18), .macOS(.v15)],
    products: [
        .library(name: "GIMCore", targets: ["GIMCore"])
    ],
    targets: [
        .binaryTarget(name: "GIMCoreFFI", path: "GIMCore.xcframework.zip"),
        .target(
            name: "GIMCore",
            dependencies: ["GIMCoreFFI"],
            swiftSettings: [.swiftLanguageMode(.v5)],
            // getrandom uses SecRandomCopyBytes; reqwest reads the system
            // proxy settings.
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("CoreFoundation"),
                .linkedLibrary("c++"),
                .linkedLibrary("resolv"),
            ]
        ),
    ]
)
