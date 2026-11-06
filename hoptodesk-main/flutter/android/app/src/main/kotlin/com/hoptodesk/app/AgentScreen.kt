package com.hoptodesk.app

import android.accessibilityservice.AccessibilityService
import android.accessibilityservice.GestureDescription
import android.graphics.Bitmap
import android.content.Context
import android.graphics.Path
import android.graphics.Point
import android.graphics.Rect
import android.os.Build
import android.os.Bundle
import android.util.Base64
import android.util.Log
import android.view.Display
import android.view.WindowManager
import android.view.accessibility.AccessibilityNodeInfo
import androidx.annotation.RequiresApi
import java.io.ByteArrayOutputStream
import java.util.concurrent.CountDownLatch
import java.util.concurrent.Executors
import java.util.concurrent.TimeUnit
import kotlin.math.max
import org.json.JSONArray
import org.json.JSONObject

object AgentScreen {
    private const val logTag = "AgentScreen"
    private const val MAX_IMAGE_EDGE = 1280
    private const val JPEG_QUALITY = 70
    private const val MAX_NODES = 500
    private const val TAP_MS = 60L

    private val executor = Executors.newSingleThreadExecutor()

    fun call(ctx: Context, name: String, argsJson: String): String {
        if (AgentCommands.isPlayBuild(ctx)) return error("Not available in the Google Play version of HopToDesk.")
        val service = InputService.ctx
            ?: return error("The HopToDesk accessibility service is off. Turn it on in Android Settings > Accessibility.")
        return try {
            val args = if (argsJson.isEmpty()) JSONObject() else JSONObject(argsJson)
            when (name) {
                "screenshot" -> screenshot(service)
                "read_screen" -> readScreen(service)
                "foreground_app" -> foregroundApp(service)
                "tap" -> tap(service, args)
                "swipe" -> swipe(service, args)
                "scroll" -> scroll(service, args)
                "screen_size" -> screenSize(service)
                "type_text" -> typeText(service, args.optString("text"))
                "global_action" -> {
                    service.performGlobalActionByName(args.optString("action", "home"))
                    ok().toString()
                }
                else -> error("Unknown command: $name")
            }
        } catch (e: Exception) {
            Log.e(logTag, "$name error: $e")
            error("$name failed: ${e.message}")
        }
    }

    private fun error(message: String): String = JSONObject().put("error", message).toString()

    private fun ok(): JSONObject = JSONObject().put("ok", true)

