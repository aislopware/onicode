import UIKit

/// Settings & utilities: the computers this phone reaches, providers,
/// devices, permissions, appearance, archive.
final class MoreViewController: UIViewController, UICollectionViewDelegate {
    private let app: AppModel
    private var collectionView: UICollectionView!
    private var dataSource: UICollectionViewDiffableDataSource<String, Row>!

    struct Row: Hashable {
        let id: String
        let title: String
        var subtitle: String?
        var symbol: String
        var tint: UIColor = Palette.secondary
        var accessory: Accessory = .disclosure
        var destructive = false

        // swiftlint:disable:next nesting - a Row field's type, used nowhere else
        enum Accessory: Hashable {
            case disclosure
            case plain
            case check(Bool)
            case dot(Bool)
            case toggle(Bool)
        }
    }

    init(app: AppModel) {
        self.app = app
        super.init(nibName: nil, bundle: nil)
        title = "Settings"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override func viewDidLoad() {
        super.viewDidLoad()
        view.backgroundColor = Palette.background
        navigationItem.largeTitleDisplayMode = .always
        var config = UICollectionLayoutListConfiguration(appearance: .insetGrouped)
        config.backgroundColor = .clear
        config.headerMode = .supplementary
        collectionView = UICollectionView(
            frame: view.bounds, collectionViewLayout: UICollectionViewCompositionalLayout.list(using: config))
        collectionView.autoresizingMask = [.flexibleWidth, .flexibleHeight]
        collectionView.backgroundColor = .clear
        collectionView.delegate = self
        view.addSubview(collectionView)

        let cell = UICollectionView.CellRegistration<UICollectionViewListCell, Row> { [weak self] cell, _, row in
            var c = UIListContentConfiguration.subtitleCell()
            c.text = row.title
            c.textProperties.font = Fonts.ui(.sansMedium, 16)
            c.textProperties.color = row.destructive ? Palette.danger : Palette.text
            c.secondaryText = row.subtitle
            c.secondaryTextProperties.font = Fonts.ui(.sans, 13)
            c.secondaryTextProperties.color = Palette.secondary
            c.image = UIImage(systemName: row.symbol)
            c.imageProperties.tintColor = row.destructive ? Palette.danger : row.tint
            cell.contentConfiguration = c
            var bg = UIBackgroundConfiguration.listGroupedCell()
            bg.backgroundColor = Palette.elevated
            cell.backgroundConfiguration = bg
            switch row.accessory {
            case .disclosure: cell.accessories = [.disclosureIndicator()]
            case .plain: cell.accessories = []
            case let .check(on): cell.accessories = on ? [.checkmark(options: .init(tintColor: Palette.accent))] : []
            case let .dot(online):
                let dot = UIView(frame: CGRect(x: 0, y: 0, width: 8, height: 8))
                dot.backgroundColor = online ? Palette.success : Palette.tertiary
                dot.layer.cornerRadius = 4
                cell.accessories = [.customView(configuration: .init(customView: dot, placement: .trailing()))]
            case let .toggle(on):
                let toggle = UISwitch()
                toggle.isOn = on
                toggle.onTintColor = Palette.accent
                toggle.accessibilityIdentifier = "toggle-\(row.id)"
                toggle.addAction(
                    UIAction { [weak self, weak toggle] _ in
                        self?.toggled(row.id, toggle?.isOn ?? false)
                    }, for: .valueChanged)
                cell.accessories = [.customView(configuration: .init(customView: toggle, placement: .trailing()))]
            }
            cell.accessibilityIdentifier = "settings-\(row.id)"
        }
        let header = UICollectionView.SupplementaryRegistration<UICollectionViewListCell>(
            elementKind: UICollectionView.elementKindSectionHeader
        ) { [weak self] view, _, path in
            var c = UIListContentConfiguration.groupedHeader()
            c.text = self?.dataSource.snapshot().sectionIdentifiers[path.section]
            view.contentConfiguration = c
        }
        dataSource = UICollectionViewDiffableDataSource(collectionView: collectionView) { cv, path, row in
            cv.dequeueConfiguredReusableCell(using: cell, for: path, item: row)
        }
        dataSource.supplementaryViewProvider = { cv, _, path in
            cv.dequeueConfiguredReusableSupplementary(using: header, for: path)
        }
        reload()
        wallpaperObserver = NotificationCenter.default.addObserver(
            forName: WallpaperStore.didChange, object: nil, queue: .main
        ) { [weak self] _ in self?.reload() }
        // Devices go on/offline, the tailnet comes up.
        appToken = app.observe { [weak self] in self?.reload() }
    }

    override func viewWillAppear(_ animated: Bool) {
        super.viewWillAppear(animated)
        if dataSource != nil { reload() }
    }

    private func toggled(_ id: String, _ on: Bool) {
        if id == "groupByProject" {
            app.groupByProject = on
            return
        }
        if id == "math:render" {
            MathSettings.render = on
            reload()
        }
    }

    private var wallpaperObserver: NSObjectProtocol?
    private var appToken: AnyObject?

    private func reload() {
        var s = NSDiffableDataSourceSnapshot<String, Row>()
        s.appendSections(["Tailnet"])
        s.appendItems([computersRow])
        s.appendSections(["Agents"])
        s.appendItems([
            Row(
                id: "providers", title: "Providers", subtitle: "Agents and their accounts on each computer",
                symbol: "cpu")
        ])
        s.appendSections(["Devices"])
        s.appendItems(deviceRows)
        if app.voice.available {
            s.appendSections(["Voice"])
            s.appendItems([
                Row(
                    id: "voice", title: "Voice",
                    subtitle: app.voice.selectedStyle.map { "Codex voice · \($0.capitalized)" }
                        ?? "Codex voice device and style", symbol: "waveform")
            ])
        }
        s.appendSections(["Privacy"])
        s.appendItems([
            Row(
                id: "permissions", title: "Permissions", subtitle: "Notifications and the microphone",
                symbol: "hand.raised")
        ])
        s.appendSections(["Appearance"])
        let style = UserDefaults.standard.integer(forKey: "appearance")
        s.appendItems([
            Row(id: "appearance:0", title: "System", symbol: "circle.lefthalf.filled", accessory: .check(style == 0)),
            Row(id: "appearance:1", title: "Light", symbol: "sun.max", accessory: .check(style == 1)),
            Row(id: "appearance:2", title: "Dark", symbol: "moon", accessory: .check(style == 2)),
        ])
        s.appendSections(["Wallpaper"])
        s.appendItems(wallpaperRows)
        s.appendSections(["Math"])
        s.appendItems(mathRows)
        s.appendSections(["Sessions"])
        s.appendItems([
            Row(
                id: "groupByProject", title: "Group by Project",
                subtitle: "Sessions outside your sections, under their project", symbol: "square.stack.3d.up",
                accessory: .toggle(app.groupByProject)),
            Row(id: "archived", title: "Archived Sessions", symbol: "archivebox"),
        ])
        dataSource.apply(s, animatingDifferences: false)
    }

    func collectionView(_ collectionView: UICollectionView, didSelectItemAt path: IndexPath) {
        collectionView.deselectItem(at: path, animated: true)
        guard let row = dataSource.itemIdentifier(for: path) else { return }
        if let page = page(for: row.id) {
            navigationController?.pushViewController(page, animated: true)
            return
        }
        switch row.id {
        case let id where id.hasPrefix("appearance:"):
            let style = Int(id.dropFirst(11)) ?? 0
            UserDefaults.standard.set(style, forKey: "appearance")
            view.window?.overrideUserInterfaceStyle = UIUserInterfaceStyle(rawValue: style) ?? .unspecified
            reload()
        case "wallpaper:choose":
            WallpaperPicker.present(from: self)
        case "wallpaper:effect", "math:font", "math:size":
            choose(row.id, from: collectionView.cellForItem(at: path))
        case let id where id.hasPrefix("device:") && !app.isDemo:
            // A Mac's desktop, full screen (Desktop/DesktopView.swift).
            guard let client = app.client else { return }
            let desktop = DesktopViewController(
                client: client, deviceId: String(id.dropFirst("device:".count)), deviceName: row.title)
            present(desktop, animated: true)
        case "wallpaper:remove":
            WallpaperStore.remove()
        default:
            break
        }
    }
}

/// The rows `reload` builds that take more than a line, and the sheets rows open.
extension MoreViewController {
    /// The computers this phone reaches, and whether it's on the tailnet.
    private var computersRow: Row {
        if app.isDemo {
            return Row(
                id: "computers", title: "Computers", subtitle: "The demo's computers are simulated", symbol: "network",
                accessory: .plain)
        }
        let hosts = app.engineHosts
        let subtitle =
            hosts.isEmpty
            ? "Add a computer by its tailnet name"
            : app.onTailnet ? hosts.joined(separator: ", ") : "Waiting for Tailscale · \(hosts.joined(separator: ", "))"
        return Row(id: "computers", title: "Computers", subtitle: subtitle, symbol: "network")
    }

