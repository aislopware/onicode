import UIKit

/// Settings → Providers, as on the desktop: pick a computer, then turn its
/// agents on or off and choose the account each runs on. A computer's agents
/// and logins live on that computer.
final class ProvidersViewController: UIViewController, UICollectionViewDelegate {
    private let app: AppModel
    private var collectionView: UICollectionView!
    private var dataSource: UICollectionViewDiffableDataSource<String, Row>!
    private var appToken: AnyObject?

    /// The computer shown.
    private var target: String
    private var harnesses: [String: [HarnessInfo]] = [:]
    private var accounts: [String: [DeviceAccountInfo]] = [:]
    private var failed: [String: String] = [:]
    private var toggling: Set<String> = []

    private static let lastTargetKey = "providersDevice"

    struct Row: Hashable {
        let id: String
        let title: String
        var subtitle: String?
        var harness: String?
        var symbol: String?
        var toggle: Bool?
        var toggleEnabled = true
        var dimmed = false
        var disclosure = false
        var busy = false
        var value: String?
    }

    init(app: AppModel, target: String? = nil) {
        self.app = app
        let computers = app.hostOptions
        let remembered = UserDefaults.standard.string(forKey: Self.lastTargetKey)
        self.target =
            target
            ?? remembered.flatMap { r in computers.contains { $0.id == r } ? r : nil }
            ?? computers.first(where: \.online)?.id
            ?? computers.first?.id
            ?? ""
        super.init(nibName: nil, bundle: nil)
        title = "Providers"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    private var targetName: String {
        app.hostOptions.first { $0.id == target }?.name ?? app.deviceName(target)
    }

    private var targetOnline: Bool {
        app.hostOptions.first { $0.id == target }?.online == true
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .never
        var config = UICollectionLayoutListConfiguration(appearance: .insetGrouped)
        config.backgroundColor = .clear
        config.headerMode = .supplementary
        config.footerMode = .supplementary
        collectionView = UICollectionView(
            frame: view.bounds, collectionViewLayout: UICollectionViewCompositionalLayout.list(using: config))
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        collectionView.backgroundColor = .clear
        collectionView.delegate = self
        collectionView.refreshControl = UIRefreshControl(
            frame: .zero,
            primaryAction: UIAction { [weak self] _ in
                self?.load()
            })
        view.addSubview(collectionView)

        let cell = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { [weak self] cell, _, row in
            guard let self else { return }
            var c = UIListContentConfiguration.subtitleCell()
            c.text = row.title
            c.textProperties.font = Fonts.ui(.sansMedium, 16)
            c.textProperties.color = row.dimmed ? Palette.tertiary : Palette.text
            c.secondaryText = row.subtitle
            c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
            c.secondaryTextProperties.color = Palette.secondary
            if let harness = row.harness {
                let mark = BrandMarks.image(for: harness, side: 20)
                c.image = row.dimmed ? mark.flatMap { $0.withAlpha(0.45) } : mark
            } else if let symbol = row.symbol {
                c.image = UIImage(systemName: symbol)
                c.imageProperties.tintColor = Palette.secondary
            }
            c.imageProperties.maximumSize = CGSize(width: 22, height: 22)
            c.imageProperties.reservedLayoutSize = CGSize(width: 28, height: 28)
            cell.contentConfiguration = c
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            var accessories: [UICellAccessory] = []
            if row.id == "device" {
                // UIKit aborts on a pop-up menu with nothing to select: no
                // computer found yet.
                let menu = self.deviceMenu()
                if !menu.children.isEmpty {
                    accessories.append(.popUpMenu(menu, displayed: .always, options: .init(tintColor: Palette.text)))
                }
                let name = UILabel()
                name.text = self.targetName
                name.font = Fonts.ui(.sansMedium, 15)
                name.textColor = Palette.secondary
                accessories.insert(
                    .customView(
                        configuration: .init(
                            customView: name, placement: .trailing(displayed: .always), reservedLayoutWidth: .actual)),
                    at: 0)
            } else if row.busy {
                let spinner = UIActivityIndicatorView(style: .medium)
                spinner.startAnimating()
                accessories.append(.customView(configuration: .init(customView: spinner, placement: .trailing())))
            } else if let on = row.toggle {
                let toggle = UISwitch()
                toggle.isOn = on
                toggle.isEnabled = row.toggleEnabled
                toggle.onTintColor = Palette.accent
                toggle.accessibilityIdentifier = "toggle-\(row.id)"
                toggle.addAction(
                    UIAction { [weak self, weak toggle] _ in
                        guard let harness = row.harness else { return }
                        self?.setEnabled(harness, toggle?.isOn ?? false)
                    }, for: .valueChanged)
                accessories.append(.customView(configuration: .init(customView: toggle, placement: .trailing())))
            }
            if row.disclosure { accessories.append(.disclosureIndicator()) }
            cell.accessories = accessories
            cell.accessibilityIdentifier = "providers-\(row.id)"
        }
        let header = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(
            elementKind: UICollectionView.elementKindSectionHeader
        ) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedHeader()
            let id = self?.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "device" ? nil : id
            view.contentConfiguration = c
        }
        let footer = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(
            elementKind: UICollectionView.elementKindSectionFooter
        ) { [weak self] view, _, path in
            guard let self else { return }
            var c = UIListContentConfiguration.groupedFooter()
            let id = self.dataSource.snapshot().sectionIdentifiers[path.section]
            c.text = id == "Agents" ? self.footerText : nil
            view.contentConfiguration = c
        }
        dataSource = UICollectionViewDiffableDataSource(collectionView: collectionView) { cv, path, row in
            cv.dequeueConfiguredReusableCell(using: cell, for: path, item: row)
        }
        dataSource.supplementaryViewProvider = { cv, kind, path in
            kind == UICollectionView.elementKindSectionHeader
                ? cv.dequeueConfiguredReusableSupplementary(using: header, for: path)
                : cv.dequeueConfiguredReusableSupplementary(using: footer, for: path)
        }
        appToken = app.observe { [weak self] in self?.reload() }
        reload()
        load()
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        // Back from an agent's accounts: they may have switched or removed one.
        if dataSource != nil { loadAccounts() }
    }

