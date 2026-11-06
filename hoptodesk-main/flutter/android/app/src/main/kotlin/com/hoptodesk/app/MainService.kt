package com.hoptodesk.app

import ffi.FFI

/**
 * Capture screen,get video and audio,send to rust.
 * Dispatch notifications
 *
 * Inspired by [droidVNC-NG] https://github.com/bk138/droidVNC-NG
 */

import android.annotation.SuppressLint
import android.app.*
import android.app.PendingIntent.FLAG_IMMUTABLE
import android.app.PendingIntent.FLAG_UPDATE_CURRENT
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.content.res.Configuration
import android.content.res.Configuration.ORIENTATION_LANDSCAPE
import android.graphics.Color
import android.graphics.PixelFormat
import android.hardware.display.DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR
import android.hardware.display.VirtualDisplay
import android.media.*
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionManager
import android.net.wifi.WifiManager
import android.os.*
import android.util.DisplayMetrics
import android.util.Log
import android.view.Surface
import android.view.Surface.FRAME_RATE_COMPATIBILITY_DEFAULT
import android.view.WindowManager
import androidx.annotation.Keep
import androidx.annotation.RequiresApi
import androidx.core.app.ServiceCompat
import androidx.core.app.NotificationCompat
import androidx.core.content.ContextCompat
import io.flutter.embedding.android.FlutterActivity
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean
import org.json.JSONException
import org.json.JSONArray
import org.json.JSONObject
import kotlin.math.max
import kotlin.math.min

const val DEFAULT_NOTIFY_TEXT = "Service is running"
const val DEFAULT_NOTIFY_ID = 1
const val NOTIFY_ID_OFFSET = 100

const val MIME_TYPE = MediaFormat.MIMETYPE_VIDEO_VP9

// video const

const val MAX_SCREEN_SIZE = 1200

const val VIDEO_KEY_BIT_RATE = 1024_000
const val VIDEO_KEY_FRAME_RATE = 30

class MainService : Service() {

    @Keep
    @RequiresApi(Build.VERSION_CODES.N)
    fun rustPointerInput(kind: Int, mask: Int, x: Int, y: Int) {
        // turn on screen with LEFT_DOWN when screen off
        if (!powerManager.isInteractive && (kind == 0 || mask == LEFT_DOWN)) {
            if (wakeLock.isHeld) {
                Log.d(logTag, "Turn on Screen, WakeLock release")
                wakeLock.release()
            }
            Log.d(logTag,"Turn on Screen")
            wakeLock.acquire(5000)
        } else {
            if (ZebraInput.onPointerInput(mask, x, y)) {
                return
            }
            when (kind) {
                0 -> { // touch
                    InputService.ctx?.onTouchInput(mask, x, y)
                }
                1 -> { // mouse
                    InputService.ctx?.onMouseInput(mask, x, y)
                }
                else -> {
                }
            }
        }
    }

    @Keep
    @RequiresApi(Build.VERSION_CODES.N)
    fun rustKeyEventInput(input: ByteArray) {
        if (ZebraInput.onKeyEvent(input)) {
            return
        }
        InputService.ctx?.onKeyEvent(input)
    }
        
    @Keep
    fun rustGetByName(name: String): String {
        return when (name) {
            "screen_size" -> {
                JSONObject().apply {
                    put("width",SCREEN_INFO.width)
                    put("height",SCREEN_INFO.height)
                    put("scale",SCREEN_INFO.scale)
                }.toString()
            }
            "is_start" -> {
                isStart.toString()
            }
            "device_health", "list_apps" -> {
                AgentCommands.getByName(applicationContext, name)
            }
            else -> ""
        }
    }

    @Keep
    fun rustAgentCall(name: String, argsJson: String): String {
        return AgentScreen.call(applicationContext, name, argsJson)
    }

	private fun invokeFlutterMethod(method: String, arguments: Any?) {
		Handler(Looper.getMainLooper()).post {
			MainActivity.flutterMethodChannel?.invokeMethod(method, arguments)
		}
	}

