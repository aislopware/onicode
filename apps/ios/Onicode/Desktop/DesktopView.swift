import SwiftUI
import UIKit

/// A Mac's desktop, full screen: the picture, the controls badge, and what to do when the Mac
/// cannot show it yet.
struct DesktopView: View {
    @ObservedObject var model: DesktopModel
    var close: () -> Void

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()
            DesktopSurface(model: model).ignoresSafeArea()
            if let overlay {
                overlay
            }
            VStack {
                topBar
                Spacer()
            }
            .padding(.horizontal, 12)
        }
        .statusBarHidden(model.state?.phase == .joined)
        .onAppear { model.start() }
        .onDisappear { model.stop() }
    }

    // MARK: Bar

    private var topBar: some View {
        HStack(spacing: 8) {
            Button(action: close) {
                Image(systemName: "xmark").font(.system(size: 15, weight: .semibold))
                    .frame(width: 40, height: 40)
                    .accessibilityLabel("Close")
            }
            .buttonStyle(.plain)
            .foregroundStyle(Color(uiColor: Palette.text))
            .glassEffect(.regular.interactive(), in: .circle)
            .accessibilityIdentifier("desktop-close")
            Spacer()
            if model.state?.phase == .joined {
                SeatBadge(model: model)
                Button {
                    model.keyboardShown.toggle()
                    model.surface.keyboardShown = model.keyboardShown
                } label: {
                    Image(systemName: model.keyboardShown ? "keyboard.chevron.compact.down" : "keyboard")
                        .font(.system(size: 15, weight: .medium))
                        .frame(width: 40, height: 40)
                        .accessibilityLabel(model.keyboardShown ? "Hide Keyboard" : "Show Keyboard")
                }
                .buttonStyle(.plain)
                .foregroundStyle(Color(uiColor: Palette.text))
                .glassEffect(.regular.interactive(), in: .circle)
                .accessibilityIdentifier("desktop-keyboard")
            }
        }
    }

    // MARK: Overlays

    private var overlay: AnyView? {
        if let error = model.error {
            return AnyView(
                Card(symbol: "exclamationmark.triangle", title: "Can't reach \(model.host)", detail: error) {
                    Button("Try Again") { model.start() }.buttonStyle(.glassProminent)
                })
        }
        guard let state = model.state else {
            return AnyView(Waiting(text: "Asking \(model.host) for its screen…"))
        }
        switch state.phase {
        case .connecting:
            return AnyView(Waiting(text: "Connecting to \(model.host)…"))
        case .reconnecting:
            let seconds = max(1, Int((Double(state.retryInMs) / 1_000).rounded()))
            return AnyView(Waiting(text: "Lost \(model.host). Trying again in \(seconds) s…"))
        case .notPermitted:
            return AnyView(NotPermitted(model: model, missing: state.missing))
        case .closed:
            return AnyView(
                Card(
                    symbol: "display.trianglebadge.exclamationmark", title: "\(model.host) closed its screen",
                    detail: state.closedReason
                ) {
                    Button("Connect Again") { model.start() }.buttonStyle(.glassProminent)
                })
        case .joined:
            if state.width == 0 {
                return AnyView(Waiting(text: "Opening \(model.host)'s display…"))
            }
            if !state.live {
                return AnyView(
                    Card(
                        symbol: "lock.display", title: "\(model.host) is locked or asleep",
                        detail: "The picture comes back when its display does."
                    ) { EmptyView() })
            }
            return nil
        }
    }
}

/// The view's picture surface: the UIKit view whose layer the decoder's pictures go to.
struct DesktopSurface: UIViewRepresentable {
    let model: DesktopModel

    func makeUIView(context _: Context) -> DesktopSurfaceView { model.surface }
    func updateUIView(_: DesktopSurfaceView, context _: Context) {}
}

/// Who holds the controls, and the way to take them.
private struct SeatBadge: View {
    @ObservedObject var model: DesktopModel

    var body: some View {
        let state = model.state
        let you = state?.youControl ?? false
        HStack(spacing: 8) {
            Circle()
                .fill(Color(uiColor: you ? Palette.success : Palette.warning))
                .frame(width: 7, height: 7)
            Text(label(state))
                .font(.desktop(.sansMedium, 14))
                .foregroundStyle(Color(uiColor: Palette.text))
                .lineLimit(1)
            if !you {
                Button("Take Control") { model.takeControl() }
                    .font(.desktop(.sansSemibold, 14))
                    .foregroundStyle(Color(uiColor: Palette.accent))
                    .accessibilityIdentifier("desktop-take-control")
            }
        }
        .padding(.horizontal, 14)
        .frame(height: 40)
        .glassEffect(.regular, in: .capsule)
        .accessibilityIdentifier("desktop-seat")
    }

