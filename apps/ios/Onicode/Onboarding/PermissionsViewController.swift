import UIKit

/// The permissions page: Notifications and the Microphone, each with its
/// reason, where it stands, and one button. On first launch it's presented
/// with Continue; Settings → Permissions pushes it without.
final class PermissionsViewController: UIViewController {
    private let model: PermissionsModel
    private let onContinue: (() -> Void)?
    private var rows: [Permission: PermissionRow] = [:]
    private var activeObserver: NSObjectProtocol?

    /// `model` nil: a new one over the real system. `onContinue` nil: reached from Settings, so
    /// no Continue button.
    init(model: PermissionsModel? = nil, onContinue: (() -> Void)? = nil) {
        self.model = model ?? PermissionsModel()
        self.onContinue = onContinue
        super.init(nibName: nil, bundle: nil)
        title = "Permissions"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never

        let note = UILabel()
        note.text = "Each is asked only when you tap it. You can change them later in Settings."
        note.font = Fonts.ui(.sans, 13)
        note.textColor = Palette.secondary
        note.numberOfLines = 0

        let stack = UIStackView(arrangedSubviews: [card(), note])
        stack.axis = .vertical
        stack.spacing = 10
        if onContinue != nil {
            let top = header()
            stack.insertArrangedSubview(top, at: 0)
            stack.setCustomSpacing(24, after: top)
        }

        layout(stack)

        model.onChange = { [weak self] in self?.render() }
        render()
        // Back from Settings or a system alert: read both again.
        activeObserver = NotificationCenter.default.addObserver(
            forName: UIApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.model.refresh() }
        }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        Task { await model.refresh() }
    }

    /// The two rows on one elevated card, a hairline between.
    private func card() -> UIView {
        let card = UIStackView()
        card.axis = .vertical
        card.backgroundColor = Palette.elevated
        card.layer.cornerRadius = 22
        card.layer.cornerCurve = .continuous
        for (i, permission) in Permission.allCases.enumerated() {
            if i > 0 { card.addArrangedSubview(hairline()) }
            let row = PermissionRow(permission) { [weak self] in
                Task { @MainActor in await self?.model.tap(permission) }
            }
            rows[permission] = row
            card.addArrangedSubview(row)
        }
        return card
    }

    /// The page scrolls; Continue, when there is one, stays at the bottom.
    private func layout(_ stack: UIStackView) {
        let scroll = UIScrollView()
        scroll.alwaysBounceVertical = true
        for v in [scroll, stack] { v.translatesAutoresizingMaskIntoConstraints = false }
        view.addSubview(scroll)
        scroll.addSubview(stack)
        let guide = view.layoutMarginsGuide
        NSLayoutConstraint.activate([
            scroll.topAnchor.constraint(equalTo: view.topAnchor),
            scroll.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            scroll.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            stack.topAnchor.constraint(equalTo: scroll.contentLayoutGuide.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: scroll.contentLayoutGuide.bottomAnchor, constant: -16),
            stack.leadingAnchor.constraint(equalTo: guide.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: guide.trailingAnchor),
        ])
        if onContinue != nil {
            let button = continueButton()
            view.addSubview(button)
            NSLayoutConstraint.activate([
                scroll.bottomAnchor.constraint(equalTo: button.topAnchor, constant: -12),
                button.leadingAnchor.constraint(equalTo: guide.leadingAnchor),
                button.trailingAnchor.constraint(equalTo: guide.trailingAnchor),
                button.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor, constant: -12),
                button.heightAnchor.constraint(equalToConstant: 50),
            ])
        } else {
            scroll.bottomAnchor.constraint(equalTo: view.bottomAnchor).isActive = true
        }
    }

    private func render() {
        for (permission, row) in rows { row.show(model.statuses[permission]) }
    }

    private func header() -> UIView {
        let title = UILabel()
        title.text = "Before you start"
        title.font = Fonts.ui(.sansSemibold, 28)
        title.textColor = Palette.text
        title.accessibilityTraits = .header
        let detail = UILabel()
        detail.text = "Two things make Onicode better. Nothing else is asked for until you need it."
        detail.font = Fonts.ui(.sans, 15)
        detail.textColor = Palette.secondary
        detail.numberOfLines = 0
        let header = UIStackView(arrangedSubviews: [title, detail])
        header.axis = .vertical
        header.spacing = 6
        header.isLayoutMarginsRelativeArrangement = true
        header.directionalLayoutMargins = NSDirectionalEdgeInsets(top: 24, leading: 4, bottom: 0, trailing: 4)
        return header
    }

    /// The question panel's primary: a text-colored prominent glass capsule.
    private func continueButton() -> UIButton {
        var c = UIButton.Configuration.prominentGlass()
        c.title = "Continue"
        c.baseBackgroundColor = Palette.text
        c.baseForegroundColor = Palette.background
        c.cornerStyle = .capsule
        c.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { a in
            var a = a
            a.font = Fonts.ui(.sansSemibold, 16)
            return a
        }
        let button = UIButton(
            configuration: c,
            primaryAction: UIAction { [weak self] _ in
                self?.model.markShown()
                self?.onContinue?()
            })
        button.accessibilityIdentifier = "permissions-continue"
        button.translatesAutoresizingMaskIntoConstraints = false
        return button
    }

    private func hairline() -> UIView {
        let line = UIView()
        line.backgroundColor = Palette.hairline
        line.heightAnchor.constraint(equalToConstant: 1 / max(1, traitCollection.displayScale)).isActive = true
        return line
    }
}