    private var footerText: String {
        if target.isEmpty {
            return "Add your computers in Settings → Computers to run agents and manage their accounts here."
        }
        if !targetOnline {
            return "\(targetName) is offline — showing what it last reported."
        }
        return
            "Agents run with the logins on \(targetName). Switch accounts here; to add one, sign in from Onicode on that computer."
    }

    // MARK: Loading

    private func load() {
        let device = target
        guard !device.isEmpty else {
            collectionView.refreshControl?.endRefreshing()
            return
        }
        Task { @MainActor in
            guard let client = self.app.client else { return }
            let list = await client.listHarnesses(deviceId: device)
            self.harnesses[device] = list
            self.reload()
            self.loadAccounts()
            self.collectionView.refreshControl?.endRefreshing()
        }
    }

    private func loadAccounts() {
        let device = target
        Task { @MainActor in
            guard let client = self.app.client, self.targetOnline else { return }
            do {
                self.accounts[device] = try await client.deviceAgentAccounts(deviceId: device)
                self.failed[device] = nil
            } catch {
                self.failed[device] = SettingsUI.message(error)
            }
            self.reload()
        }
    }

    private func deviceMenu() -> UIMenu {
        let actions: [UIMenuElement] = app.hostOptions.map { host in
            let action = UIAction(
                title: host.name, subtitle: host.online ? nil : "Offline", image: UIImage(systemName: "laptopcomputer")
            ) { [weak self] _ in
                self?.select(host.id)
            }
            action.state = host.id == target ? .on : .off
            return action
        }
        return UIMenu(title: "Computers", children: actions)
    }

    private func select(_ device: String) {
        guard device != target else { return }
        target = device
        UserDefaults.standard.set(device, forKey: Self.lastTargetKey)
        reload()
        load()
    }

    // MARK: Rows

    private func reload() {
        guard dataSource != nil else { return }
        // No computer found when this opened: the first one found since.
        if target.isEmpty, let computer = app.hostOptions.first {
            target = computer.id
            load()
        }
        var s = NSDiffableDataSourceSnapshot<String, Row>()
        s.appendSections(["device"])
        // The device in the row's value: switching re-renders it.
        s.appendItems([
            Row(id: "device", title: "Computer", subtitle: nil, symbol: "laptopcomputer", value: target)
        ])
        s.appendSections(["Agents"])
        s.appendItems(computerRows())
        dataSource.apply(s, animatingDifferences: false)
    }

