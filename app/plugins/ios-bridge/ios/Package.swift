// swift-tools-version:5.3

import PackageDescription

let package = Package(
  name: "tauri-plugin-anytrans-bridge",
  platforms: [
    .macOS(.v10_13),
    .iOS(.v14),
  ],
  products: [
    .library(
      name: "tauri-plugin-anytrans-bridge",
      type: .static,
      targets: ["tauri-plugin-anytrans-bridge"])
  ],
  dependencies: [
    .package(name: "Tauri", path: "../.tauri/tauri-api")
  ],
  targets: [
    .target(
      name: "tauri-plugin-anytrans-bridge",
      dependencies: [
        .byName(name: "Tauri")
      ],
      path: "Sources")
  ]
)