    private fun screenshot(service: InputService): String {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            return error("screenshot needs Android 11 or newer; this device runs Android ${Build.VERSION.RELEASE}. Use read_screen instead.")
        }
        val result = arrayOfNulls<String>(1)
        val latch = CountDownLatch(1)
        service.takeScreenshot(Display.DEFAULT_DISPLAY, executor, object : AccessibilityService.TakeScreenshotCallback {
            override fun onSuccess(shot: AccessibilityService.ScreenshotResult) {
                try {
                    result[0] = encode(shot)
                } catch (e: Exception) {
                    result[0] = error("screenshot encode failed: ${e.message}")
                } finally {
                    latch.countDown()
                }
            }

            override fun onFailure(errorCode: Int) {
                result[0] = error(
                    when (errorCode) {
                        AccessibilityService.ERROR_TAKE_SCREENSHOT_INTERVAL_TIME_SHORT -> "Android allows one screenshot per second. Wait and retry."
                        AccessibilityService.ERROR_TAKE_SCREENSHOT_NO_ACCESSIBILITY_ACCESS -> "Turn the HopToDesk accessibility service off and on again to allow screenshots."
                        else -> "screenshot failed with Android error $errorCode"
                    }
                )
                latch.countDown()
            }
        })
        if (!latch.await(8, TimeUnit.SECONDS)) {
            return error("screenshot timed out")
        }
        return result[0] ?: error("screenshot returned nothing")
    }

    @RequiresApi(Build.VERSION_CODES.R)
    private fun encode(shot: AccessibilityService.ScreenshotResult): String {
        val buffer = shot.hardwareBuffer
        val hardware = Bitmap.wrapHardwareBuffer(buffer, shot.colorSpace)
        buffer.close()
        if (hardware == null) {
            return error("screenshot buffer was empty")
        }
        val full = hardware.copy(Bitmap.Config.ARGB_8888, false)
        hardware.recycle()
        val factor = max(1f, max(full.width, full.height).toFloat() / MAX_IMAGE_EDGE)
        val image = if (factor > 1f) {
            Bitmap.createScaledBitmap(full, (full.width / factor).toInt(), (full.height / factor).toInt(), true)
        } else {
            full
        }
        val bytes = ByteArrayOutputStream()
        image.compress(Bitmap.CompressFormat.JPEG, JPEG_QUALITY, bytes)
        val json = JSONObject()
            .put("data", Base64.encodeToString(bytes.toByteArray(), Base64.NO_WRAP))
            .put("image_width", image.width)
            .put("image_height", image.height)
            .put("screen_width", full.width)
            .put("screen_height", full.height)
        if (image !== full) image.recycle()
        full.recycle()
        return json.toString()
    }

    private fun roots(service: InputService): List<AccessibilityNodeInfo> {
        val fromWindows = try {
            service.windows.mapNotNull { it.root }
        } catch (e: Exception) {
            emptyList()
        }
        if (fromWindows.isNotEmpty()) return fromWindows
        return listOfNotNull(service.rootInActiveWindow)
    }

    private fun walk(node: AccessibilityNodeInfo, visit: (AccessibilityNodeInfo) -> Boolean): Boolean {
        if (!visit(node)) return false
        for (i in 0 until node.childCount) {
            val child = node.getChild(i) ?: continue
            if (!walk(child, visit)) return false
        }
        return true
    }

    private fun label(node: AccessibilityNodeInfo): String {
        if (node.isPassword) return ""
        return node.text?.toString()?.takeIf { it.isNotEmpty() } ?: node.contentDescription?.toString() ?: ""
    }

    private fun readScreen(service: InputService): String {
        val nodes = JSONArray()
        val bounds = Rect()
        for (root in roots(service)) {
            walk(root) { node ->
                val text = label(node)
                if (node.isVisibleToUser && (text.isNotEmpty() || node.isClickable || node.isEditable)) {
                    node.getBoundsInScreen(bounds)
                    val o = JSONObject()
                        .put("text", text)
                        .put("class", node.className?.toString()?.substringAfterLast('.') ?: "")
                        .put("bounds", JSONArray(listOf(bounds.left, bounds.top, bounds.right, bounds.bottom)))
                    node.viewIdResourceName?.substringAfter('/')?.takeIf { it.isNotEmpty() }?.let { o.put("id", it) }
                    if (node.isClickable) o.put("clickable", true)
                    if (node.isEditable) o.put("editable", true)
                    if (node.isFocused) o.put("focused", true)
                    if (node.isPassword) o.put("password", true)
                    if (node.isCheckable) o.put("checked", node.isChecked)
                    nodes.put(o)
                }
                nodes.length() < MAX_NODES
            }
        }
        return JSONObject()
            .put("package", service.rootInActiveWindow?.packageName?.toString() ?: "")
            .put("truncated", nodes.length() >= MAX_NODES)
            .put("nodes", nodes)
            .toString()
    }

    private fun foregroundApp(service: InputService): String {
        return JSONObject().put("package", service.rootInActiveWindow?.packageName?.toString() ?: "").toString()
    }

    private fun findByText(service: InputService, wanted: String): Rect? {
        var exact: Rect? = null
        var partial: Rect? = null
        for (root in roots(service)) {
            walk(root) { node ->
                val text = label(node)
                if (node.isVisibleToUser && text.isNotEmpty()) {
                    if (text.equals(wanted, ignoreCase = true)) {
                        exact = Rect().also { node.getBoundsInScreen(it) }
                    } else if (partial == null && text.contains(wanted, ignoreCase = true)) {
                        partial = Rect().also { node.getBoundsInScreen(it) }
                    }
                }
                exact == null
            }
            if (exact != null) break
        }
        return exact ?: partial
    }

    private fun gesture(service: InputService, path: Path, durationMs: Long): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.N) return false
        val stroke = GestureDescription.StrokeDescription(path, 0, durationMs)
        return service.dispatchGesture(GestureDescription.Builder().addStroke(stroke).build(), null, null)
    }

    private fun tap(service: InputService, args: JSONObject): String {
        val wanted = args.optString("text")
        var x = args.optInt("x", -1)
        var y = args.optInt("y", -1)
        if (wanted.isNotEmpty()) {
            val bounds = findByText(service, wanted) ?: return error("No visible element contains \"$wanted\". Call read_screen to see what is on screen.")
            x = bounds.centerX()
            y = bounds.centerY()
        }
        if (x < 0 || y < 0) return error("Give text, or x and y in screen pixels.")
        val path = Path().apply { moveTo(x.toFloat(), y.toFloat()) }
        if (!gesture(service, path, TAP_MS)) return error("Android rejected the tap gesture.")
        return ok().put("x", x).put("y", y).toString()
    }

    private fun swipe(service: InputService, args: JSONObject): String {
        val path = Path().apply {
            moveTo(args.optInt("x1").toFloat(), args.optInt("y1").toFloat())
            lineTo(args.optInt("x2").toFloat(), args.optInt("y2").toFloat())
        }
        val durationMs = args.optLong("duration_ms", 300).coerceIn(50, 5000)
        if (!gesture(service, path, durationMs)) return error("Android rejected the swipe gesture.")
        return ok().toString()
    }

    private fun realSize(service: InputService): Point {
        val size = Point()
        @Suppress("DEPRECATION")
        (service.getSystemService(Context.WINDOW_SERVICE) as WindowManager).defaultDisplay.getRealSize(size)
        return size
    }

    private fun screenSize(service: InputService): String {
        val size = realSize(service)
        return JSONObject().put("width", size.x).put("height", size.y).toString()
    }

    private fun scroll(service: InputService, args: JSONObject): String {
        val size = realSize(service)
        val x = args.optInt("x", size.x / 2).toFloat()
        val y = args.optInt("y", size.y / 2).toFloat()
        val distance = (args.optInt("amount", 3).coerceIn(1, 10) * size.y / 12).toFloat()
        val path = Path().apply {
            moveTo(x, y)
            when (args.optString("direction", "down")) {
                "up" -> lineTo(x, (y + distance).coerceAtMost(size.y - 1f))
                "left" -> lineTo((x + distance).coerceAtMost(size.x - 1f), y)
                "right" -> lineTo((x - distance).coerceAtLeast(0f), y)
                else -> lineTo(x, (y - distance).coerceAtLeast(0f))
            }
        }
        if (!gesture(service, path, 300)) return error("Android rejected the scroll gesture.")
        return ok().toString()
    }

    private fun typeText(service: InputService, text: String): String {
        val target = service.rootInActiveWindow?.findFocus(AccessibilityNodeInfo.FOCUS_INPUT)
            ?: return error("No text field has focus. Tap a text field first.")
        if (!target.isEditable) return error("The focused element is not a text field.")
        val arguments = Bundle().apply {
            putCharSequence(AccessibilityNodeInfo.ACTION_ARGUMENT_SET_TEXT_CHARSEQUENCE, text)
        }
        if (!target.performAction(AccessibilityNodeInfo.ACTION_SET_TEXT, arguments)) {
            return error("The text field refused the text.")
        }
        return ok().toString()
    }
}
