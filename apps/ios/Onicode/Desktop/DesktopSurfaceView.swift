import AVFoundation
import CoreMedia
import UIKit

/// Where a Mac's pictures go, and where touch comes from.
///
/// The view's layer is an `AVSampleBufferDisplayLayer`: each decoded `CVPixelBuffer` is wrapped in
/// a `CMSampleBuffer` marked to display immediately and enqueued from the decoder's thread, so a
/// picture waits for no main-thread frame. A picture coded as two stripes goes up on two more
/// display layers, each clipped to its rows. The Mac's pointer is drawn over them.
///
/// Touch drives the Mac like a trackpad the size of the screen: one finger moves the pointer as
/// far as it moved, a tap clicks, a double tap double-clicks, a press then a drag drags, two
/// fingers scroll (with phases) or right-click with a tap, and a pinch magnifies.
final class DesktopSurfaceView: UIView, @unchecked Sendable {
    override static var layerClass: AnyClass { AVSampleBufferDisplayLayer.self }

    weak var viewer: DesktopViewer?

    /// The view's own layer, for the decoder's thread to enqueue on.
    nonisolated(unsafe) private var display: AVSampleBufferDisplayLayer!

    /// The two stripes' layers, each in a container that clips it to its rows.
    private let topClip = CALayer()
    private let lowerClip = CALayer()
    nonisolated(unsafe) private let topStripe = AVSampleBufferDisplayLayer()
    nonisolated(unsafe) private let lowerStripe = AVSampleBufferDisplayLayer()
    private let cursorLayer = CALayer()
    let keys = KeyCatcher()

    /// The picture's size in pixels, and how the stripes meet; read on the main thread.
    private var pictureSize = CGSize.zero
    nonisolated(unsafe) private var stripes: StripeRows?
    private var cursorSerial: UInt64 = .max

    /// The newest picture enqueued, reported to the viewer at the next display refresh.
    private let pending = NSLock()
    nonisolated(unsafe) private var pendingToken: UInt64?
    private var link: CADisplayLink?

    /// What the touch gestures remember between events.
    var touch = TouchMemory()

    override init(frame: CGRect) {
        super.init(frame: frame)
        display = layer as? AVSampleBufferDisplayLayer
        backgroundColor = .black
        display.videoGravity = .resizeAspect
        for (clip, stripe) in [(topClip, topStripe), (lowerClip, lowerStripe)] {
            clip.masksToBounds = true
            clip.isHidden = true
            stripe.videoGravity = .resize
            clip.addSublayer(stripe)
            layer.addSublayer(clip)
        }
        cursorLayer.isHidden = true
        cursorLayer.zPosition = 10
        layer.addSublayer(cursorLayer)
        keys.surface = self
        addSubview(keys)
        isMultipleTouchEnabled = true
        installGestures()
        accessibilityIdentifier = "desktop-surface"
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError("init(coder:) is not supported") }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        link?.invalidate()
        link = nil
        guard window != nil else { return }
        let link = CADisplayLink(target: self, selector: #selector(refreshed))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        viewer?.setViewSize(width: Float(bounds.width), height: Float(bounds.height))
        layoutStripes()
    }

    override func pressesBegan(_ presses: Set<UIPress>, with event: UIPressesEvent?) {
        if !sendKeys(presses) { super.pressesBegan(presses, with: event) }
    }

    // MARK: Pictures

    /// Show `buffer`, whole. Any thread.
    nonisolated func show(_ buffer: CVPixelBuffer, token: UInt64) {
        guard let sample = Self.sample(buffer) else { return }
        enqueue(sample, on: display)
        remember(token)
        if stripes != nil {
            DispatchQueue.main.async { [weak self] in
                MainActor.assumeIsolated { self?.setStriped(nil) }
            }
        }
    }

