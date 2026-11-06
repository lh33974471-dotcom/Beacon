import UIKit
import Flutter
import AVFoundation

/// Top-level C-compatible function for the Rust log callback.
/// Must be a free function (not a closure) for @convention(c) compatibility.
private func rustLogBridge(_ msg: UnsafePointer<CChar>?) {
    if let msg = msg {
        NSLog("[Rust] %s", String(cString: msg))
    }
}

@main
@objc class AppDelegate: FlutterAppDelegate {
  override func application(
    _ application: UIApplication,
    didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]?
  ) -> Bool {
    GeneratedPluginRegistrant.register(with: self)
    do {
      try AVAudioSession.sharedInstance().setCategory(.ambient)
    } catch {
      NSLog("Failed to configure audio mixing: %@", error.localizedDescription)
    }
    dummyMethodToEnforceBundling();

    // Register NSLog callback so Rust code can write to the iOS system log.
    // (env_logger's stderr output is not visible in release/TestFlight builds.)
    ios_set_log_callback(rustLogBridge)
    let managed = UserDefaults.standard.dictionary(forKey: "com.apple.configuration.managed")
    if let token = managed?["enrollment_token"] as? String {
        ios_set_managed_enrollment_token(token)
    }
    if let url = managed?["custom_api_url"] as? String {
        ios_set_managed_api_url(url)
    }

    let controller = window?.rootViewController as! FlutterViewController

    // Set up the broadcast picker method channel (Flutter → native)
    let broadcastChannel = FlutterMethodChannel(name: "com.hoptodesk.app/broadcast_picker", binaryMessenger: controller.binaryMessenger)
    broadcastChannel.setMethodCallHandler { (call, result) in
        switch call.method {
        case "show_broadcast_picker":
            BroadcastPickerHelper.shared.showPicker()
            result(nil)
        case "stop_broadcast":
            BroadcastManager.shared.requestStopBroadcast()
            result(nil)
        case "get_broadcast_status":
            result(BroadcastManager.shared.isRunning)
        default:
            result(FlutterMethodNotImplemented)
        }
    }

    // Initialize BroadcastManager and give it the channel for native → Flutter events
    BroadcastManager.shared.flutterChannel = broadcastChannel

    // Set up the hidden broadcast picker for programmatic triggering
    BroadcastPickerHelper.shared.setup()

    // Register RPSystemBroadcastPickerView as a Flutter platform view (legacy, kept for compat)
    let factory = BroadcastPickerViewFactory(messenger: controller.binaryMessenger)
    registrar(forPlugin: "BroadcastPickerView")!
        .register(factory, withId: "broadcast_picker_view")

    return super.application(application, didFinishLaunchingWithOptions: launchOptions)
  }

  public func dummyMethodToEnforceBundling() {
      dummy_method_to_enforce_bundling();
    session_get_rgba(nil, 0);
  }
}
