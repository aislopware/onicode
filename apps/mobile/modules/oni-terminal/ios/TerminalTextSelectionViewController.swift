import UIKit

final class TerminalTextSelectionViewController: UIViewController {
  private let text: String
  private let fontSize: CGFloat
  private let labels: [String: String]

  init(text: String, fontSize: CGFloat, labels: [String: String]) {
    self.text = text
    self.labels = labels
    self.fontSize = fontSize
    super.init(nibName: nil, bundle: nil)
  }

  required init?(coder: NSCoder) {
    return nil
  }

  override func viewDidLoad() {
    super.viewDidLoad()
    title = labels["title"] ?? "Terminal text"
    view.backgroundColor = .systemBackground

    navigationItem.leftBarButtonItem = UIBarButtonItem(
      title: labels["copyAll"] ?? "Copy All", style: .plain, target: self, action: #selector(copyAll))
    // UIKit's own "Done" follows the app bundle's languages, which is English only.
    if #available(iOS 26.0, *) {
      let done = UIBarButtonItem(barButtonSystemItem: .done, target: self, action: #selector(close))
      done.accessibilityLabel = labels["done"]
      navigationItem.rightBarButtonItem = done
    } else {
      navigationItem.rightBarButtonItem = UIBarButtonItem(
        title: labels["done"] ?? "Done", style: .done, target: self, action: #selector(close))
    }

    let textView = UITextView()
    textView.text = text
    textView.font = .monospacedSystemFont(ofSize: max(fontSize, 14), weight: .regular)
    textView.textColor = .label
    textView.backgroundColor = .systemBackground
    textView.isEditable = false
    textView.isSelectable = true
    // Login flows print auth URLs; tapping one opens it.
    textView.dataDetectorTypes = .link
    textView.accessibilityLabel = labels["output"] ?? "Terminal output"
    textView.textContainerInset = UIEdgeInsets(top: 16, left: 12, bottom: 16, right: 12)
    textView.translatesAutoresizingMaskIntoConstraints = false
    view.addSubview(textView)

    NSLayoutConstraint.activate([
      textView.leadingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.leadingAnchor),
      textView.trailingAnchor.constraint(equalTo: view.safeAreaLayoutGuide.trailingAnchor),
      textView.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor),
      textView.bottomAnchor.constraint(equalTo: view.safeAreaLayoutGuide.bottomAnchor),
    ])
  }

  @objc
  private func copyAll() {
    UIPasteboard.general.string = text
    close()
  }

  @objc
  private func close() {
    dismiss(animated: !UIAccessibility.isReduceMotionEnabled)
  }
}
