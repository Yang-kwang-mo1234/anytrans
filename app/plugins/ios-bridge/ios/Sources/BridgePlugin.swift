import Network
import Photos
import SwiftRs
import Tauri
import UIKit
import WebKit

/// Must match the app group in both targets' entitlements and in the share
/// extension's ShareViewController.
let appGroup = "group.com.anytrans.client"

struct SaveArgs: Decodable {
  let path: String
}

class BridgePlugin: Plugin {
  private let monitor = NWPathMonitor()

  override init() {
    super.init()
    // `currentPath` is only kept up to date while the monitor is running.
    monitor.start(queue: DispatchQueue.global(qos: .utility))
  }

  /// Where the share extension leaves what the user shared.
  @objc public func sharedInbox(_ invoke: Invoke) {
    let container = FileManager.default.containerURL(
      forSecurityApplicationGroupIdentifier: appGroup)
    if let dir = container?.appendingPathComponent("inbox", isDirectory: true) {
      invoke.resolve(["dir": dir.path])
    } else {
      invoke.resolve(["dir": NSNull()])
    }
  }

  @objc public func saveToPhotos(_ invoke: Invoke) throws {
    let args = try invoke.parseArgs(SaveArgs.self)
    let url = URL(fileURLWithPath: args.path)
    let videoExtensions: Set<String> = ["mov", "mp4", "m4v"]
    let type: PHAssetResourceType =
      videoExtensions.contains(url.pathExtension.lowercased()) ? .video : .photo

    PHPhotoLibrary.requestAuthorization(for: .addOnly) { status in
      guard status == .authorized || status == .limited else {
        invoke.reject("사진 보관함에 추가할 권한이 없습니다. 설정 앱에서 허용해 주세요.")
        return
      }
      PHPhotoLibrary.shared().performChanges({
        let options = PHAssetResourceCreationOptions()
        // Keep the original in the Files app as well.
        options.shouldMoveFile = false
        PHAssetCreationRequest.forAsset().addResource(with: type, fileURL: url, options: options)
      }) { saved, error in
        if saved {
          invoke.resolve()
        } else {
          invoke.reject(error?.localizedDescription ?? "사진 보관함에 저장하지 못했습니다.")
        }
      }
    }
  }

  @objc public func network(_ invoke: Invoke) {
    invoke.resolve(["expensive": monitor.currentPath.isExpensive])
  }
}

@_cdecl("init_plugin_anytrans_bridge")
func initPlugin() -> Plugin {
  return BridgePlugin()
}