    /// Show a picture coded as two stripes: `top`'s first `topRows` rows, `lower`'s from
    /// `lowerFrom` down under them. Any thread.
    nonisolated func show(top: CVPixelBuffer, lower: CVPixelBuffer, topRows: Int, lowerFrom: Int, token: UInt64) {
        guard let topSample = Self.sample(top), let lowerSample = Self.sample(lower) else { return }
        enqueue(topSample, on: topStripe)
        enqueue(lowerSample, on: lowerStripe)
        remember(token)
        let shape = StripeRows(topRows: topRows, lowerFrom: lowerFrom, lowerHeight: CVPixelBufferGetHeight(lower))
        let width = CVPixelBufferGetWidth(top)
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.restripe(shape, width: width) }
        }
    }

    /// The stripes' rows changed (a resize, a first striped picture): lay them out anew.
    private func restripe(_ shape: StripeRows, width: Int) {
        guard stripes != shape || Int(pictureSize.width) != width else { return }
        pictureSize = CGSize(width: width, height: shape.topRows + shape.lowerHeight - shape.lowerFrom)
        setStriped(shape)
    }

    nonisolated private func enqueue(_ sample: CMSampleBuffer, on layer: AVSampleBufferDisplayLayer) {
        let renderer = layer.sampleBufferRenderer
        if renderer.status == .failed {
            renderer.flush()
        }
        renderer.enqueue(sample)
    }

    nonisolated private func remember(_ token: UInt64) {
        pending.lock()
        pendingToken = token
        pending.unlock()
    }

    /// A display refresh: the picture enqueued before it is on the glass now, near enough.
    @objc private func refreshed() {
        pending.lock()
        let token = pendingToken
        pendingToken = nil
        pending.unlock()
        if let token {
            viewer?.presented(token: token, agoNs: 0)
        }
    }

    private func setStriped(_ shape: StripeRows?) {
        stripes = shape
        let striped = shape != nil
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        topClip.isHidden = !striped
        lowerClip.isHidden = !striped
        if striped {
            display.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
        } else {
            topStripe.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
            lowerStripe.sampleBufferRenderer.flush(removingDisplayedImage: true, completionHandler: nil)
        }
        CATransaction.commit()
        layoutStripes()
    }

    /// The picture's place in the view, letterboxed: the same fit Rust maps touch by.
    private var pictureRect: CGRect {
        guard pictureSize.width > 0, pictureSize.height > 0, bounds.width > 0, bounds.height > 0 else {
            return bounds
        }
        let scale = min(bounds.width / pictureSize.width, bounds.height / pictureSize.height)
        let size = CGSize(width: pictureSize.width * scale, height: pictureSize.height * scale)
        return CGRect(
            x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2, width: size.width,
            height: size.height)
    }

    private func layoutStripes() {
        guard let stripes else { return }
        let rect = pictureRect
        let scale = rect.height / max(pictureSize.height, 1)
        let topHeight = CGFloat(stripes.topRows) * scale
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        topClip.frame = CGRect(x: rect.minX, y: rect.minY, width: rect.width, height: topHeight)
        topStripe.frame = CGRect(
            x: 0, y: 0, width: rect.width, height: CGFloat(stripes.topRows + stripes.lowerFrom) * scale)
        lowerClip.frame = CGRect(
            x: rect.minX, y: rect.minY + topHeight, width: rect.width, height: rect.height - topHeight)
        lowerStripe.frame = CGRect(
            x: 0, y: -CGFloat(stripes.lowerFrom) * scale, width: rect.width,
            height: CGFloat(stripes.lowerHeight) * scale)
        CATransaction.commit()
    }

    /// A `CMSampleBuffer` around `buffer`, marked to display at once.
    nonisolated private static func sample(_ buffer: CVPixelBuffer) -> CMSampleBuffer? {
        var format: CMVideoFormatDescription?
        guard
            CMVideoFormatDescriptionCreateForImageBuffer(
                allocator: kCFAllocatorDefault, imageBuffer: buffer, formatDescriptionOut: &format) == noErr,
            let format
        else { return nil }
        var timing = CMSampleTimingInfo(
            duration: .invalid, presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        guard
            CMSampleBufferCreateReadyWithImageBuffer(
                allocator: kCFAllocatorDefault, imageBuffer: buffer, formatDescription: format,
                sampleTiming: &timing, sampleBufferOut: &sample) == noErr,
            let sample
        else { return nil }
        if let attachments = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: true),
            CFArrayGetCount(attachments) > 0
        {
            let dict = unsafeBitCast(CFArrayGetValueAtIndex(attachments, 0), to: CFMutableDictionary.self)
            CFDictionarySetValue(
                dict, Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
                Unmanaged.passUnretained(kCFBooleanTrue).toOpaque())
        }
        return sample
    }

    // MARK: State and pointer

    func apply(_ state: DesktopStateInfo) {
        if stripes == nil, state.width > 0 {
            pictureSize = CGSize(width: Int(state.width), height: Int(state.height))
        }
        if state.secureField {
            keys.isSecureTextEntry = true
        } else {
            keys.isSecureTextEntry = false
        }
    }

    func place(_ cursor: DesktopCursor) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        defer { CATransaction.commit() }
        guard cursor.visible else {
            cursorLayer.isHidden = true
            return
        }
        if cursor.shapeSerial != cursorSerial {
            cursorSerial = cursor.shapeSerial
            cursorLayer.contents = cursor.shape.flatMap(Self.image) ?? Self.arrow
            let scale = CGFloat(max(cursor.shape?.scale ?? 2, 1))
            let size = cursor.shape.map { CGSize(width: CGFloat($0.width) / scale, height: CGFloat($0.height) / scale) }
            cursorLayer.bounds = CGRect(origin: .zero, size: size ?? CGSize(width: 14, height: 20))
            let hot = cursor.shape.map { CGPoint(x: CGFloat($0.hotX) / scale, y: CGFloat($0.hotY) / scale) } ?? .zero
            cursorLayer.anchorPoint = CGPoint(
                x: hot.x / max(cursorLayer.bounds.width, 1), y: hot.y / max(cursorLayer.bounds.height, 1))
        }
        cursorLayer.position = CGPoint(x: CGFloat(cursor.x), y: CGFloat(cursor.y))
        cursorLayer.isHidden = false
    }

    private static func image(_ shape: DesktopCursorShape) -> CGImage? {
        let (w, h) = (Int(shape.width), Int(shape.height))
        guard w > 0, h > 0, shape.bgra.count >= w * h * 4,
            let provider = CGDataProvider(data: Data(shape.bgra) as CFData)
        else { return nil }
        return CGImage(
            width: w, height: h, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: w * 4,
            space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGBitmapInfo(
                rawValue: CGBitmapInfo.byteOrder32Little.rawValue | CGImageAlphaInfo.premultipliedFirst.rawValue),
            provider: provider, decode: nil, shouldInterpolate: true, intent: .defaultIntent)
    }

    /// The system arrow, for a Mac that sent no picture of its pointer.
    private static let arrow: CGImage? = {
        let config = UIImage.SymbolConfiguration(pointSize: 18, weight: .regular)
        return UIImage(systemName: "cursorarrow", withConfiguration: config)?
            .withTintColor(.white, renderingMode: .alwaysOriginal).cgImage
    }()
}

/// How a picture coded as two stripes is cut: `top`'s first `topRows` rows, then `lower`'s rows
/// from `lowerFrom` on, of the `lowerHeight` it has.
private struct StripeRows: Equatable, Sendable {
    let topRows: Int
    let lowerFrom: Int
    let lowerHeight: Int
}
