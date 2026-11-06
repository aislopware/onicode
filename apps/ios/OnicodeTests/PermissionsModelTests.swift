import XCTest

@testable import Onicode

/// The permissions page asks only on a tap, sends a refusal to Settings, and
/// opens once on a plain launch. A fake stands in for the system, so nothing
/// prompts.
@MainActor
final class PermissionsModelTests: XCTestCase {
    private final class FakeSystem: PermissionSystem {
        var current: [Permission: PermissionStatus] = [:]
        /// What the person answers in the system alert.
        var answer = PermissionStatus.granted
        var requested: [Permission] = []
        var settingsOpened = 0

        func status(_ permission: Permission) -> PermissionStatus { current[permission] ?? .notDetermined }

        func request(_ permission: Permission) -> PermissionStatus {
            requested.append(permission)
            current[permission] = answer
            return answer
        }

        func openSettings() { settingsOpened += 1 }
    }

    private func defaults() -> UserDefaults {
        let name = "PermissionsModelTests.\(UUID().uuidString)"
        let defaults = UserDefaults(suiteName: name) ?? .standard
        defaults.removePersistentDomain(forName: name)
        return defaults
    }

    func testEachStatusHasOneAction() {
        XCTAssertEqual(PermissionAction(.notDetermined), .ask)
        XCTAssertEqual(PermissionAction(.denied), .openSettings)
        XCTAssertEqual(PermissionAction(.granted), .done)
    }

    func testRefreshReadsWithoutAsking() async {
        let system = FakeSystem()
        system.current = [.notifications: .denied, .microphone: .granted]
        let model = PermissionsModel(system: system, defaults: defaults())
        var changes = 0
        model.onChange = { changes += 1 }
        await model.refresh()
        XCTAssertEqual(model.statuses, [.notifications: .denied, .microphone: .granted])
        XCTAssertEqual(changes, 1)
        XCTAssertTrue(system.requested.isEmpty)
        XCTAssertEqual(system.settingsOpened, 0)
    }

    func testTapAsksOnceWhenNotDetermined() async {
        let system = FakeSystem()
        system.answer = .denied
        let model = PermissionsModel(system: system, defaults: defaults())
        await model.refresh()
        await model.tap(.microphone)
        XCTAssertEqual(system.requested, [.microphone])
        XCTAssertEqual(model.statuses[.microphone], .denied)
        // Refused: the next tap goes to Settings, never a second alert.
        await model.tap(.microphone)
        XCTAssertEqual(system.requested, [.microphone])
        XCTAssertEqual(system.settingsOpened, 1)
    }

    func testTapOnGrantedDoesNothing() async {
        let system = FakeSystem()
        system.current = [.notifications: .granted]
        let model = PermissionsModel(system: system, defaults: defaults())
        await model.tap(.notifications)
        XCTAssertTrue(system.requested.isEmpty)
        XCTAssertEqual(system.settingsOpened, 0)
    }

    func testOpensOnceOnAPlainLaunch() {
        let defaults = defaults()
        let model = PermissionsModel(system: FakeSystem(), defaults: defaults)
        let plain = ["Onicode"]
        XCTAssertTrue(PermissionsModel.presentsOnLaunch(arguments: plain, environment: [:], defaults: defaults))
        XCTAssertFalse(
            PermissionsModel.presentsOnLaunch(arguments: plain + ["-demo"], environment: [:], defaults: defaults))
        XCTAssertFalse(
            PermissionsModel.presentsOnLaunch(
                arguments: plain, environment: ["XCTestConfigurationFilePath": "x"], defaults: defaults))
        model.markShown()
        XCTAssertFalse(PermissionsModel.presentsOnLaunch(arguments: plain, environment: [:], defaults: defaults))
        XCTAssertTrue(
            PermissionsModel.presentsOnLaunch(arguments: plain + ["-onboarding"], environment: [:], defaults: defaults))
    }
}