	@Keep
	fun rustSetByName(name: String, arg1: String, arg2: String) {
		when (name) {
			"add_connection" -> {
				try {
					val jsonObject = JSONObject(arg1)
					val id = jsonObject["id"] as Int
					val username = jsonObject["name"] as String
					val peerId = jsonObject["peer_id"] as String
					val authorized = jsonObject["authorized"] as Boolean
					val isFileTransfer = jsonObject["is_file_transfer"] as Boolean
					val type = if (jsonObject.optBoolean("is_invite")) {
						translate("Invite")
					} else if (isFileTransfer) {
						translate("File Connection")
					} else {
						translate("Screen Connection")
					}
					if (authorized) {
						if (!isFileTransfer && !isStart) {
							startCapture()
						}
						onClientAuthorizedNotification(id, type, username, peerId)
					} else {
						loginRequestNotification(id, type, username, peerId)
					}
				} catch (e: JSONException) {
					e.printStackTrace()
				}
			}
			"update_voice_call_state" -> {
				try {
					val jsonObject = JSONObject(arg1)
					val id = jsonObject["id"] as Int
					val username = jsonObject["name"] as String
					val peerId = jsonObject["peer_id"] as String
					val inVoiceCall = jsonObject["in_voice_call"] as Boolean
					val incomingVoiceCall = jsonObject["incoming_voice_call"] as Boolean
					if (!inVoiceCall) {
						if (incomingVoiceCall) {
							voiceCallRequestNotification(id, "Voice Call Request", username, peerId)
						} else {
							if (!audioRecordHandle.switchOutVoiceCall(mediaProjection)) {
								Log.e(logTag, "switchOutVoiceCall fail")
								invokeFlutterMethod("msgbox", mapOf(
									"type" to "custom-nook-nocancel-hasclose-error",
									"title" to "Voice call",
									"text" to "Failed to switch out voice call."
								))
							}
						}
					} else {
						if (!audioRecordHandle.switchToVoiceCall(mediaProjection)) {
							Log.e(logTag, "switchToVoiceCall fail")
							invokeFlutterMethod("msgbox", mapOf(
								"type" to "custom-nook-nocancel-hasclose-error",
								"title" to "Voice call",
								"text" to "Failed to switch to voice call."
							))
						}
					}
				} catch (e: JSONException) {
					e.printStackTrace()
				}
			}
			"stop_capture" -> {
				Log.d(logTag, "from rust:stop_capture")
				stopCapture()
			}
			"half_scale" -> {
				val halfScale = arg1.toBoolean()
				if (isHalfScale != halfScale) {
					isHalfScale = halfScale
					updateScreenInfo(resources.configuration.orientation)
				}
			}
			"launch_app", "wake_screen", "reboot_device" -> {
				AgentCommands.setByName(applicationContext, name, arg1)
			}
			else -> {
			}
		}
	}

    private var serviceLooper: Looper? = null
    private var serviceHandler: Handler? = null

