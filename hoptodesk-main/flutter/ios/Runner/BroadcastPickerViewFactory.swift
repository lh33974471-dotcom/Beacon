import Flutter
import ReplayKit
import UIKit

/// Singleton that holds a hidden RPSystemBroadcastPickerView and can
/// programmatically trigger its internal button to show the broadcast sheet.
class BroadcastPickerHelper {
    static let shared = BroadcastPickerHelper()

    private var pickerView: RPSystemBroadcastPickerView?

    private init() {}

    func setup() {
        // Use a reasonable frame size — some iOS versions ignore very small picker views
        let picker = RPSystemBroadcastPickerView(frame: CGRect(x: -100, y: -100, width: 44, height: 44))
        picker.preferredExtension = Bundle.main.bundleIdentifier.map { $0 + ".BroadcastUploadExtension" }
        picker.showsMicrophoneButton = false
        picker.alpha = 0
        self.pickerView = picker

        // Add to key window so it exists in the view hierarchy (required for button action)
        if let window = UIApplication.shared.windows.first {
            window.addSubview(picker)
            NSLog("[BroadcastPicker] setup complete, added to window, preferredExtension: \(picker.preferredExtension ?? "nil")")
            NSLog("[BroadcastPicker] picker subviews: \(picker.subviews)")
        } else {
            NSLog("[BroadcastPicker] WARNING: no window found during setup")
        }
    }

    func showPicker() {
        guard let picker = pickerView else {
            NSLog("[BroadcastPicker] showPicker called but pickerView is nil")
            return
        }
        NSLog("[BroadcastPicker] showPicker called, subviews count: \(picker.subviews.count)")
        // Find the internal UIButton and send a tap action
        for subview in picker.subviews {
            NSLog("[BroadcastPicker] subview: \(type(of: subview))")
            if let button = subview as? UIButton {
                NSLog("[BroadcastPicker] Found UIButton, sending touchUpInside")
                button.sendActions(for: .allTouchEvents)
                return
            }
        }
        NSLog("[BroadcastPicker] WARNING: No UIButton found in picker subviews")
    }
}

/// Factory for the platform view (kept for backwards compatibility but the view
/// is now just an empty transparent placeholder; the real picker is triggered
/// via the method channel).
class BroadcastPickerViewFactory: NSObject, FlutterPlatformViewFactory {
    private var messenger: FlutterBinaryMessenger

    init(messenger: FlutterBinaryMessenger) {
        self.messenger = messenger
        super.init()
    }

    func create(
        withFrame frame: CGRect,
        viewIdentifier viewId: Int64,
        arguments args: Any?
    ) -> FlutterPlatformView {
        return BroadcastPickerPlatformView(frame: frame, viewId: viewId, messenger: messenger)
    }

    func createArgsCodec() -> FlutterMessageCodec & NSObjectProtocol {
        return FlutterStandardMessageCodec.sharedInstance()
    }
}

class BroadcastPickerPlatformView: NSObject, FlutterPlatformView {
    private var emptyView: UIView

    init(frame: CGRect, viewId: Int64, messenger: FlutterBinaryMessenger) {
        emptyView = UIView(frame: frame)
        emptyView.backgroundColor = .clear
        super.init()
    }

    func view() -> UIView {
        return emptyView
    }
}
