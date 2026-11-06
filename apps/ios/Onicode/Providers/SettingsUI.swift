import UIKit

/// Small pieces Settings' pages share: symbols at a size, and core errors as
/// a line a person can read.
enum SettingsUI {
    static func symbol(_ name: String, _ size: CGFloat = 17, weight: UIImage.SymbolWeight = .medium) -> UIImage? {
        UIImage(systemName: name, withConfiguration: UIImage.SymbolConfiguration(pointSize: size, weight: weight))
    }

    /// What went wrong, as the core put it.
    static func message(_ error: Error) -> String {
        guard let core = error as? CoreError else { return error.localizedDescription }
        switch core {
        case let .NotFound(m), let .InvalidArgument(m), let .HostUnavailable(m), let .Unsupported(m),
            let .HostError(m), let .Storage(m), let .NotImplemented(m), let .Internal(m):
            return m
        case .Closed:
            return "Onicode stopped. Open it again."
        }
    }
}

extension UIFont {
    /// Figures that don't shift as they change.
    func withMonospacedDigits() -> UIFont {
        let descriptor = fontDescriptor.addingAttributes([
            .featureSettings: [
                [
                    UIFontDescriptor.FeatureKey.type: kNumberSpacingType,
                    UIFontDescriptor.FeatureKey.selector: kMonospacedNumbersSelector,
                ]
            ]
        ])
        return UIFont(descriptor: descriptor, size: pointSize)
    }
}

extension UIImage {
    /// The image, fainter (a dimmed row's mark).
    func withAlpha(_ alpha: CGFloat) -> UIImage? {
        UIGraphicsImageRenderer(size: size, format: imageRendererFormat).image { _ in
            draw(at: .zero, blendMode: .normal, alpha: alpha)
        }
    }
}
