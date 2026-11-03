// swift-tools-version: 6.0
// Strata's menu bar app: a window onto `strata status`, with the everyday actions.
import PackageDescription

let package = Package(
    name: "StrataBar",
    platforms: [.macOS(.v14)],
    targets: [.executableTarget(name: "StrataBar")]
)
