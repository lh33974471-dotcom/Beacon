import Foundation
import Flutter

/// Manages communication between the main app and the Broadcast Upload Extension.
/// Reads screen frames from shared memory and pushes them to the Rust video pipeline
/// via the `ios_on_video_frame_update` FFI function.
///
/// Also notifies the Flutter layer (via method channel) when broadcasting starts/stops
/// so the UI can show the ID/password and connection status.
class BroadcastManager {
    static let shared = BroadcastManager()

    private var frameReader: SharedFrameReader?
    private var pollingTimer: DispatchSourceTimer?
    private var broadcastCheckTimer: DispatchSourceTimer?
    private var lastDetectedSeq: UInt32 = 0
    private(set) var isRunning = false
    private var staleHeartbeatTicks = 0
    private var lastWidth: UInt32 = 0
    private var lastHeight: UInt32 = 0
    private var lastRotation: UInt32 = 0
    private var framesPushedToRust: UInt64 = 0
    private var framesReadFromMmap: UInt64 = 0

    /// Method channel used to push broadcast state events to Flutter
    var flutterChannel: FlutterMethodChannel?

    private init() {
        NSLog("BroadcastManager: Initializing")
        registerDarwinNotifications()
        // Start a periodic check for broadcast activity.
        // Darwin notifications from extensions can be missed if the main app
        // wasn't running when the broadcast started, or in certain background states.
        startBroadcastDetection()
    }

    private func registerDarwinNotifications() {
        let center = CFNotificationCenterGetDarwinNotifyCenter()

        // Listen for broadcast started
        CFNotificationCenterAddObserver(
            center, Unmanaged.passUnretained(self).toOpaque(),
            { (_, observer, name, _, _) in
                guard let observer = observer else { return }
                let mgr = Unmanaged<BroadcastManager>.fromOpaque(observer).takeUnretainedValue()
                mgr.onBroadcastStarted()
            },
            kBroadcastStartedNotificationName as CFString,
            nil, .deliverImmediately
        )

        // Listen for broadcast stopped
        CFNotificationCenterAddObserver(
            center, Unmanaged.passUnretained(self).toOpaque(),
            { (_, observer, name, _, _) in
                guard let observer = observer else { return }
                let mgr = Unmanaged<BroadcastManager>.fromOpaque(observer).takeUnretainedValue()
                mgr.onBroadcastStopped()
            },
            kBroadcastStoppedNotificationName as CFString,
            nil, .deliverImmediately
        )
    }