    /// Choose or change the wallpaper, and once one is set, its effect and removal.
    private var wallpaperRows: [Row] {
        var wall = [
            Row(
                id: "wallpaper:choose", title: WallpaperStore.isSet ? "Change Wallpaper…" : "Choose Wallpaper…",
                subtitle: WallpaperStore.isSet ? WallpaperStore.name : "Shown behind new chats and the sessions list",
                symbol: "photo")
        ]
        if WallpaperStore.isSet {
            let effect = WallpaperStore.effect
            wall.append(
                Row(
                    id: "wallpaper:effect", title: "Effect",
                    subtitle: "\(WallpaperStore.label(effect)) — \(WallpaperStore.detail(effect))",
                    symbol: "wand.and.stars"))
            wall.append(
                Row(
                    id: "wallpaper:remove", title: "Remove Wallpaper", symbol: "trash", accessory: .plain,
                    destructive: true))
        }
        return wall
    }

    /// Render Math, and while it's on, the face and size.
    private var mathRows: [Row] {
        var math = [
            Row(
                id: "math:render", title: "Render Math", subtitle: "Off shows LaTeX as its source", symbol: "function",
                accessory: .toggle(MathSettings.render))
        ]
        if MathSettings.render {
            let face = MathSettings.face
            math.append(
                Row(
                    id: "math:font", title: "Math Font",
                    subtitle: "\(MathSettings.label(face)) — \(MathSettings.detail(face))", symbol: "textformat"))
            math.append(
                Row(
                    id: "math:size", title: "Math Size", subtitle: "\(MathSettings.size)% of the text around it",
                    symbol: "textformat.size"))
        }
        return math
    }

