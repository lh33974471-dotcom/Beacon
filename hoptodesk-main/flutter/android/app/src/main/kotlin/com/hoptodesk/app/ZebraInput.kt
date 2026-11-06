package com.hoptodesk.app

import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.ServiceConnection
import android.os.Build
import android.os.IBinder
import android.os.SystemClock
import android.util.Log
import android.view.InputDevice
import android.view.KeyCharacterMap
import android.view.KeyEvent as KeyEventAndroid
import android.view.MotionEvent
import com.zebra.eventinjectionservice.IEventInjectionService
import hbb.KeyEventConverter
import hbb.MessageOuterClass.KeyEvent

object ZebraInput {

    private const val SERVICE_PACKAGE = "com.zebra.eventinjectionservice"
    private const val SERVICE_ACTION = "com.zebra.eventinjectionservice.IEventInjectionService"
    private const val MODE_WAIT_FOR_FINISH = 2
    private const val VIRTUAL_DEVICE_ID = KeyCharacterMap.VIRTUAL_KEYBOARD

    private const val logTag = "zebra input"

    private var service: IEventInjectionService? = null
    private var authenticated = false
    private var bindRequested = false
    private var pointerDownTime = 0L
    private var pointerIsDown = false

    val isAvailable: Boolean
        get() = authenticated && service != null

    val isZebraDevice: Boolean
        get() = sequenceOf(Build.MANUFACTURER, Build.BRAND)
            .filterNotNull()
            .map { it.lowercase() }
            .any { it.contains("zebra") || it.contains("symbol") }

    fun connect(context: Context) {
        if (bindRequested || !isZebraDevice) {
            return
        }
        bindRequested = true
        val intent = Intent(SERVICE_ACTION).setPackage(SERVICE_PACKAGE)
        val requested = try {
            context.applicationContext.bindService(intent, connection, Context.BIND_AUTO_CREATE)
        } catch (e: Exception) {
            Log.e(logTag, "bindService failed: ${e.message}")
            false
        }
        if (!requested) {
            bindRequested = false
            Log.d(logTag, "event injection service not present")
        }
    }

    fun disconnect(context: Context) {
        if (!bindRequested) {
            return
        }
        try {
            context.applicationContext.unbindService(connection)
        } catch (e: Exception) {
            Log.e(logTag, "unbindService failed: ${e.message}")
        }
        bindRequested = false
        service = null
        authenticated = false
        pointerIsDown = false
    }

    private val connection = object : ServiceConnection {
        override fun onServiceConnected(name: ComponentName, binder: IBinder) {
            val bound = IEventInjectionService.Stub.asInterface(binder)
            service = bound
            authenticated = try {
                bound.authenticate()
            } catch (e: Exception) {
                Log.e(logTag, "authenticate failed: ${e.message}")
                false
            }
            if (!authenticated) {
                Log.w(logTag, "authenticate rejected, check the Access Manager profile")
            }
        }

        override fun onServiceDisconnected(name: ComponentName) {
            service = null
            authenticated = false
            pointerIsDown = false
        }
    }

    fun onPointerInput(mask: Int, x: Int, y: Int): Boolean {
        if (!isAvailable) {
            return false
        }
        val now = SystemClock.uptimeMillis()
        val action = when (mask) {
            LEFT_DOWN -> {
                pointerDownTime = now
                pointerIsDown = true
                MotionEvent.ACTION_DOWN
            }
            LEFT_MOVE, 0 -> {
                if (!pointerIsDown) return true
                MotionEvent.ACTION_MOVE
            }
            LEFT_UP -> {
                if (!pointerIsDown) return true
                pointerIsDown = false
                MotionEvent.ACTION_UP
            }
            else -> return false
        }
        val event = MotionEvent.obtain(
            pointerDownTime, now, action, x.toFloat(), y.toFloat(), 0
        )
        event.source = InputDevice.SOURCE_TOUCHSCREEN
        return try {
            inject(event)
        } finally {
            event.recycle()
        }
    }

    fun onKeyEvent(data: ByteArray): Boolean {
        if (!isAvailable) {
            return false
        }
        val proto = try {
            KeyEvent.parseFrom(data)
        } catch (e: Exception) {
            Log.e(logTag, "key event parse failed: ${e.message}")
            return false
        }
        if (proto.hasSeq()) {
            return false
        }
        val event = KeyEventConverter.toAndroidKeyEvent(proto)
        if (event.keyCode == 0) {
            return false
        }
        val now = SystemClock.uptimeMillis()
        val down = KeyEventAndroid(
            now, now, event.action, event.keyCode, 0, event.metaState,
            VIRTUAL_DEVICE_ID, 0, 0, InputDevice.SOURCE_KEYBOARD
        )
        if (!inject(down)) {
            return false
        }
        if (!proto.press) {
            return true
        }
        val up = KeyEventAndroid(
            now, now, KeyEventAndroid.ACTION_UP, event.keyCode, 0, event.metaState,
            VIRTUAL_DEVICE_ID, 0, 0, InputDevice.SOURCE_KEYBOARD
        )
        return inject(up)
    }

    private fun inject(event: android.view.InputEvent): Boolean {
        val bound = service ?: return false
        return try {
            bound.injectInputEvent(event, MODE_WAIT_FOR_FINISH)
        } catch (e: Exception) {
            Log.e(logTag, "injectInputEvent failed: ${e.message}")
            authenticated = false
            false
        }
    }
}
