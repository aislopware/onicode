import XCTest

/// Settings → Computers: the list this phone joins the tailnet through. Live
/// mode, but no computer is listed and none is reached, so it needs no
/// network.
final class ComputersTests: XCTestCase {
    override func setUp() {
        continueAfterFailure = false
    }

    func testListedComputerShowsInSettings() {
        let app = XCUIApplication()
        app.launchArguments = ["-engines", "", "-route", "more"]
        app.launch()

        let row = app.cells["settings-computers"]
        XCTAssertTrue(row.waitForExistence(timeout: 10))
        row.tap()

        let field = app.textViews["engine-hosts"]
        XCTAssertTrue(field.waitForExistence(timeout: 5))
        field.tap()
        field.typeText("studio")
        app.navigationBars["Computers"].buttons["Done"].tap()
        app.navigationBars["Computers"].buttons.element(boundBy: 0).tap()

        let listed = app.staticTexts.containing(NSPredicate(format: "label CONTAINS 'studio'")).firstMatch
        XCTAssertTrue(listed.waitForExistence(timeout: 5), "the computers row names the listed computer")
    }
}