    private val powerManager: PowerManager by lazy { applicationContext.getSystemService(Context.POWER_SERVICE) as PowerManager }
    private val wakeLock: PowerManager.WakeLock by lazy { powerManager.newWakeLock(PowerManager.ACQUIRE_CAUSES_WAKEUP or PowerManager.SCREEN_BRIGHT_WAKE_LOCK, "hoptodesk:wakelock")}
    private val sessionWakeLock: PowerManager.WakeLock by lazy { powerManager.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "hoptodesk:session") }
    private val wifiManager: WifiManager by lazy { applicationContext.getSystemService(Context.WIFI_SERVICE) as WifiManager }
    private val sessionWifiLock: WifiManager.WifiLock by lazy {
        val mode = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) WifiManager.WIFI_MODE_FULL_LOW_LATENCY else WifiManager.WIFI_MODE_FULL_HIGH_PERF
        wifiManager.createWifiLock(mode, "hoptodesk:session")
    }

    private fun acquireSessionLocks() {
        if (!sessionWakeLock.isHeld) {
            sessionWakeLock.acquire()
        }
        if (!sessionWifiLock.isHeld) {
            sessionWifiLock.acquire()
        }
    }

    private fun releaseSessionLocks() {
        if (sessionWakeLock.isHeld) {
            sessionWakeLock.release()
        }
        if (sessionWifiLock.isHeld) {
            sessionWifiLock.release()
        }
    }

    companion object {
        val mediaProjectionRequestInFlight = AtomicBoolean(false)
        private var _isReady = false // media permission ready status
        @Volatile
        private var _isStart = false // screen capture start status
        private var _isAudioStart = false // audio capture start status
        val isReady: Boolean
            get() = _isReady
        val isStart: Boolean
            get() = _isStart
        val isAudioStart: Boolean
            get() = _isAudioStart
    }

    private val logTag = "LOG_SERVICE"
    private val useVP9 = false
    private val binder = LocalBinder()

    private var reuseVirtualDisplay = Build.VERSION.SDK_INT > 33

    // video
    private var mediaProjection: MediaProjection? = null
    private var mediaProjectionCallback: MediaProjection.Callback? = null
    private var captureRestartPending = false
    private var mediaProjectionForegroundService = false
    private var surface: Surface? = null
    private val sendVP9Thread = Executors.newSingleThreadExecutor()
    private var videoEncoder: MediaCodec? = null
    private var imageReader: ImageReader? = null
    private var virtualDisplay: VirtualDisplay? = null

    // audio
    private val audioRecordHandle = AudioRecordHandle(this, { isStart }, { isAudioStart })

    // notification
    private lateinit var notificationManager: NotificationManager
    private lateinit var notificationChannel: String
    private lateinit var notificationBuilder: NotificationCompat.Builder

    override fun onCreate() {
        super.onCreate()
        Log.d(logTag,"MainService onCreate, sdk int:${Build.VERSION.SDK_INT} reuseVirtualDisplay:$reuseVirtualDisplay")
        FFI.init(this)
        HandlerThread("Service", Process.THREAD_PRIORITY_BACKGROUND).apply {
            start()
            serviceLooper = looper
            serviceHandler = Handler(looper)
        }
        updateScreenInfo(resources.configuration.orientation)
        initNotification()

        ZebraInput.connect(applicationContext)

        clearPasswordIfRestoredToNewDevice(applicationContext)
        // keep the config dir same with flutter
        val prefs = applicationContext.getSharedPreferences(KEY_SHARED_PREFERENCES, FlutterActivity.MODE_PRIVATE)
        val configPath = prefs.getString(KEY_APP_DIR_CONFIG_PATH, "") ?: ""
        FFI.startServer(configPath, "")
        
        createForegroundNotification()
    }

    override fun onDestroy() {
        checkMediaPermission()
        stopService(Intent(this, FloatingWindowService::class.java))
        super.onDestroy()
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        FFI.closeAllSessions()
        super.onTaskRemoved(rootIntent)
    }

    private var isHalfScale: Boolean? = null;
    private fun updateScreenInfo(orientation: Int) {
        var w: Int
        var h: Int
        var dpi: Int
        val windowManager = getSystemService(Context.WINDOW_SERVICE) as WindowManager

        @Suppress("DEPRECATION")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val m = windowManager.maximumWindowMetrics
            w = m.bounds.width()
            h = m.bounds.height()
            dpi = resources.configuration.densityDpi
        } else {
            val dm = DisplayMetrics()
            windowManager.defaultDisplay.getRealMetrics(dm)
            w = dm.widthPixels
            h = dm.heightPixels
            dpi = dm.densityDpi
        }

        val max = max(w,h)
        val min = min(w,h)
        if (orientation == ORIENTATION_LANDSCAPE) {
            w = max
            h = min
        } else {
            w = min
            h = max
        }
        Log.d(logTag,"updateScreenInfo:w:$w,h:$h")
        var scale = 1
        if (w != 0 && h != 0) {
            if (isHalfScale == true && (w > MAX_SCREEN_SIZE || h > MAX_SCREEN_SIZE)) {
                scale = 2
                w /= scale
                h /= scale
                dpi /= scale
            }
            if (SCREEN_INFO.width != w) {
                SCREEN_INFO.width = w
                SCREEN_INFO.height = h
                SCREEN_INFO.scale = scale
                SCREEN_INFO.dpi = dpi
                if (isStart) {
                    stopCapture()
                    FFI.refreshScreen()
                    startCapture()
                } else {
                    FFI.refreshScreen()
                }
            }

        }
    }

    override fun onBind(intent: Intent): IBinder {
        Log.d(logTag, "service onBind")
        return binder
    }

    inner class LocalBinder : Binder() {
        init {
            Log.d(logTag, "LocalBinder init")
        }

        fun getService(): MainService = this@MainService
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        Log.d("whichService", "this service: ${Thread.currentThread()}")
        super.onStartCommand(intent, flags, startId)
        if (intent?.action == ACT_INIT_MEDIA_PROJECTION_AND_SERVICE) {
            createForegroundNotification()

            if (intent.getBooleanExtra(EXT_INIT_FROM_BOOT, false)) {
                FFI.startService()
            }
            Log.d(logTag, "service starting: ${startId}:${Thread.currentThread()}")
            val mediaProjectionManager =
                getSystemService(MEDIA_PROJECTION_SERVICE) as MediaProjectionManager

            intent.getParcelableExtra<Intent>(EXT_MEDIA_PROJECTION_RES_INTENT)?.let {
                replaceMediaProjection(mediaProjectionManager, it)
            } ?: let {
                Log.d(logTag, "getParcelableExtra intent null, invoke requestMediaProjection")
                requestMediaProjection()
            }
        }
        return START_NOT_STICKY // don't use sticky (auto restart), the new service (from auto restart) will lose control
    }

    override fun onConfigurationChanged(newConfig: Configuration) {
        super.onConfigurationChanged(newConfig)
        updateScreenInfo(newConfig.orientation)
    }

    private fun requestMediaProjection() {
        if (!mediaProjectionRequestInFlight.compareAndSet(false, true)) {
            Log.d(logTag, "requestMediaProjection already in flight, ignoring")
            return
        }
        try {
            val intent = Intent(this, PermissionRequestTransparentActivity::class.java).apply {
                action = ACT_REQUEST_MEDIA_PROJECTION
                flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
            }
            startActivity(intent)
        } catch (e: Exception) {
            mediaProjectionRequestInFlight.set(false)
            Log.e(logTag, "requestMediaProjection failed: $e")
        }
    }

    @SuppressLint("WrongConstant")
    private fun createSurface(): Surface? {
        return if (useVP9) {
            // TODO
            null
        } else {
            Log.d(logTag, "ImageReader.newInstance:INFO:$SCREEN_INFO")
            imageReader =
                ImageReader.newInstance(
                    SCREEN_INFO.width,
                    SCREEN_INFO.height,
                    PixelFormat.RGBA_8888,
                    4
                ).apply {
                    setOnImageAvailableListener({ imageReader: ImageReader ->
                        synchronized(this@MainService) {
                            try {
                                // If not call acquireLatestImage, listener will not be called again
                                imageReader.acquireLatestImage().use { image ->
                                    if (image == null || !isStart) return@setOnImageAvailableListener
                                    val planes = image.planes
                                    val buffer = planes[0].buffer
                                    buffer.rewind()
                                    FFI.onVideoFrameUpdate(buffer)
                                }
                            } catch (ignored: java.lang.Exception) {
                            }
                        }
                    }, serviceHandler)
                }
            Log.d(logTag, "ImageReader.setOnImageAvailableListener done")
            imageReader?.surface
        }
    }

    private fun releaseMediaProjection() {
        mediaProjectionCallback?.let { mediaProjection?.unregisterCallback(it) }
        mediaProjection?.stop()
        mediaProjection = null
        mediaProjectionCallback = null
    }

    @Synchronized
    private fun handleMediaProjectionStopped(stoppedProjection: MediaProjection) {
        if (mediaProjection !== stoppedProjection) {
            return
        }
        Log.d(logTag, "MediaProjection stopped")
        setMediaProjectionForegroundService(false)
        stopCapture()
        virtualDisplay?.release()
        virtualDisplay = null
        mediaProjection = null
        mediaProjectionCallback = null
        _isReady = false
        checkMediaPermission()
    }

    @Synchronized
    private fun replaceMediaProjection(mediaProjectionManager: MediaProjectionManager, resultIntent: Intent) {
        val restartCapture = isStart || captureRestartPending
        if (!setMediaProjectionForegroundService(true)) {
            return
        }
        val projection = mediaProjectionManager.getMediaProjection(Activity.RESULT_OK, resultIntent)
        if (projection == null) {
            Log.w(logTag, "getMediaProjection returned null")
            if (mediaProjection == null) {
                setMediaProjectionForegroundService(false)
            }
            return
        }
        if (isStart) {
            stopCapture()
        }
        virtualDisplay?.release()
        virtualDisplay = null
        releaseMediaProjection()
        val callback = object : MediaProjection.Callback() {
            override fun onStop() {
                handleMediaProjectionStopped(projection)
            }
        }
        projection.registerCallback(callback, Handler(Looper.getMainLooper()))
        mediaProjection = projection
        mediaProjectionCallback = callback
        _isReady = true
        checkMediaPermission()
        if (restartCapture) {
            captureRestartPending = false
            startCapture()
        }
    }

    fun onVoiceCallStarted(): Boolean {
        return audioRecordHandle.onVoiceCallStarted(mediaProjection)
    }

    fun onVoiceCallClosed(): Boolean {
        return audioRecordHandle.onVoiceCallClosed(mediaProjection)
    }

    @Synchronized
    fun startCapture(): Boolean {
        if (isStart) {
            return true
        }
        if (mediaProjection == null) {
            Log.w(logTag, "startCapture fail,mediaProjection is null")
            return false
        }
        acquireSessionLocks()
        updateScreenInfo(resources.configuration.orientation)
        Log.d(logTag, "Start Capture")
        surface = createSurface()

        val videoStarted = if (useVP9) {
            startVP9VideoRecorder(mediaProjection!!)
        } else {
            startRawVideoRecorder(mediaProjection!!)
        }
        if (!videoStarted) {
            releaseFailedVideoCapture()
            return false
        }

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            if (!audioRecordHandle.createAudioRecorder(false, mediaProjection)) {
                Log.d(logTag, "createAudioRecorder fail")
            } else {
                Log.d(logTag, "audio recorder start")
                audioRecordHandle.startAudioRecorder()
            }
        }
        checkMediaPermission()
        _isStart = true
        FFI.setFrameRawEnable("video",true)
        MainActivity.rdClipboardManager?.setCaptureStarted(_isStart)
        return true
    }

    private fun releaseFailedVideoCapture() {
        imageReader?.setOnImageAvailableListener(null, null)
        imageReader?.close()
        imageReader = null
        videoEncoder?.let {
            it.signalEndOfInputStream()
            it.stop()
            it.release()
        }
        videoEncoder = null
        surface?.release()
        surface = null
    }

    @Synchronized
    fun stopCapture() {
        Log.d(logTag, "Stop Capture")
        captureRestartPending = false
        _isStart = false
        releaseSessionLocks()
        FFI.setFrameRawEnable("video",false)
        MainActivity.rdClipboardManager?.setCaptureStarted(_isStart)
        // release video
        if (reuseVirtualDisplay) {
            // The virtual display video projection can be paused by calling `setSurface(null)`.
            // https://developer.android.com/reference/android/hardware/display/VirtualDisplay.Callback
            // https://learn.microsoft.com/en-us/dotnet/api/android.hardware.display.virtualdisplay.callback.onpaused?view=net-android-34.0
            virtualDisplay?.setSurface(null)
        } else {
            virtualDisplay?.release()
        }
        // Remove listener before close to prevent callbacks from accessing freed image buffers
        imageReader?.setOnImageAvailableListener(null, null)
        // suface needs to be release after `imageReader.close()` to imageReader access released surface
        // https://github.com/rustdesk/rustdesk/issues/4118#issuecomment-1515666629
        imageReader?.close()
        imageReader = null
        videoEncoder?.let {
            it.signalEndOfInputStream()
            it.stop()
            it.release()
        }
        if (!reuseVirtualDisplay) {
            virtualDisplay = null
        }
        videoEncoder = null
        // suface needs to be release after `imageReader.close()` to imageReader access released surface
        // https://github.com/rustdesk/rustdesk/issues/4118#issuecomment-1515666629
        surface?.release()

        // release audio
        _isAudioStart = false
        audioRecordHandle.tryReleaseAudio()
    }

    fun destroy() {
        Log.d(logTag, "destroy service")
        _isReady = false
        _isAudioStart = false

        stopCapture()
        releaseSessionLocks()

        if (reuseVirtualDisplay) {
            virtualDisplay?.release()
            virtualDisplay = null
        }

        releaseMediaProjection()
        mediaProjectionForegroundService = false
        checkMediaPermission()
        stopForeground(true)
        stopService(Intent(this, FloatingWindowService::class.java))
        stopSelf()
    }

    fun checkMediaPermission(): Boolean {
        Handler(Looper.getMainLooper()).post {
            MainActivity.flutterMethodChannel?.invokeMethod(
                "on_state_changed",
                mapOf("name" to "media", "value" to isReady.toString())
            )
        }
        Handler(Looper.getMainLooper()).post {
            MainActivity.flutterMethodChannel?.invokeMethod(
                "on_state_changed",
                mapOf("name" to "input", "value" to isRemoteInputAvailable.toString())
            )
        }
        return isReady
    }

    private fun startRawVideoRecorder(mp: MediaProjection): Boolean {
        Log.d(logTag, "startRawVideoRecorder,screen info:$SCREEN_INFO")
        if (surface == null) {
            Log.d(logTag, "startRawVideoRecorder failed,surface is null")
            return false
        }
        return createOrSetVirtualDisplay(mp, surface!!)
    }

    private fun startVP9VideoRecorder(mp: MediaProjection): Boolean {
        createMediaCodec()
        return videoEncoder?.let {
            surface = it.createInputSurface()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                surface!!.setFrameRate(1F, FRAME_RATE_COMPATIBILITY_DEFAULT)
            }
            it.setCallback(cb)
            it.start()
            createOrSetVirtualDisplay(mp, surface!!)
        } ?: false
    }

    // https://github.com/bk138/droidVNC-NG/blob/b79af62db5a1c08ed94e6a91464859ffed6f4e97/app/src/main/java/net/christianbeier/droidvnc_ng/MediaProjectionService.java#L250
    // Reuse virtualDisplay if it exists, to avoid media projection confirmation dialog every connection.
    private fun createOrSetVirtualDisplay(mp: MediaProjection, s: Surface): Boolean {
        try {
            virtualDisplay?.let {
                it.resize(SCREEN_INFO.width, SCREEN_INFO.height, SCREEN_INFO.dpi)
                it.setSurface(s)
            } ?: let {
                virtualDisplay = mp.createVirtualDisplay(
                    "RustDeskVD",
                    SCREEN_INFO.width, SCREEN_INFO.height, SCREEN_INFO.dpi, VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR,
                    s, null, null
                )
            }
        } catch (e: SecurityException) {
            Log.w(logTag, "createOrSetVirtualDisplay: got SecurityException, re-requesting confirmation");
            return handleVirtualDisplayFailure()
        }
        if (virtualDisplay == null) {
            Log.e(logTag, "createOrSetVirtualDisplay failed")
            return handleVirtualDisplayFailure()
        }
        return true
    }

    private fun handleVirtualDisplayFailure(): Boolean {
        captureRestartPending = true
        virtualDisplay?.release()
        virtualDisplay = null
        releaseMediaProjection()
        setMediaProjectionForegroundService(false)
        _isReady = false
        checkMediaPermission()
        requestMediaProjection()
        return false
    }

    private val cb: MediaCodec.Callback = object : MediaCodec.Callback() {
        override fun onInputBufferAvailable(codec: MediaCodec, index: Int) {}
        override fun onOutputFormatChanged(codec: MediaCodec, format: MediaFormat) {}

        override fun onOutputBufferAvailable(
            codec: MediaCodec,
            index: Int,
            info: MediaCodec.BufferInfo
        ) {
            codec.getOutputBuffer(index)?.let { buf ->
                sendVP9Thread.execute {
                    val byteArray = ByteArray(buf.limit())
                    buf.get(byteArray)
                    // sendVp9(byteArray)
                    codec.releaseOutputBuffer(index, false)
                }
            }
        }

        override fun onError(codec: MediaCodec, e: MediaCodec.CodecException) {
            Log.e(logTag, "MediaCodec.Callback error:$e")
        }
    }

    private fun createMediaCodec() {
        Log.d(logTag, "MediaFormat.MIMETYPE_VIDEO_VP9 :$MIME_TYPE")
        videoEncoder = MediaCodec.createEncoderByType(MIME_TYPE)
        val mFormat =
            MediaFormat.createVideoFormat(MIME_TYPE, SCREEN_INFO.width, SCREEN_INFO.height)
        mFormat.setInteger(MediaFormat.KEY_BIT_RATE, VIDEO_KEY_BIT_RATE)
        mFormat.setInteger(MediaFormat.KEY_FRAME_RATE, VIDEO_KEY_FRAME_RATE)
        mFormat.setInteger(
            MediaFormat.KEY_COLOR_FORMAT,
            MediaCodecInfo.CodecCapabilities.COLOR_FormatYUV420Flexible
        )
        mFormat.setInteger(MediaFormat.KEY_I_FRAME_INTERVAL, 5)
        try {
            videoEncoder!!.configure(mFormat, null, null, MediaCodec.CONFIGURE_FLAG_ENCODE)
        } catch (e: Exception) {
            Log.e(logTag, "mEncoder.configure fail!")
        }
    }

    private fun initNotification() {
        notificationManager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        notificationChannel = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channelId = "HopToDesk"
            val channelName = "${getString(R.string.app_name)} Service"
            val channel = NotificationChannel(
                channelId,
                channelName, NotificationManager.IMPORTANCE_HIGH
            ).apply {
                description = "$channelName Channel"
            }
            channel.lightColor = Color.BLUE
            channel.lockscreenVisibility = Notification.VISIBILITY_PRIVATE
            notificationManager.createNotificationChannel(channel)
            channelId
        } else {
            ""
        }
        notificationBuilder = NotificationCompat.Builder(this, notificationChannel)
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun foregroundServiceType(): Int {
        var serviceType = ServiceInfo.FOREGROUND_SERVICE_TYPE_NONE
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
            serviceType = serviceType or ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
        }
        if (mediaProjectionForegroundService) {
            serviceType = serviceType or ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION
        }
        return serviceType
    }

    private fun setMediaProjectionForegroundService(enabled: Boolean): Boolean {
        if (mediaProjectionForegroundService == enabled) {
            return true
        }
        mediaProjectionForegroundService = enabled
        if (createForegroundNotification()) {
            return true
        }
        mediaProjectionForegroundService = !enabled
        return false
    }

	@SuppressLint("UnspecifiedImmutableFlag")
	private fun createForegroundNotification(): Boolean {
		val intent = Intent(this, MainActivity::class.java).apply {
			flags = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED
			action = Intent.ACTION_MAIN
			addCategory(Intent.CATEGORY_LAUNCHER)
		}
		val pendingIntent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
			PendingIntent.getActivity(this, 0, intent, FLAG_UPDATE_CURRENT or FLAG_IMMUTABLE)
		} else {
			PendingIntent.getActivity(this, 0, intent, FLAG_UPDATE_CURRENT)
		}
		val notification = notificationBuilder
			.setOngoing(true)
			.setSmallIcon(R.mipmap.ic_stat_logo)
			.setDefaults(Notification.DEFAULT_ALL)
			.setAutoCancel(true)
			.setPriority(NotificationCompat.PRIORITY_DEFAULT)
			.setContentTitle(getString(R.string.app_name))
			.setContentText(translate(DEFAULT_NOTIFY_TEXT))
			.setOnlyAlertOnce(true)
			.setContentIntent(pendingIntent)
			.setColor(ContextCompat.getColor(this, R.color.primary))
			.setWhen(System.currentTimeMillis())
			.build()

		Log.d(logTag, "Attempting to start foreground service")
		return try {
			if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
				startForeground(DEFAULT_NOTIFY_ID, notification, foregroundServiceType())
			} else {
				startForeground(DEFAULT_NOTIFY_ID, notification)
			}
			true
		} catch (e: Exception) {
			Log.e(logTag, "Failed to start foreground service: $e")
			false
		}
	}  

	private fun loginRequestNotification(
	    clientID: Int,
	    type: String,
	    username: String,
	    peerId: String
	) {
	    // Create a proper intent for the notification
	    val intent = Intent(this, MainActivity::class.java).apply {
	        flags = Intent.FLAG_ACTIVITY_NEW_TASK or 
	                Intent.FLAG_ACTIVITY_SINGLE_TOP or  // Add this
	                Intent.FLAG_ACTIVITY_CLEAR_TOP
	        action = Intent.ACTION_MAIN
	        addCategory(Intent.CATEGORY_LAUNCHER)
	    }
	    
	    val pendingIntent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
	        PendingIntent.getActivity(this, clientID, intent, FLAG_UPDATE_CURRENT or FLAG_IMMUTABLE)
	    } else {
	        PendingIntent.getActivity(this, clientID, intent, FLAG_UPDATE_CURRENT)
	    }
	    
	    val notification = notificationBuilder
	        .setOngoing(false)
	        .setSmallIcon(R.mipmap.ic_stat_logo)
	        .setPriority(NotificationCompat.PRIORITY_MAX)
	        .setContentTitle(translate("Do you accept?"))
	        .setContentText("$type:$username-$peerId")
	        .setContentIntent(pendingIntent)
	        .build()
	    
	    notificationManager.notify(getClientNotifyID(clientID), notification)
	}	

    private fun onClientAuthorizedNotification(
        clientID: Int,
        type: String,
        username: String,
        peerId: String
    ) {
        cancelNotification(clientID)
        val notification = notificationBuilder
            .setOngoing(false)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setContentTitle("$type ${translate("Established")}")
            .setContentText("$username - $peerId")
            .build()
        notificationManager.notify(getClientNotifyID(clientID), notification)
    }

    private fun voiceCallRequestNotification(
        clientID: Int,
        type: String,
        username: String,
        peerId: String
    ) {
        val notification = notificationBuilder
            .setOngoing(false)
            .setPriority(NotificationCompat.PRIORITY_MAX)
            .setContentTitle(translate("Do you accept?"))
            .setContentText("$type:$username-$peerId")
            .build()
        notificationManager.notify(getClientNotifyID(clientID), notification)
    }

    private fun getClientNotifyID(clientID: Int): Int {
        return clientID + NOTIFY_ID_OFFSET
    }

    fun cancelNotification(clientID: Int) {
        notificationManager.cancel(getClientNotifyID(clientID))
    }

    @SuppressLint("UnspecifiedImmutableFlag")
    private fun genLoginRequestPendingIntent(res: Boolean): PendingIntent {
        val intent = Intent(this, MainService::class.java).apply {
            action = ACT_LOGIN_REQ_NOTIFY
            putExtra(EXT_LOGIN_REQ_NOTIFY, res)
        }
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.M) {
            PendingIntent.getService(this, 111, intent, FLAG_IMMUTABLE)
        } else {
            PendingIntent.getService(this, 111, intent, FLAG_UPDATE_CURRENT)
        }
    }

    private fun setTextNotification(_title: String?, _text: String?) {
        val title = _title ?: getString(R.string.app_name)
        val text = _text ?: translate(DEFAULT_NOTIFY_TEXT)
        val notification = notificationBuilder
            .clearActions()
            .setStyle(null)
            .setContentTitle(title)
            .setContentText(text)
            .build()
        notificationManager.notify(DEFAULT_NOTIFY_ID, notification)
    }
}
