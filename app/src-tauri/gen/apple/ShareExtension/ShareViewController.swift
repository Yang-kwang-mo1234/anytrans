import UIKit
import UniformTypeIdentifiers

/// Must match the app group in the entitlements and in the app's bridge plugin.
private let appGroup = "group.com.anytrans.client"

/// Receives what the user shares to AnyTrans from another app.
///
/// The extension does no networking. It copies the shared items into a folder
/// both it and the app can reach, then opens the app, which asks which device
/// to send them to.
class ShareViewController: UIViewController {
  override func viewDidAppear(_ animated: Bool) {
    super.viewDidAppear(animated)
    collect()
  }

  private func collect() {
    let container = FileManager.default.containerURL(
      forSecurityApplicationGroupIdentifier: appGroup)
    guard let inbox = container?.appendingPathComponent("inbox", isDirectory: true) else {
      finish()
      return
    }
    // One folder per share, so equal file names from two shares cannot clash.
    let batch = inbox.appendingPathComponent(UUID().uuidString, isDirectory: true)
    try? FileManager.default.createDirectory(at: batch, withIntermediateDirectories: true)

    let items = extensionContext?.inputItems as? [NSExtensionItem] ?? []
    let providers = items.flatMap { $0.attachments ?? [] }
    let pending = DispatchGroup()
    let names = NameAllocator()

    for provider in providers {
      guard let type = provider.registeredTypeIdentifiers.first else { continue }
      pending.enter()
      // The file handed to this callback is deleted when it returns, so the
      // copy has to happen inside it.
      provider.loadFileRepresentation(forTypeIdentifier: type) { url, _ in
        defer { pending.leave() }
        guard let url = url else { return }
        let destination = batch.appendingPathComponent(names.unique(url.lastPathComponent))
        try? FileManager.default.copyItem(at: url, to: destination)
      }
    }

    pending.notify(queue: .main) {
      self.openApp()
      self.finish()
    }
  }

  /// Extensions cannot open other apps directly, and the compiler refuses
  /// `UIApplication.open` in extension code. So this walks up to the host
  /// application object and calls that method by name. If that ever stops
  /// working the items are still waiting when the user opens AnyTrans by hand.
  private func openApp() {
    guard let url = URL(string: "anytrans://share") else { return }
    let selector = sel_registerName("openURL:options:completionHandler:")
    typealias Open = @convention(c) (AnyObject, Selector, NSURL, NSDictionary, AnyObject?) -> Void

    var responder: UIResponder? = self
    while let current = responder {
      if current is UIApplication, current.responds(to: selector) {
        let open = unsafeBitCast(current.method(for: selector), to: Open.self)
        open(current, selector, url as NSURL, NSDictionary(), nil)
        return
      }
      responder = current.next
    }
  }

  private func finish() {
    extensionContext?.completeRequest(returningItems: nil, completionHandler: nil)
  }
}

/// Hands out file names that are distinct within one share. Items load on
/// several threads at once.
private final class NameAllocator {
  private var used = Set<String>()
  private let lock = NSLock()

  func unique(_ name: String) -> String {
    lock.lock()
    defer { lock.unlock() }
    var candidate = name
    var counter = 1
    let base = (name as NSString).deletingPathExtension
    let ext = (name as NSString).pathExtension
    while used.contains(candidate) {
      candidate = ext.isEmpty ? "\(base) (\(counter))" : "\(base) (\(counter)).\(ext)"
      counter += 1
    }
    used.insert(candidate)
    return candidate
  }
}
