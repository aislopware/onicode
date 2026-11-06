import UIKit

/// Keyboard and touch: what the person does on the surface goes to the Mac.
extension DesktopSurfaceView {
    // MARK: Keyboard

    var keyboardShown: Bool {
        get { keys.isFirstResponder }
        set {
            if newValue { keys.becomeFirstResponder() } else { keys.resignFirstResponder() }
        }
    }

    fileprivate func typed(_ text: String) {
        for (i, line) in text.split(separator: "\n", omittingEmptySubsequences: false).enumerated() {
            if i > 0 { viewer?.key(key: .enter, mods: 0) }
            if !line.isEmpty { viewer?.text(text: String(line)) }
        }
    }

    /// Send the keys among `presses` the Mac takes; whether there were any.
    func sendKeys(_ presses: Set<UIPress>) -> Bool {
        var handled = false
        for press in presses {
            guard let key = press.key, let mapped = Self.key(key.keyCode) else { continue }
            viewer?.key(key: mapped, mods: Self.mods(key.modifierFlags))
            handled = true
        }
        return handled
    }

    private static func key(_ code: UIKeyboardHIDUsage) -> DesktopKey? {
        switch code {
        case .keyboardLeftArrow: .left
        case .keyboardRightArrow: .right
        case .keyboardUpArrow: .up
        case .keyboardDownArrow: .down
        case .keyboardEscape: .escape
        case .keyboardTab: .tab
        case .keyboardDeleteForward: .delete
        default: nil
        }
    }

    /// `Mods` bits, as `onicode_desktop_proto::input::Mods` numbers them.
    private static func mods(_ flags: UIKeyModifierFlags) -> UInt16 {
        var bits: UInt16 = 0
        if flags.contains(.shift) { bits |= 1 << 0 }
        if flags.contains(.alternate) { bits |= 1 << 1 }
        if flags.contains(.control) { bits |= 1 << 2 }
        if flags.contains(.command) { bits |= 1 << 3 }
        return bits
    }

    // MARK: Touch

    func installGestures() {
        let move = UIPanGestureRecognizer(target: self, action: #selector(moved(_:)))
        move.maximumNumberOfTouches = 1
        let scroll = UIPanGestureRecognizer(target: self, action: #selector(scrolled(_:)))
        scroll.minimumNumberOfTouches = 2
        scroll.allowedScrollTypesMask = .continuous
        let tap = UITapGestureRecognizer(target: self, action: #selector(tapped(_:)))
        let rightTap = UITapGestureRecognizer(target: self, action: #selector(rightTapped))
        rightTap.numberOfTouchesRequired = 2
        let press = UILongPressGestureRecognizer(target: self, action: #selector(pressed(_:)))
        press.minimumPressDuration = 0.35
        let pinch = UIPinchGestureRecognizer(target: self, action: #selector(pinched(_:)))
        let hover = UIHoverGestureRecognizer(target: self, action: #selector(hovered(_:)))
        for g in [move, scroll, tap, rightTap, press, pinch, hover] as [UIGestureRecognizer] {
            addGestureRecognizer(g)
        }
        tap.require(toFail: press)
    }

    @objc private func moved(_ g: UIPanGestureRecognizer) {
        let d = g.translation(in: self)
        g.setTranslation(.zero, in: self)
        viewer?.pointerMoved(dx: Float(d.x), dy: Float(d.y))
    }

    @objc private func tapped(_ g: UITapGestureRecognizer) {
        let now = CACurrentMediaTime()
        let point = g.location(in: self)
        var clicks: UInt8 = 1
        if let last = touch.lastTap, now - last.at < 0.35, hypot(point.x - last.point.x, point.y - last.point.y) < 30 {
            clicks = 2
        }
        touch.lastTap = clicks == 2 ? nil : (now, point)
        viewer?.click(button: .left, clicks: clicks)
    }

    @objc private func rightTapped() {
        viewer?.click(button: .right, clicks: 1)
    }

    /// Press, then drag: the button stays down while the finger moves.
    @objc private func pressed(_ g: UILongPressGestureRecognizer) {
        switch g.state {
        case .began:
            touch.dragging = true
            touch.lastDrag = g.location(in: self)
            viewer?.button(button: .left, down: true, clicks: 1, mods: 0)
        case .changed:
            let p = g.location(in: self)
            viewer?.pointerMoved(dx: Float(p.x - touch.lastDrag.x), dy: Float(p.y - touch.lastDrag.y))
            touch.lastDrag = p
        case .ended, .cancelled, .failed:
            if touch.dragging { viewer?.button(button: .left, down: false, clicks: 1, mods: 0) }
            touch.dragging = false
        default: break
        }
    }

    @objc private func scrolled(_ g: UIPanGestureRecognizer) {
        let d = g.translation(in: self)
        g.setTranslation(.zero, in: self)
        viewer?.scroll(dx: Float(d.x), dy: Float(d.y), phase: Self.phase(g.state), momentum: .none)
    }

    @objc private func pinched(_ g: UIPinchGestureRecognizer) {
        if g.state == .began { touch.lastPinch = 1 }
        let delta = g.scale - touch.lastPinch
        touch.lastPinch = g.scale
        viewer?.magnify(delta: Float(delta), phase: Self.phase(g.state))
    }

    /// A pointer device over the view (an iPad's trackpad): the pointer goes where it is.
    @objc private func hovered(_ g: UIHoverGestureRecognizer) {
        guard g.state == .changed || g.state == .began else { return }
        let p = g.location(in: self)
        viewer?.pointerAt(x: Float(p.x), y: Float(p.y))
    }

    private static func phase(_ state: UIGestureRecognizer.State) -> DesktopGesturePhase {
        switch state {
        case .began: .began
        case .changed: .changed
        case .ended: .ended
        case .cancelled, .failed: .cancelled
        default: .none
        }
    }
}

/// What the touch gestures remember between events.
struct TouchMemory {
    /// The last single tap, for a second one near it to make a double click.
    var lastTap: (at: CFTimeInterval, point: CGPoint)?
    /// A press-then-drag holds the button down.
    var dragging = false
    var lastDrag = CGPoint.zero
    var lastPinch: CGFloat = 1
}

/// The keyboard's way in: an invisible text input that hands every keystroke to the Mac.
final class KeyCatcher: UIView, UIKeyInput {
    weak var surface: DesktopSurfaceView?
    var isSecureTextEntry = false
    var autocorrectionType: UITextAutocorrectionType = .no
    var autocapitalizationType: UITextAutocapitalizationType = .none
    var smartQuotesType: UITextSmartQuotesType = .no
    var smartDashesType: UITextSmartDashesType = .no
    var spellCheckingType: UITextSpellCheckingType = .no
    var keyboardType: UIKeyboardType = .asciiCapable

    override init(frame _: CGRect) {
        super.init(frame: CGRect(x: 0, y: 0, width: 1, height: 1))
        alpha = 0.01
        isUserInteractionEnabled = false
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override var canBecomeFirstResponder: Bool { true }
    var hasText: Bool { true }

    func insertText(_ text: String) {
        surface?.typed(text)
    }

    func deleteBackward() {
        surface?.viewer?.key(key: .backspace, mods: 0)
    }
}