    private func computerRows() -> [Row] {
        if target.isEmpty {
            return [
                Row(
                    id: "no-computers", title: "No computers yet",
                    subtitle: "Run Onicode on a computer on your tailnet, then add it in Settings → Computers.",
                    symbol: "desktopcomputer")
            ]
        }
        guard let list = harnesses[target] else {
            return [Row(id: "loading", title: "Loading…", symbol: "ellipsis")]
        }
        let online = targetOnline
        let enabledCount = list.filter(\.offered).count
        let logins = accounts[target] ?? []
        return list.sorted { ($0.installed ? 0 : 1, $0.label) < ($1.installed ? 0 : 1, $1.label) }.map { h in
            let mine = logins.filter { $0.harness == h.id }
            let on = h.offered
            let subtitle: String?
            if !h.installed {
                subtitle = "Not installed"
            } else if accounts[target] == nil {
                subtitle = failed[target] ?? (online ? "Installed" : "Offline")
            } else {
                subtitle = Self.summary(
                    active: mine.first(where: \.active).map(Self.accountTitle), count: mine.count,
                    showsNone: agentLoginRules(harness: h.id).signsIn)
            }
            return Row(
                id: "\(target):\(h.id)",
                title: h.label,
                subtitle: subtitle,
                harness: h.id,
                toggle: h.installed ? on : false,
                // The last agent standing stays on: new sessions need one.
                toggleEnabled: online && h.installed && !(on && enabledCount <= 1),
                dimmed: !h.installed,
                // Every agent with logins opens its accounts — to add the
                // first one too.
                disclosure: h.installed && (agentLoginRules(harness: h.id).signsIn || !mine.isEmpty),
                busy: toggling.contains(h.id)
            )
        }
    }

    static func accountTitle(_ a: DeviceAccountInfo) -> String {
        a.email ?? a.displayName ?? (a.apiKey ? "API key" : "Account")
    }

    private static func summary(active: String?, count: Int, showsNone: Bool = true) -> String? {
        switch (active, count) {
        case let (active?, n) where n > 1: return "\(active) · \(n) accounts"
        case let (active?, _): return active
        case (nil, 0): return showsNone ? "No account" : nil
        case (nil, 1): return "1 account"
        case let (nil, n): return "\(n) accounts"
        }
    }

    // MARK: Actions

    private func setEnabled(_ harness: String, _ on: Bool) {
        let device = target
        toggling.insert(harness)
        reload()
        Task { @MainActor in
            defer {
                self.toggling.remove(harness)
                self.reload()
            }
            guard let client = self.app.client else { return }
            do {
                self.harnesses[device] = try await client.setHarnessEnabled(
                    deviceId: device, harness: harness, enabled: on)
                UISelectionFeedbackGenerator().selectionChanged()
            } catch {
                let a = UIAlertController(
                    title: on ? "Couldn't turn it on" : "Couldn't turn it off", message: SettingsUI.message(error),
                    preferredStyle: .alert)
                a.addAction(UIAlertAction(title: "OK", style: .default))
                self.present(a, animated: true)
            }
        }
    }

    /// Pushed in Settings; over the page as a sheet where there's no stack.
    private func show(_ vc: UIViewController) {
        if let nav = navigationController {
            nav.pushViewController(vc, animated: true)
            return
        }
        vc.navigationItem.rightBarButtonItem = UIBarButtonItem(
            systemItem: .done,
            primaryAction: UIAction { [weak vc] _ in
                vc?.dismiss(animated: true)
            })
        present(UINavigationController(rootViewController: vc), animated: true)
    }

    func collectionView(_ cv: UICollectionView, didSelectItemAt path: IndexPath) {
        cv.deselectItem(at: path, animated: true)
        guard let row = dataSource.itemIdentifier(for: path), row.disclosure, let harness = row.harness else { return }
        let device = target
        let vc = AgentAccountsViewController(
            app: app,
            target: .init(id: device, name: targetName, harness: harness, label: row.title),
            accounts: accounts[device]
        )
        vc.onChange = { [weak self] list in
            guard let self else { return }
            self.accounts[device] = list
            self.reload()
        }
        show(vc)
    }
}