/// One permission: its mark, name, reason and status, and its button.
private final class PermissionRow: UIView {
    private let status = UILabel()
    private let button: UIButton
    private let check = UIImageView(image: SettingsUI.symbol("checkmark.circle.fill", 22))

    init(_ permission: Permission, onTap: @escaping () -> Void) {
        button = UIButton(primaryAction: UIAction { _ in onTap() })
        super.init(frame: .zero)

        let (name, reason, symbol) =
            switch permission {
            case .notifications: ("Notifications", "Hear when an agent finishes or needs you.", "bell.badge")
            case .microphone: ("Microphone", "Talk to your agents by voice.", "mic")
            }
        check.tintColor = Palette.success
        check.accessibilityLabel = "\(name) allowed"
        check.isHidden = true
        button.accessibilityIdentifier = "permission-\(name.lowercased())"
        button.isHidden = true
        for v in [button, check] as [UIView] {
            v.setContentHuggingPriority(.required, for: .horizontal)
            v.setContentCompressionResistancePriority(.required, for: .horizontal)
        }

        let mark = Self.mark(symbol)
        let row = UIStackView(arrangedSubviews: [mark, text(name, reason), button, check])
        row.axis = .horizontal
        row.alignment = .center
        row.spacing = 12
        row.isLayoutMarginsRelativeArrangement = true
        row.directionalLayoutMargins = NSDirectionalEdgeInsets(top: 14, leading: 16, bottom: 14, trailing: 16)
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            mark.widthAnchor.constraint(equalToConstant: 36),
            mark.heightAnchor.constraint(equalToConstant: 36),
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor),
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(equalTo: trailingAnchor),
        ])
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    /// The permission's symbol on a soft accent tile.
    private static func mark(_ symbol: String) -> UIView {
        let mark = UIImageView(image: SettingsUI.symbol(symbol, 17))
        mark.tintColor = Palette.accent
        mark.contentMode = .center
        mark.backgroundColor = Palette.accentSoft
        mark.layer.cornerRadius = 10
        mark.layer.cornerCurve = .continuous
        mark.isAccessibilityElement = false
        return mark
    }

    /// Name, reason, and the status line under them.
    private func text(_ name: String, _ reason: String) -> UIView {
        let title = UILabel()
        title.text = name
        title.font = Fonts.ui(.sansMedium, 16)
        title.textColor = Palette.text
        let detail = UILabel()
        detail.text = reason
        detail.font = Fonts.ui(.sans, 13)
        detail.textColor = Palette.secondary
        detail.numberOfLines = 0
        status.font = Fonts.ui(.sans, 13)
        status.numberOfLines = 0
        let text = UIStackView(arrangedSubviews: [title, detail, status])
        text.axis = .vertical
        text.spacing = 2
        return text
    }

    /// `nil`: not read yet, so no status and no button.
    func show(_ state: PermissionStatus?) {
        switch state {
        case nil:
            status.text = nil
        case .notDetermined:
            status.text = "Not asked yet"
            status.textColor = Palette.tertiary
        case .denied:
            status.text = "Off — turn it on in Settings"
            status.textColor = Palette.danger
        case .granted:
            status.text = "Allowed"
            status.textColor = Palette.success
        }
        status.isHidden = state == nil
        check.isHidden = state != .granted
        let action = state.map(PermissionAction.init)
        button.isHidden = action == nil || action == .done
        if action == .ask { button.configuration = Self.style("Allow…") }
        if action == .openSettings { button.configuration = Self.style("Open Settings") }
    }

    /// Account rows' Switch button: a capsule on the control fill.
    private static func style(_ title: String) -> UIButton.Configuration {
        var c = UIButton.Configuration.filled()
        c.title = title
        c.baseBackgroundColor = Palette.controlFill
        c.baseForegroundColor = Palette.text
        c.cornerStyle = .capsule
        c.contentInsets = NSDirectionalEdgeInsets(top: 6, leading: 14, bottom: 6, trailing: 14)
        c.titleTextAttributesTransformer = UIConfigurationTextAttributesTransformer { a in
            var a = a
            a.font = Fonts.ui(.sansMedium, 14)
            return a
        }
        return c
    }
}