    /// Periodically checks if the broadcast extension is actively writing frames.
    /// This catches the case where the Darwin notification was missed (e.g., the extension
    /// started before the app launched, or the notification was lost).
    private func startBroadcastDetection() {
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .utility))
        timer.schedule(deadline: .now() + .milliseconds(500), repeating: .seconds(1))
        timer.setEventHandler { [weak self] in
            self?.checkBroadcastActive()
        }
        timer.resume()
        broadcastCheckTimer = timer
    }

    /// Detect if the broadcast extension is actively writing frames, or has stopped.
    /// When not running: uses heartbeat timestamp and mmap sequence to detect start.
    /// When running: checks if the extension's timer-driven heartbeat has gone stale to
    /// confirm the extension actually stopped (fallback for missed Darwin notifications).
    private func checkBroadcastActive() {
        if isRunning {
            // Stop immediately on a stale heartbeat if a stop was explicitly requested;
            // otherwise require several consecutive stale ticks so a briefly-suspended
            // main app resuming ahead of the extension's next beat doesn't falsely stop.
            guard let defaults = UserDefaults(suiteName: kAppGroupIdentifier) else { return }
            let stopRequested = defaults.bool(forKey: kBroadcastStopRequestKey)
            let ts = defaults.double(forKey: kBroadcastActiveTimestampKey)
            let age = ts > 0 ? Date().timeIntervalSince1970 - ts : Double.infinity
            if ts == 0 || age > 5.0 {
                staleHeartbeatTicks += 1
                if stopRequested || staleHeartbeatTicks >= 3 {
                    NSLog("BroadcastManager: heartbeat is stale (age: %.1fs, ticks: %d, stopRequested: %d), stopping", age, staleHeartbeatTicks, stopRequested ? 1 : 0)
                    staleHeartbeatTicks = 0
                    onBroadcastStopped()
                }
            } else {
                staleHeartbeatTicks = 0
            }
            return
        }

        // Start-detection (only when not running):

        // Method 1: Check the UserDefaults heartbeat timestamp written by the extension.
        // The extension updates this every ~1 second while broadcasting.
        if let defaults = UserDefaults(suiteName: kAppGroupIdentifier) {
            let ts = defaults.double(forKey: kBroadcastActiveTimestampKey)
            if ts > 0 {
                let age = Date().timeIntervalSince1970 - ts
                if age < 3.0 {
                    NSLog("BroadcastManager: Detected active broadcast via UserDefaults timestamp (age: %.1fs)", age)
                    onBroadcastStarted()
                    return
                }
            }
        }

        // Method 2: Compare the mmap sequence number across consecutive timer ticks.
        // A changing sequence number proves the extension is actively writing frames.
        // (A stale header from a previous session will have a fixed sequence.)
        guard let containerURL = FileManager.default.containerURL(
            forSecurityApplicationGroupIdentifier: kAppGroupIdentifier
        ) else { return }

        let fileURL = containerURL.appendingPathComponent(kSharedFrameFileName)
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return }

        guard let handle = try? FileHandle(forReadingFrom: fileURL) else { return }
        defer { handle.closeFile() }
        let headerData = handle.readData(ofLength: kFrameHeaderSize)
        guard headerData.count >= kFrameHeaderSize else { return }

        headerData.withUnsafeBytes { rawBuf in
            guard let ptr = rawBuf.baseAddress?.assumingMemoryBound(to: UInt32.self) else { return }
            let magic = ptr[0]
            let seq = ptr[1]
            let width = ptr[2]
            let height = ptr[3]

            guard magic == kFrameMagic && seq > 0 && width > 0 && height > 0 else {
                lastDetectedSeq = 0
                return
            }

            // If the sequence number changed since our last check, the extension is actively writing
            if lastDetectedSeq > 0 && seq != lastDetectedSeq {
                NSLog("BroadcastManager: Detected active broadcast via mmap sequence change (%u -> %u, %ux%u)", lastDetectedSeq, seq, width, height)
                lastDetectedSeq = 0
                onBroadcastStarted()
            } else {
                // Record the sequence — we'll check again on the next timer tick
                lastDetectedSeq = seq
            }
        }
    }

    private func onBroadcastStarted() {
        NSLog("BroadcastManager: Broadcast started notification received")
        startPolling()
        // Notify Flutter on the main thread
        DispatchQueue.main.async {
            NSLog("BroadcastManager: Sending broadcast_started to Flutter")
            self.flutterChannel?.invokeMethod("broadcast_started", arguments: nil)
        }
    }

    private func onBroadcastStopped() {
        NSLog("BroadcastManager: Broadcast stopped notification received")
        stopPolling()
        // Notify Flutter on the main thread
        DispatchQueue.main.async {
            NSLog("BroadcastManager: Sending broadcast_stopped to Flutter")
            self.flutterChannel?.invokeMethod("broadcast_stopped", arguments: nil)
        }
    }

    /// Request the broadcast extension to stop.
    /// Sets a shared UserDefaults flag and sends a Darwin notification
    /// that the extension listens for. The extension calls finishBroadcastWithError()
    /// which triggers broadcastFinished() → sends broadcast_stopped back to us.
    func requestStopBroadcast() {
        NSLog("BroadcastManager: Requesting broadcast extension to stop")
        if let defaults = UserDefaults(suiteName: kAppGroupIdentifier) {
            defaults.set(true, forKey: kBroadcastStopRequestKey)
        }
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        CFNotificationCenterPostNotification(
            center,
            CFNotificationName(kBroadcastStopRequestNotificationName as CFString),
            nil, nil, true
        )
    }

    func startPolling() {
        guard !isRunning else { return }
        isRunning = true
        staleHeartbeatTicks = 0

        frameReader = SharedFrameReader()
        guard frameReader != nil else {
            NSLog("BroadcastManager: Failed to create SharedFrameReader")
            isRunning = false
            return
        }

        // Enable frame capture in Rust
        "video".withCString { name in
            ios_set_frame_raw_enable(name, true)
        }

        // Poll at ~30fps (33ms intervals)
        let timer = DispatchSource.makeTimerSource(queue: DispatchQueue.global(qos: .userInteractive))
        timer.schedule(deadline: .now(), repeating: .milliseconds(33))
        timer.setEventHandler { [weak self] in
            self?.pollFrame()
        }
        timer.resume()
        pollingTimer = timer

        NSLog("BroadcastManager: Started frame polling")
    }

    func stopPolling() {
        guard isRunning else { return }
        isRunning = false

        pollingTimer?.cancel()
        pollingTimer = nil
        frameReader = nil

        // Disable frame capture in Rust
        "video".withCString { name in
            ios_set_frame_raw_enable(name, false)
        }

        // Reset screen size
        ios_set_screen_size(0, 0, 0)
        ios_set_frame_rotation(0)
        lastWidth = 0
        lastHeight = 0
        lastRotation = 0

        NSLog("BroadcastManager: Stopped frame polling")
    }

    private func pollFrame() {
        guard let reader = frameReader else { return }
        guard let (dataPtr, dataLen, width, height, stride, rotation) = reader.readNewFrame() else { return }

        framesReadFromMmap += 1

        if rotation != lastRotation {
            lastRotation = rotation
            ios_set_frame_rotation(rotation)
            NSLog("BroadcastManager: Frame rotation updated to \(rotation)")
        }

        // Update screen size if it changed
        if width != lastWidth || height != lastHeight {
            lastWidth = width
            lastHeight = height
            // Scale of 1 for now; could calculate from UIScreen.main.scale
            ios_set_screen_size(UInt16(width), UInt16(height), 1)
            NSLog("BroadcastManager: Screen size updated to \(width)x\(height)")
        }

        // Push frame to Rust video pipeline
        ios_on_video_frame_update(dataPtr.assumingMemoryBound(to: UInt8.self), UInt(dataLen))
        framesPushedToRust += 1

        // Log periodically (first 3, then every 100th) — include Rust-side diagnostic state
        if framesPushedToRust <= 3 || framesPushedToRust % 100 == 0 {
            var rustState = "?"
            if let ptr = ios_get_diagnostic_state() {
                rustState = String(cString: ptr)
                ios_free_diagnostic_string(ptr)
            }
            NSLog("BroadcastManager: frame #%llu (len=%d, %ux%u) rust_state=[%@]", framesPushedToRust, dataLen, width, height, rustState)
        }
    }

    deinit {
        broadcastCheckTimer?.cancel()
        broadcastCheckTimer = nil
        stopPolling()
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        CFNotificationCenterRemoveEveryObserver(center, Unmanaged.passUnretained(self).toOpaque())
    }
}
