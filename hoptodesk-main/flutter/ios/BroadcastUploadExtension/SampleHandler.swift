import ReplayKit
import CoreVideo
import Accelerate
import ImageIO

class SampleHandler: RPBroadcastSampleHandler {
    private var frameWriter: SharedFrameWriter?
    private let heartbeatQueue = DispatchQueue(label: "com.hoptodesk.app.heartbeat")
    private var heartbeatTimer: DispatchSourceTimer?
    /// Cached vImage conversion info (reused across frames)
    private var conversionInfo: vImage_YpCbCrToARGB?
    private var conversionInfoFullRange: Bool?

    override func broadcastStarted(withSetupInfo setupInfo: [String: NSObject]?) {
        frameWriter = SharedFrameWriter()

        // Clear any previous stop request and mark broadcast as active
        if let defaults = UserDefaults(suiteName: kAppGroupIdentifier) {
            defaults.set(false, forKey: kBroadcastStopRequestKey)
            defaults.set(Date().timeIntervalSince1970, forKey: kBroadcastActiveTimestampKey)
        }

        let timer = DispatchSource.makeTimerSource(queue: heartbeatQueue)
        timer.schedule(deadline: .now(), repeating: .seconds(1))
        timer.setEventHandler {
            UserDefaults(suiteName: kAppGroupIdentifier)?
                .set(Date().timeIntervalSince1970, forKey: kBroadcastActiveTimestampKey)
        }
        timer.resume()
        heartbeatTimer = timer

        // Listen for stop request from main app via Darwin notification
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        CFNotificationCenterAddObserver(
            center, Unmanaged.passUnretained(self).toOpaque(),
            { (_, observer, _, _, _) in
                guard let observer = observer else { return }
                let handler = Unmanaged<SampleHandler>.fromOpaque(observer).takeUnretainedValue()
                handler.handleStopRequest()
            },
            kBroadcastStopRequestNotificationName as CFString,
            nil, .deliverImmediately
        )

        // Notify main app that broadcasting started
        CFNotificationCenterPostNotification(
            center,
            CFNotificationName(kBroadcastStartedNotificationName as CFString),
            nil, nil, true
        )
    }

    /// Called when the main app requests stopping the broadcast.
    /// Verifies via shared UserDefaults flag then ends the broadcast.
    private func handleStopRequest() {
        if let defaults = UserDefaults(suiteName: kAppGroupIdentifier),
           defaults.bool(forKey: kBroadcastStopRequestKey) {
            let error = NSError(
                domain: RPRecordingErrorDomain,
                code: RPRecordingErrorCode.userDeclined.rawValue,
                userInfo: [NSLocalizedDescriptionKey: "Screen sharing stopped by user"]
            )
            finishBroadcastWithError(error)
        }
    }

    override func broadcastPaused() {
        // No-op: frames just stop arriving
    }

    override func broadcastResumed() {
        // No-op: frames resume arriving
    }

    override func broadcastFinished() {
        // Remove Darwin notification observer
        let center = CFNotificationCenterGetDarwinNotifyCenter()
        CFNotificationCenterRemoveEveryObserver(center, Unmanaged.passUnretained(self).toOpaque())

        heartbeatTimer?.cancel()
        heartbeatTimer = nil

        // Clear stop flag and broadcast active timestamp
        heartbeatQueue.sync {
            if let defaults = UserDefaults(suiteName: kAppGroupIdentifier) {
                defaults.set(false, forKey: kBroadcastStopRequestKey)
                defaults.removeObject(forKey: kBroadcastActiveTimestampKey)
            }
        }

        // Notify main app that broadcasting stopped
        CFNotificationCenterPostNotification(
            center,
            CFNotificationName(kBroadcastStoppedNotificationName as CFString),
            nil, nil, true
        )
        frameWriter = nil
    }

    override func processSampleBuffer(_ sampleBuffer: CMSampleBuffer, with sampleBufferType: RPSampleBufferType) {
        switch sampleBufferType {
        case .video:
            processVideoSample(sampleBuffer)
        case .audioApp:
            // App audio - could be forwarded if needed
            break
        case .audioMic:
            // Mic audio - could be forwarded if needed
            break
        @unknown default:
            break
        }
    }

    private func rotationDegrees(_ sampleBuffer: CMSampleBuffer) -> UInt32 {
        guard let attachment = CMGetAttachment(
                  sampleBuffer,
                  key: RPVideoSampleOrientationKey as CFString,
                  attachmentModeOut: nil
              ) as? NSNumber,
              let orientation = CGImagePropertyOrientation(rawValue: attachment.uint32Value) else {
            return 0
        }
        switch orientation {
        case .left, .leftMirrored: return 90
        case .down, .downMirrored: return 180
        case .right, .rightMirrored: return 270
        default: return 0
        }
    }

    private func processVideoSample(_ sampleBuffer: CMSampleBuffer) {
        guard let writer = frameWriter else { return }
        guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }

