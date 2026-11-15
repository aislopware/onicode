import ExpoModulesCore
import ObjectiveC
import UIKit

public final class OniTimerClockSleepSubscriber: ExpoAppDelegateSubscriber {
  public func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
  ) -> Bool {
    OniTimerClockSleep.install()
    return true
  }
}

/// React Native's timer clock (`RCTTiming`) stays on the display link while any timer is due
/// within a second, so a label that ticks each second wakes the JS thread on every frame.
/// This hands the clock to its own sleep timer whenever the next timer is further than
/// `nearTimer` away. React Native ships prebuilt, so the one-second constant can't be patched.
enum OniTimerClockSleep {
  /// Display frames the timer clock has handled. Written on the JS thread; the behavior probe reads it there.
  nonisolated(unsafe) static var frames = 0

  private static let nearTimer: TimeInterval = 0.05
  private static let didUpdateFrame = NSSelectorFromString("didUpdateFrame:")
  private static let scheduleSleepTimer = NSSelectorFromString("scheduleSleepTimer:")

  static func install() {
    guard let timing = NSClassFromString("RCTTiming"),
      let method = class_getInstanceMethod(timing, didUpdateFrame)
    else { return }
    // Every private name is checked first: a React Native that renamed one keeps its stock clock,
    // and the frame count then shows it.
    let canSleep =
      class_getInstanceMethod(timing, scheduleSleepTimer) != nil
      && NSClassFromString("_RCTTimer").map {
        class_getInstanceMethod($0, NSSelectorFromString("target")) != nil
      } == true
      && ["_timers", "_paused", "_sendIdleEvents", "_inBackground"].allSatisfy({ (name: String) in
        class_getInstanceVariable(timing, name) != nil
      })
    typealias DidUpdateFrame = @convention(c) (NSObject, Selector, AnyObject?) -> Void
    let original = unsafeBitCast(method_getImplementation(method), to: DidUpdateFrame.self)
    let replacement: @convention(block) (NSObject, AnyObject?) -> Void = { timing, update in
      original(timing, didUpdateFrame, update)
      frames &+= 1
      if canSleep { sleepUntilNextTimer(timing) }
    }
    method_setImplementation(method, imp_implementationWithBlock(replacement))
  }

  private static func sleepUntilNextTimer(_ timing: NSObject) {
    guard timing.value(forKey: "paused") as? Bool == false,
      timing.value(forKey: "sendIdleEvents") as? Bool == false,
      timing.value(forKey: "inBackground") as? Bool == false,
      let timers = timing.value(forKey: "timers") as? NSDictionary
    else { return }
    objc_sync_enter(timers)
    let targets = timers.allValues.map { ($0 as? NSObject)?.value(forKey: "target") as? Date }
    objc_sync_exit(timers)
    guard let next = targets.compactMap({ $0 }).min(), !targets.contains(nil),
      next.timeIntervalSinceNow > nearTimer
    else { return }
    _ = timing.perform(scheduleSleepTimer, with: next)
    timing.setValue(true, forKey: "paused")
  }
}