    private func label(_ state: DesktopStateInfo?) -> String {
        guard let state else { return "" }
        if state.youControl {
            return state.clients > 1 ? "You have control · \(state.clients) viewing" : "You have control"
        }
        if let controller = state.controller { return "Viewing · \(controller) has control" }
        return state.inputRefused ? "Viewing · take control to use the Mac" : "Viewing"
    }
}

/// The Mac may not stream: which permission is off on which Mac, and a way to turn it on there.
private struct NotPermitted: View {
    @ObservedObject var model: DesktopModel
    let missing: [DesktopPane]

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Image(systemName: "lock.shield")
                .font(.system(size: 28, weight: .medium))
                .foregroundStyle(Color(uiColor: Palette.accent))
                .accessibilityHidden(true)
            Text("\(model.host) needs a permission")
                .font(.desktop(.sansSemibold, 19))
                .foregroundStyle(Color(uiColor: Palette.text))
            ForEach(missing, id: \.self) { pane in
                VStack(alignment: .leading, spacing: 8) {
                    Text("\(pane.title) is off on \(model.host). Onicode Desktop needs it \(pane.reason).")
                        .font(.desktop(.sans, 15))
                        .foregroundStyle(Color(uiColor: Palette.secondary))
                        .fixedSize(horizontal: false, vertical: true)
                    if model.guideOpened == pane {
                        Text(
                            "System Settings is open on \(model.host). Turn on Onicode Desktop there; this screen follows."
                        )
                        .font(.desktop(.sansMedium, 14))
                        .foregroundStyle(Color(uiColor: Palette.success))
                        .fixedSize(horizontal: false, vertical: true)
                    } else {
                        Button("Turn On") { model.openGuide(pane) }
                            .buttonStyle(.glassProminent)
                            .accessibilityIdentifier("desktop-turn-on-\(pane)")
                    }
                }
            }
        }
        .padding(22)
        .frame(maxWidth: 420, alignment: .leading)
        .glassEffect(.regular, in: .rect(cornerRadius: 26))
        .padding(24)
        .accessibilityIdentifier("desktop-not-permitted")
    }
}

private struct Waiting: View {
    let text: String

    var body: some View {
        VStack(spacing: 12) {
            ProgressView().tint(Color(uiColor: Palette.secondary))
            Text(text)
                .font(.desktop(.sansMedium, 15))
                .foregroundStyle(Color(uiColor: Palette.secondary))
                .multilineTextAlignment(.center)
        }
        .padding(22)
        .glassEffect(.regular, in: .rect(cornerRadius: 22))
    }
}

private struct Card<Action: View>: View {
    let symbol: String
    let title: String
    let detail: String
    @ViewBuilder var action: () -> Action

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: symbol)
                .font(.system(size: 26, weight: .medium))
                .foregroundStyle(Color(uiColor: Palette.secondary))
                .accessibilityHidden(true)
            Text(title)
                .font(.desktop(.sansSemibold, 17))
                .foregroundStyle(Color(uiColor: Palette.text))
                .multilineTextAlignment(.center)
            if !detail.isEmpty {
                Text(detail)
                    .font(.desktop(.sans, 14))
                    .foregroundStyle(Color(uiColor: Palette.secondary))
                    .multilineTextAlignment(.center)
            }
            action()
        }
        .padding(22)
        .frame(maxWidth: 380)
        .glassEffect(.regular, in: .rect(cornerRadius: 24))
        .padding(24)
    }
}

extension Font {
    /// The app's face at `size`, as SwiftUI takes it.
    static func desktop(_ face: FaceRole, _ size: CGFloat) -> Font {
        Font(Fonts.ui(face, size) as CTFont)
    }
}

/// Presents a Mac's desktop full screen from UIKit.
final class DesktopViewController: UIHostingController<DesktopView> {
    private let model: DesktopModel

    init(client: CoreClient, deviceId: String, deviceName: String) {
        let model = DesktopModel(client: client, deviceId: deviceId, deviceName: deviceName)
        self.model = model
        var dismiss: () -> Void = {}
        super.init(rootView: DesktopView(model: model) { dismiss() })
        dismiss = { [weak self] in self?.dismiss(animated: true) }
        modalPresentationStyle = .fullScreen
    }

    @available(*, unavailable)
    @MainActor dynamic required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override var prefersHomeIndicatorAutoHidden: Bool { true }
}