        let rotation = rotationDegrees(sampleBuffer)

        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }

        let width = CVPixelBufferGetWidth(pixelBuffer)
        let height = CVPixelBufferGetHeight(pixelBuffer)
        let pixelFormat = CVPixelBufferGetPixelFormatType(pixelBuffer)

        // Check if the pixel buffer is already in a 32-bit BGRA/ARGB format
        let isBGRA = (pixelFormat == kCVPixelFormatType_32BGRA ||
                      pixelFormat == kCVPixelFormatType_32ARGB)

        if isBGRA {
            // Already BGRA — write directly
            guard let baseAddress = CVPixelBufferGetBaseAddress(pixelBuffer) else { return }
            let stride = CVPixelBufferGetBytesPerRow(pixelBuffer)
            let dataLen = stride * height
            writer.writeFrame(
                pixelData: baseAddress,
                dataLen: dataLen,
                width: UInt32(width),
                height: UInt32(height),
                stride: UInt32(stride),
                rotation: rotation
            )
        } else if CVPixelBufferIsPlanar(pixelBuffer) {
            // YUV 420 bi-planar (420v/420f) — convert to BGRA using vImage/Accelerate
            guard CVPixelBufferGetPlaneCount(pixelBuffer) >= 2 else { return }
            guard let yPlane = CVPixelBufferGetBaseAddressOfPlane(pixelBuffer, 0),
                  let uvPlane = CVPixelBufferGetBaseAddressOfPlane(pixelBuffer, 1) else { return }

            let yStride = CVPixelBufferGetBytesPerRowOfPlane(pixelBuffer, 0)
            let uvStride = CVPixelBufferGetBytesPerRowOfPlane(pixelBuffer, 1)
            let yHeight = CVPixelBufferGetHeightOfPlane(pixelBuffer, 0)

            let bgraStride = width * 4
            let bgraLen = bgraStride * yHeight

            guard bgraLen <= writer.frameCapacity,
                  let destData = writer.frameDataPointer() else { return }

            // Set up vImage buffers for YUV → BGRA conversion
            var yBuf = vImage_Buffer(
                data: UnsafeMutableRawPointer(mutating: yPlane),
                height: vImagePixelCount(yHeight),
                width: vImagePixelCount(width),
                rowBytes: yStride
            )
            var uvBuf = vImage_Buffer(
                data: UnsafeMutableRawPointer(mutating: uvPlane),
                height: vImagePixelCount(yHeight / 2),
                width: vImagePixelCount(width / 2),
                rowBytes: uvStride
            )
            var destBuf = vImage_Buffer(
                data: destData,
                height: vImagePixelCount(yHeight),
                width: vImagePixelCount(width),
                rowBytes: bgraStride
            )

            // Create/cache conversion info for YUV 420 → BGRA
            let isFullRange = (pixelFormat == kCVPixelFormatType_420YpCbCr8BiPlanarFullRange)
            if conversionInfo == nil || conversionInfoFullRange != isFullRange {
                var info = vImage_YpCbCrToARGB()
                var pixelRange = isFullRange
                    ? vImage_YpCbCrPixelRange(Yp_bias: 0, CbCr_bias: 128, YpRangeMax: 255, CbCrRangeMax: 255, YpMax: 255, YpMin: 0, CbCrMax: 255, CbCrMin: 0)
                    : vImage_YpCbCrPixelRange(Yp_bias: 16, CbCr_bias: 128, YpRangeMax: 235, CbCrRangeMax: 240, YpMax: 235, YpMin: 16, CbCrMax: 240, CbCrMin: 16)

                let infoErr = vImageConvert_YpCbCrToARGB_GenerateConversion(
                    kvImage_YpCbCrToARGBMatrix_ITU_R_709_2,
                    &pixelRange,
                    &info,
                    kvImage420Yp8_CbCr8,
                    kvImageARGB8888,
                    vImage_Flags(kvImageNoFlags)
                )
                guard infoErr == kvImageNoError else { return }
                conversionInfo = info
                conversionInfoFullRange = isFullRange
            }

            // Perform the conversion: YUV 420 bi-planar → BGRA8888
            // The permute map reorders ARGB → BGRA: channel indices [3,2,1,0]
            let permuteMap: [UInt8] = [3, 2, 1, 0]
            var info = conversionInfo!
            let convErr = vImageConvert_420Yp8_CbCr8ToARGB8888(
                &yBuf,
                &uvBuf,
                &destBuf,
                &info,
                permuteMap,  // reorder ARGB → BGRA
                255,         // alpha fill value
                vImage_Flags(kvImageNoFlags)
            )
            guard convErr == kvImageNoError else { return }

            writer.commitFrame(
                dataLen: bgraLen,
                width: UInt32(width),
                height: UInt32(yHeight),
                stride: UInt32(bgraStride),
                rotation: rotation
            )
        } else {
            // Unknown non-planar, non-BGRA format — skip
            return
        }
    }
}