    /// The computers on the tailnet; a Mac's opens its screen.
    private var deviceRows: [Row] {
        app.hostOptions.map {
            Row(
                id: "device:\($0.id)", title: $0.name,
                subtitle: $0.online ? "Online · Tap to show its screen" : "Offline",
                symbol: "desktopcomputer", accessory: .dot($0.online))
        }
    }

    /// The page a row pushes, if it pushes one.
    private func page(for id: String) -> UIViewController? {
        switch id {
        case "voice": VoiceViewController(app: app)
        case "permissions": PermissionsViewController()
        case "providers": ProvidersViewController(app: app)
        case "computers" where !app.isDemo: EngineHostsViewController(app: app)
        case "archived":
            FolderViewController(
                app: app, folder: FolderRowVM(id: "archived", name: "Archived", count: 0, symbol: "archivebox"))
        default: nil
        }
    }

    /// The sheet a choice row opens.
    private func choose(_ id: String, from source: UIView?) {
        switch id {
        case "wallpaper:effect":
            sheet(
                "Wallpaper Effect", WallpaperStore.allEffects.map { ($0, WallpaperStore.label($0)) },
                current: WallpaperStore.effect, from: source
            ) { WallpaperStore.effect = $0 }
        case "math:font":
            sheet(
                "Math Font", MathSettings.allFaces.map { ($0, MathSettings.label($0)) }, current: MathSettings.face,
                from: source
            ) { [weak self] in
                MathSettings.face = $0
                self?.reload()
            }
        default:
            sheet("Math Size", MathSettings.allSizes.map { ($0, "\($0)%") }, current: MathSettings.size, from: source) {
                [weak self] in
                MathSettings.size = $0
                self?.reload()
            }
        }
    }

    /// An action sheet of `options`, the current one checked.
    private func sheet<T: Equatable>(
        _ title: String, _ options: [(T, String)], current: T, from source: UIView?, _ pick: @escaping (T) -> Void
    ) {
        let sheet = UIAlertController(title: title, message: nil, preferredStyle: .actionSheet)
        for (option, label) in options {
            let action = UIAlertAction(title: label, style: .default) { _ in pick(option) }
            action.setValue(option == current, forKey: "checked")
            sheet.addAction(action)
        }
        sheet.addAction(UIAlertAction(title: "Cancel", style: .cancel))
        sheet.popoverPresentationController?.sourceView = source
        present(sheet, animated: true)
    }
}
