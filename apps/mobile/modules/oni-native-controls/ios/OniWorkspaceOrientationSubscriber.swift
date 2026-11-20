import ExpoModulesCore
import UIKit

public final class OniWorkspaceOrientationSubscriber: ExpoAppDelegateSubscriber {
  private static let hingeWindows = NSHashTable<UIWindow>.weakObjects()

  /// A screen that reads better sideways, such as the remote desktop, is open. Main thread only.
  static var freeRotation = false {
    didSet {
      guard freeRotation != oldValue else { return }
      for case let scene as UIWindowScene in UIApplication.shared.connectedScenes {
        for window in scene.windows {
          window.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations()
        }
      }
    }
  }

  static func allowHingeOrientations(in window: UIWindow) {
    guard !hingeWindows.contains(window) else { return }
    hingeWindows.add(window)
    window.rootViewController?.setNeedsUpdateOfSupportedInterfaceOrientations()
  }

  public func application(_ application: UIApplication, supportedInterfaceOrientationsFor window: UIWindow?) -> UIInterfaceOrientationMask {
    if UIDevice.current.userInterfaceIdiom == .pad {
      let orientations = Bundle.main.object(forInfoDictionaryKey: "UISupportedInterfaceOrientations~ipad") as? [String] ?? []
      return orientations == ["UIInterfaceOrientationPortrait"] ? .portrait : .all
    }
    if Self.freeRotation { return .allButUpsideDown }
    return window.map { Self.hingeWindows.contains($0) } == true ? .allButUpsideDown : .portrait
  }
}
