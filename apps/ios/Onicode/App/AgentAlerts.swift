import UIKit
import UserNotifications

/// Agent alerts as local notifications (docs/onicode/decisions/alerts.md). The core decides them
/// while the app runs, in front or briefly behind; this posts and withdraws them and runs a
/// button through the core. A suspended app posts nothing: there is no push.
final class AgentAlerts: NSObject, UNUserNotificationCenterDelegate {
    static let shared = AgentAlerts()

    /// Opens a chat on screen; set by the scene once its shell exists.
    var open: ((String) -> Void)? {
        didSet { flushOpens() }
    }

    private var shown: [String: AgentAlert] = [:]
    private var pendingOpen: String?
    /// How long a button's command has to reach its computer when the app is behind.
    private static let deliveryWindow: TimeInterval = 25

    /// From `application(_:didFinishLaunchingWithOptions:)`: before launch ends, so the response
    /// that launched the app reaches the delegate.
    func install() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.setNotificationCategories(Set(alertCategories().map(Self.category)))
    }

    private static func category(_ category: AlertCategory) -> UNNotificationCategory {
        let actions: [UNNotificationAction] = category.actions.map { button in
            if button.textInput {
                return UNTextInputNotificationAction(
                    identifier: button.id, title: button.title, options: [],
                    textInputButtonTitle: "Send", textInputPlaceholder: "Reply to the agent")
            }
            return UNNotificationAction(
                identifier: button.id, title: button.title, options: button.destructive ? [.destructive] : [])
        }
        return UNNotificationCategory(identifier: category.id, actions: actions, intentIdentifiers: [], options: [])
    }

    /// Bring the posted notifications to `alerts`: post the new or changed, take down the rest.
    func sync(_ alerts: [AgentAlert]) {
        let center = UNUserNotificationCenter.current()
        let wanted = Dictionary(alerts.map { ($0.chatId, $0) }, uniquingKeysWith: { _, newer in newer })
        let gone = shown.keys.filter { wanted[$0] == nil }
        if !gone.isEmpty {
            center.removeDeliveredNotifications(withIdentifiers: gone)
            center.removePendingNotificationRequests(withIdentifiers: gone)
        }
        for alert in alerts where shown[alert.chatId] != alert {
            let content = UNMutableNotificationContent()
            content.title = alert.title
            content.body = alert.body
            content.categoryIdentifier = alert.category
            content.threadIdentifier = alert.chatId
            content.userInfo = ["alert": alert.payload]
            content.sound = .default
            content.interruptionLevel = alert.urgent ? .timeSensitive : .active
            center.add(UNNotificationRequest(identifier: alert.chatId, content: content, trigger: nil)) { error in
                if let error { NSLog("agent alert not shown: \(error)") }
            }
        }
        shown = wanted
    }

    // MARK: UNUserNotificationCenterDelegate

    /// The core raises nothing for the chat on screen, so whatever arrives in front shows.
    func userNotificationCenter(
        _: UNUserNotificationCenter, willPresent _: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    func userNotificationCenter(
        _: UNUserNotificationCenter, didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let request = response.notification.request
        let chatId = request.identifier
        let payload = request.content.userInfo["alert"] as? String
        let action = response.actionIdentifier
        let text = (response as? UNTextInputNotificationResponse)?.userText
        DispatchQueue.main.async {
            defer { completionHandler() }
            switch action {
            case UNNotificationDefaultActionIdentifier:
                self.pendingOpen = chatId
                self.flushOpens()
            case UNNotificationDismissActionIdentifier:
                break
            default:
                guard let payload else { return }
                self.act(payload: payload, action: action, text: text)
            }
        }
    }

    /// Runs a button without opening the app: the command is durable in the chat's document, and
    /// a background task keeps the app up while it reaches the chat's computer.
    private func act(payload: String, action: String, text: String?) {
        let app = AppModel.shared
        guard let client = app.client else { return }
        var task = UIBackgroundTaskIdentifier.invalid
        let end = {
            guard task != .invalid else { return }
            UIApplication.shared.endBackgroundTask(task)
            task = .invalid
        }
        task = UIApplication.shared.beginBackgroundTask(withName: "Answer an agent", expirationHandler: end)
        do {
            try client.actOnAlert(payload: payload, action: action, text: text)
            app.noteDeliveryActivity()
        } catch {
            NSLog("agent alert action \(action) failed: \(error)")
        }
        DispatchQueue.main.asyncAfter(deadline: .now() + Self.deliveryWindow, execute: end)
    }

    private func flushOpens() {
        guard let chatId = pendingOpen, let open else { return }
        pendingOpen = nil
        open(chatId)
    }
}
