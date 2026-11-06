import Foundation
import UIKit

/// One Mac's desktop on this phone: the Rust `DesktopViewer`, what the view shows of it, and the
/// surface its pictures go to.
@MainActor
final class DesktopModel: ObservableObject {
    @Published private(set) var state: DesktopStateInfo?
    @Published private(set) var error: String?
    /// The pane whose guide was opened on the Mac, to say so until the grant comes on.
    @Published private(set) var guideOpened: DesktopPane?
    @Published var keyboardShown = false

    let deviceName: String
    let viewer: DesktopViewer
    let surface: DesktopSurfaceView
    private let bridge: SinkBridge

    init(client: CoreClient, deviceId: String, deviceName: String) {
        self.deviceName = deviceName
        let surface = DesktopSurfaceView()
        let bridge = SinkBridge(surface: surface)
        self.surface = surface
        self.bridge = bridge
        viewer = DesktopViewer(client: client, deviceId: deviceId, name: UIDevice.current.name, sink: bridge)
        surface.viewer = viewer
        bridge.model = self
    }

    /// The Mac's name as the session knows it, else as the device list does.
    var host: String {
        guard let host = state?.host, !host.isEmpty else { return deviceName }
        return host
    }

    func start() {
        error = nil
        Task {
            do {
                try await viewer.start()
            } catch {
                self.error = "\(error)"
            }
        }
    }

    func stop() {
        viewer.stop()
    }

    func takeControl() {
        viewer.takeControl(take: true)
    }

    /// Open the guide to `pane` on the Mac's own screen: System Settings at the pane, for the
    /// person at the Mac. Nothing is asked of anyone here.
    func openGuide(_ pane: DesktopPane) {
        Task {
            do {
                try await viewer.openGuide(pane: pane)
                guideOpened = pane
            } catch {
                self.error = "\(error)"
            }
        }
    }

    fileprivate func apply(_ state: DesktopStateInfo) {
        if state.phase != .notPermitted { guideOpened = nil }
        self.state = state
        surface.apply(state)
    }
}

extension DesktopPane {
    /// What System Settings calls the pane.
    var title: String {
        switch self {
        case .screenRecording: "Screen & System Audio Recording"
        case .accessibility: "Accessibility"
        }
    }

    /// What the Mac needs it for.
    var reason: String {
        switch self {
        case .screenRecording: "to show its screen"
        case .accessibility: "to take your clicks and keys"
        }
    }
}

/// The Rust viewer's sink. Pictures go straight to the surface on the decoder's thread; state and
/// pointer hop to the main thread.
private final class SinkBridge: DesktopSink, @unchecked Sendable {
    weak var model: DesktopModel?
    let surface: DesktopSurfaceView

    init(surface: DesktopSurfaceView) {
        self.surface = surface
    }

    func onPicture(pixelBuffer: UInt64, width _: UInt32, height _: UInt32, token: UInt64) {
        guard let buffer = Self.take(pixelBuffer) else { return }
        surface.show(buffer, token: token)
    }

    func onStripes(top: UInt64, lower: UInt64, topRows: UInt32, lowerFrom: UInt32, token: UInt64) {
        let top = Self.take(top)
        let lower = Self.take(lower)
        guard let top, let lower else { return }
        surface.show(top: top, lower: lower, topRows: Int(topRows), lowerFrom: Int(lowerFrom), token: token)
    }

    func onState(state: DesktopStateInfo) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.model?.apply(state) }
        }
    }

    func onCursor(cursor: DesktopCursor) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.surface.place(cursor) }
        }
    }

    /// The `CVPixelBuffer` Rust retained once for us: taken, so it is released with the last use.
    private static func take(_ address: UInt64) -> CVPixelBuffer? {
        guard let raw = UnsafeRawPointer(bitPattern: UInt(address)) else { return nil }
        return Unmanaged<CVPixelBuffer>.fromOpaque(raw).takeRetainedValue()
    }
}
