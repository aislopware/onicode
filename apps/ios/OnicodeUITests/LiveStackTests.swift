import XCTest

/// Against a real engine: a headless engine running the mock harness on a
/// computer on this Mac's tailnet, reached by its tailnet name or address.
/// Skipped unless `TEST_RUNNER_ONICODE_LIVE_ENGINE=<host>` is set:
///
///   ONICODE_DATA_DIR=/tmp/e ONICODE_HARNESS=mock onicode headless &
///   TEST_RUNNER_ONICODE_LIVE_ENGINE=studio xcodebuild test \
///     -only-testing:OnicodeUITests/LiveStackTests …
final class LiveStackTests: XCTestCase {
    func testSendRoundTripsThroughRealEngine() throws {
        let engine = try XCTUnwrap(
            ProcessInfo.processInfo.environment["ONICODE_LIVE_ENGINE"], "set TEST_RUNNER_ONICODE_LIVE_ENGINE")
        let app = XCUIApplication()
        // The draft's harness wins over the engine's ONICODE_HARNESS default:
        // pin the mock so the test never starts a real agent.
        app.launchArguments = ["-engines", engine, "-harness", "mock"]
        app.launch()

        let accessory = app.buttons["new-session"]
        XCTAssertTrue(accessory.waitForExistence(timeout: 15))
        accessory.tap()
        let input = app.textViews["composer-input"]
        XCTAssertTrue(input.waitForExistence(timeout: 5))
        input.typeText("hello from the phone")
        app.buttons["composer-send"].tap()

        // The engine adopts the command (echo keeps its id) and the mock
        // harness replies through the chat's room on the peer link.
        let echo = app.staticTexts.matching(NSPredicate(format: "label BEGINSWITH 'You: hello from the phone'"))
            .firstMatch
        XCTAssertTrue(echo.waitForExistence(timeout: 10))
        let reply = app.staticTexts.matching(identifier: "row-markdown").firstMatch
        XCTAssertTrue(reply.waitForExistence(timeout: 60), "mock harness reply streamed back")
        let shot = XCTAttachment(screenshot: app.screenshot())
        shot.name = "live-roundtrip"
        shot.lifetime = .keepAlways
        add(shot)
        XCTAssertTrue(app.buttons["Send message"].waitForExistence(timeout: 60), "turn settles")
    }
}
