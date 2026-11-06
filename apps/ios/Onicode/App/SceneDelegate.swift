import UIKit

final class SceneDelegate: UIResponder, UIWindowSceneDelegate {
    var window: UIWindow?
    private let app = AppModel()

    func scene(
        _ scene: UIScene,
        willConnectTo _: UISceneSession,
        options _: UIScene.ConnectionOptions
    ) {
        guard let scene = scene as? UIWindowScene else { return }
        let window = UIWindow(windowScene: scene)
        window.overrideUserInterfaceStyle =
            UIUserInterfaceStyle(rawValue: UserDefaults.standard.integer(forKey: "appearance")) ?? .unspecified
        window.tintColor = Palette.accent
        self.window = window
        let args = ProcessInfo.processInfo.arguments
        // `-wallpaper <path> [-wallpaper-effect ascii]`: set the wallpaper at
        // launch (screenshots, tests); `-wallpaper none` clears it.
        if let i = args.firstIndex(of: "-wallpaper"), i + 1 < args.count {
            if args[i + 1] == "none" {
                WallpaperStore.remove()
            } else if let image = UIImage(contentsOfFile: args[i + 1]) {
                WallpaperStore.set(image, name: (args[i + 1] as NSString).lastPathComponent)
            }
        }
        if let i = args.firstIndex(of: "-wallpaper-effect"), i + 1 < args.count,
            let effect = WallpaperStore.allEffects.first(where: { WallpaperStore.key($0) == args[i + 1] })
        {
            WallpaperStore.effect = effect
        }
        if args.contains("-lab") {
            window.rootViewController = MainTabController.nav(TranscriptLabViewController())
        } else {
            // The split shell everywhere: it shows the sidebar when the window
            // is regular in both directions (iPad, an unfolded iPhone Duo) and
            // collapses to the tab shell otherwise, live as the window changes —
            // folding or unfolding carries the open session across.
            window.rootViewController = SplitRootController(app: app)
        }
        window.makeKeyAndVisible()

        #if DEBUG
        if args.contains("-voice-preview") {
            app.voice.preview()
            if args.contains("-voice-stage") {
                DispatchQueue.main.async { [app] in window.rootViewController?.presentVoiceStage(app: app, source: nil)
                }
            }
        }
        #endif
        #if DEBUG
        TerminalProbe.runIfAsked(app: app, router: window.rootViewController as? AppRouter)
        #endif
        if let router = window.rootViewController as? AppRouter, let i = args.firstIndex(of: "-route"),
            i + 1 < args.count
        {
            let route = args[i + 1]
            DispatchQueue.main.async {
                if route.hasPrefix("chat:") { router.openSession(String(route.dropFirst(5))) }
                if route == "new" { router.presentNewSession(prompt: nil) }
                if route == "more" { router.showSettings() }
                if route == "search" { router.showSearch() }
                #if DEBUG
                // Settings pages straight away (screenshots, tests).
                if route.hasPrefix("providers") || route.hasPrefix("accounts") || route == "computers" {
                    self.openSettingsRoute(route)
                }
                #endif
            }
        }
    }

    #if DEBUG
    /// `-route providers[:<device id>]` · `accounts:<harness>` · `computers`.
    @MainActor private func openSettingsRoute(_ route: String) {
        guard let root = window?.rootViewController else { return }
        let parts = route.split(separator: ":").map(String.init)
        if parts.first == "providers" {
            root.present(
                UINavigationController(
                    rootViewController: ProvidersViewController(app: app, target: parts.dropFirst().first)),
                animated: false)
            return
        }
        if parts.first == "accounts", let harness = parts.dropFirst().first, let host = app.hostOptions.first {
            // A computer's accounts for one agent (the first computer),
            // reached the way the app does: pushed from Providers.
            let nav = UINavigationController(rootViewController: ProvidersViewController(app: app, target: host.id))
            nav.pushViewController(
                AgentAccountsViewController(
                    app: app, target: .init(id: host.id, name: host.name, harness: harness, label: harness)),
                animated: false
            )
            root.present(nav, animated: false)
            return
        }
        if parts.first == "computers" {
            root.present(
                UINavigationController(rootViewController: EngineHostsViewController(app: app)), animated: false)
        }
    }
    #endif

    /// `onicode://voice` (the call's Live Activity) returns to the stage.
    func scene(_: UIScene, openURLContexts urlContexts: Set<UIOpenURLContext>) {
        guard urlContexts.contains(where: { $0.url.scheme == "onicode" && $0.url.host == "voice" }),
            app.voice.live, let root = window?.rootViewController
        else { return }
        root.presentVoiceStage(app: app, source: nil)
    }

    func sceneDidEnterBackground(_: UIScene) {
        app.didEnterBackground()
    }

    func sceneWillEnterForeground(_: UIScene) {
        app.willEnterForeground()
    }
}
