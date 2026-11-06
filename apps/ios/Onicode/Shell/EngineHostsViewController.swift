import UIKit

/// Settings → Computers: the computers this phone reaches its engines on, one
/// per line, by tailnet name (`studio`, `studio.tail1234.ts.net`) or address
/// (`100.101.102.103`). The Tailscale app keeps its own list to itself, so the
/// phone is told; the list stays on this phone and is looked up as soon as it
/// changes.
final class EngineHostsViewController: UIViewController, UITextViewDelegate {
    private let app: AppModel
    private let field = UITextView()
    private let note = UILabel()

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
        title = "Computers"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        navigationItem.rightBarButtonItem = UIBarButtonItem(
            systemItem: .done,
            primaryAction: UIAction { [weak self] _ in
                self?.save()
                self?.field.resignFirstResponder()
            })

        let card = UIView()
        card.backgroundColor = Palette.elevated
        card.layer.cornerRadius = 22
        card.layer.cornerCurve = .continuous
        configureField()

        note.text =
            "One computer per line: its tailnet name (studio, or studio.tail1234.ts.net) or its address "
            + "(100.101.102.103). Each runs Onicode, and this phone reaches it while the Tailscale app is "
            + "connected."
        note.font = Fonts.ui(.sans, 13)
        note.textColor = Palette.secondary
        note.numberOfLines = 0

        for v in [card, field, note] { v.translatesAutoresizingMaskIntoConstraints = false }
        view.addSubview(card)
        card.addSubview(field)
        view.addSubview(note)
        let guide = view.layoutMarginsGuide
        NSLayoutConstraint.activate([
            card.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 16),
            card.leadingAnchor.constraint(equalTo: guide.leadingAnchor),
            card.trailingAnchor.constraint(equalTo: guide.trailingAnchor),
            card.heightAnchor.constraint(equalToConstant: 180),
            field.topAnchor.constraint(equalTo: card.topAnchor),
            field.bottomAnchor.constraint(equalTo: card.bottomAnchor),
            field.leadingAnchor.constraint(equalTo: card.leadingAnchor),
            field.trailingAnchor.constraint(equalTo: card.trailingAnchor),
            note.topAnchor.constraint(equalTo: card.bottomAnchor, constant: 10),
            note.leadingAnchor.constraint(equalTo: guide.leadingAnchor, constant: 16),
            note.trailingAnchor.constraint(equalTo: guide.trailingAnchor, constant: -16),
        ])
    }

    /// The list's text view: the hosts as they are, typed as addresses, nothing corrected.
    private func configureField() {
        field.text = app.engineHosts.joined(separator: "\n")
        field.font = Fonts.ui(.mono, 16)
        field.textColor = Palette.text
        field.backgroundColor = .clear
        field.autocorrectionType = .no
        field.autocapitalizationType = .none
        field.spellCheckingType = .no
        field.smartDashesType = .no
        field.smartQuotesType = .no
        field.keyboardType = .URL
        field.textContainerInset = UIEdgeInsets(top: 14, left: 12, bottom: 14, right: 12)
        field.delegate = self
        field.accessibilityIdentifier = "engine-hosts"
        field.accessibilityLabel = "Computers, one per line"
    }

    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        if app.engineHosts.isEmpty { field.becomeFirstResponder() }
    }

    override func viewWillDisappear(_ animated: Bool) {
        super.viewWillDisappear(animated)
        save()
    }

    func textViewDidEndEditing(_: UITextView) {
        save()
    }

    /// One computer per line; blank lines and repeats are dropped.
    private func save() {
        app.engineHosts = field.text.split(whereSeparator: \.isNewline).map(String.init)
    }
}
