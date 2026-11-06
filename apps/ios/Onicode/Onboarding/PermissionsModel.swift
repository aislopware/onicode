import AVFAudio
import UIKit
import UserNotifications

/// A permission onboarding asks for up front. Local Network is never asked
/// (engines are reached over Tailscale's VPN interface, which local-network
/// privacy doesn't cover); Camera and Photos are asked when first used
/// (docs/onicode/decisions/onboarding-grants.md).
enum Permission: CaseIterable, Hashable {
    case notifications
    case microphone
}

/// Where a permission stands, as the page shows it.
enum PermissionStatus: Equatable {
    case notDetermined
    case denied
    case granted
}

/// What a row's button does: ask (the system alert), send the person to
/// Settings (a refusal is only undone there), or nothing more (granted: a
/// check).
enum PermissionAction: Equatable {
    case ask
    case openSettings
    case done

    init(_ status: PermissionStatus) {
        switch status {
        case .notDetermined: self = .ask
        case .denied: self = .openSettings
        case .granted: self = .done
        }
    }
}

/// The system's doors for the two permissions, behind a protocol so tests
/// never prompt.
@MainActor
protocol PermissionSystem: AnyObject {
    func status(_ permission: Permission) async -> PermissionStatus
    /// Shows the system alert; the status after the person answers.
    func request(_ permission: Permission) async -> PermissionStatus
    func openSettings()
}

/// The real doors: UserNotifications and AVAudioApplication.
final class SystemPermissions: PermissionSystem {
    func status(_ permission: Permission) async -> PermissionStatus {
        switch permission {
        case .notifications:
            let settings = await UNUserNotificationCenter.current().notificationSettings()
            switch settings.authorizationStatus {
            case .notDetermined: return .notDetermined
            case .denied: return .denied
            default: return .granted // authorized, provisional, ephemeral
            }
        case .microphone:
            switch AVAudioApplication.shared.recordPermission {
            case .undetermined: return .notDetermined
            case .denied: return .denied
            default: return .granted
            }
        }
    }

    func request(_ permission: Permission) async -> PermissionStatus {
        switch permission {
        case .notifications:
            _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound, .badge])
        case .microphone:
            _ = await AVAudioApplication.requestRecordPermission()
        }
        return await status(permission)
    }

    func openSettings() {
        guard let url = URL(string: UIApplication.openSettingsURLString) else { return }
        UIApplication.shared.open(url)
    }
}

/// The permissions page's state. Nothing here prompts on its own: the system
/// alert comes only from `tap`, which only the person's tap on a row calls.
@MainActor
final class PermissionsModel {
    /// Set once the first-launch page is left with Continue.
    static let shownKey = "onboarding.permissionsShown"

    private let system: PermissionSystem
    private let defaults: UserDefaults
    /// Nil until first read.
    private(set) var statuses: [Permission: PermissionStatus] = [:]
    var onChange: (() -> Void)?

    init(system: PermissionSystem = SystemPermissions(), defaults: UserDefaults = .standard) {
        self.system = system
        self.defaults = defaults
    }

    /// Whether the page opens on this launch: once, until Continue, on a
    /// plain launch. Scripted launches (UI tests and screenshots pass `-`
    /// arguments; unit tests run in the app) skip it unless they pass
    /// `-onboarding`.
    static func presentsOnLaunch(
        arguments: [String], environment: [String: String], defaults: UserDefaults = .standard
    ) -> Bool {
        if arguments.contains("-onboarding") { return true }
        if arguments.dropFirst().contains(where: { $0.hasPrefix("-") }) { return false }
        if environment["XCTestConfigurationFilePath"] != nil { return false }
        return !defaults.bool(forKey: shownKey)
    }

    func markShown() {
        defaults.set(true, forKey: Self.shownKey)
    }

    /// Re-reads both statuses (on appear, and when the app comes back from
    /// Settings or a system alert).
    func refresh() async {
        for permission in Permission.allCases {
            statuses[permission] = await system.status(permission)
        }
        onChange?()
    }

    /// A row's button: the system alert if not yet asked, Settings if refused.
    func tap(_ permission: Permission) async {
        let current = if let known = statuses[permission] { known } else { await system.status(permission) }
        switch PermissionAction(current) {
        case .ask:
            statuses[permission] = await system.request(permission)
            onChange?()
        case .openSettings:
            system.openSettings()
        case .done:
            break
        }
    }
}
