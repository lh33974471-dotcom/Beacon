package com.hoptodesk.app

import android.app.admin.DevicePolicyManager
import android.content.ComponentName
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.BatteryManager
import android.os.Build
import android.os.Environment
import android.os.Handler
import android.os.HandlerThread
import android.os.PowerManager
import android.os.StatFs
import android.util.Log
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import org.json.JSONArray
import org.json.JSONObject

object AgentCommands {
    private const val logTag = "AgentCommands"

    private val worker = HandlerThread("hoptodesk-agent").apply { start() }
    private val handler = Handler(worker.looper)

    fun isPlayBuild(ctx: Context): Boolean = try {
        ctx.packageManager.getApplicationInfo(ctx.packageName, PackageManager.GET_META_DATA)
            .metaData?.getBoolean("com.hoptodesk.play_build", false) == true
    } catch (e: Exception) {
        false
    }

    fun getByName(ctx: Context, name: String): String {
        return when (name) {
            "device_health" -> deviceHealthJson(ctx)
            "list_apps" -> runForResult { installedAppsJson(ctx) }
            else -> ""
        }
    }

    fun setByName(ctx: Context, name: String, arg1: String) {
        when (name) {
            "launch_app" -> handler.post { launchApp(ctx, arg1) }
            "wake_screen" -> wakeScreen(ctx)
            "reboot_device" -> handler.post { rebootDevice(ctx) }
            else -> {}
        }
    }

    private fun isDeviceOwner(ctx: Context): Boolean {
        return try {
            val dpm = ctx.getSystemService(Context.DEVICE_POLICY_SERVICE) as DevicePolicyManager
            dpm.isDeviceOwnerApp(ctx.packageName)
        } catch (e: Exception) {
            false
        }
    }

    private fun rebootDevice(ctx: Context) {
        try {
            if (!isDeviceOwner(ctx)) {
                Log.e(logTag, "reboot_device: app is not device owner")
                return
            }
            val dpm = ctx.getSystemService(Context.DEVICE_POLICY_SERVICE) as DevicePolicyManager
            dpm.reboot(ComponentName(ctx, HopAdminReceiver::class.java))
        } catch (e: Exception) {
            Log.e(logTag, "rebootDevice error: $e")
        }
    }

    private fun runForResult(block: () -> String): String {
        val result = arrayOf("")
        val latch = CountDownLatch(1)
        handler.post {
            try {
                result[0] = block()
            } catch (e: Exception) {
                Log.e(logTag, "runForResult error: $e")
            } finally {
                latch.countDown()
            }
        }
        return try {
            if (latch.await(8, TimeUnit.SECONDS)) result[0] else "[]"
        } catch (e: Exception) {
            "[]"
        }
    }

    private fun deviceHealthJson(ctx: Context): String {
        val json = JSONObject()
        try {
            json.put("model", Build.MODEL)
            json.put("manufacturer", Build.MANUFACTURER)
            json.put("android_sdk", Build.VERSION.SDK_INT)
            json.put("android_release", Build.VERSION.RELEASE)
            json.put("accessibility_enabled", InputService.isOpen)
            try {
                val bm = ctx.registerReceiver(null, IntentFilter(Intent.ACTION_BATTERY_CHANGED))
                if (bm != null) {
                    val level = bm.getIntExtra(BatteryManager.EXTRA_LEVEL, -1)
                    val scale = bm.getIntExtra(BatteryManager.EXTRA_SCALE, 100)
                    if (level >= 0 && scale > 0) {
                        json.put("battery_percent", level * 100 / scale)
                    }
                    val status = bm.getIntExtra(BatteryManager.EXTRA_STATUS, -1)
                    json.put("is_charging", status == BatteryManager.BATTERY_STATUS_CHARGING || status == BatteryManager.BATTERY_STATUS_FULL)
                }
            } catch (e: Exception) {
            }
            try {
                val stat = StatFs(Environment.getDataDirectory().path)
                json.put("storage_free_mb", stat.availableBytes / (1024 * 1024))
                json.put("storage_total_mb", stat.totalBytes / (1024 * 1024))
            } catch (e: Exception) {
            }
        } catch (e: Exception) {
            Log.e(logTag, "deviceHealthJson error: $e")
        }
        return json.toString()
    }

    private fun installedAppsJson(ctx: Context): String {
        val arr = JSONArray()
        try {
            val pm = ctx.packageManager
            val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
            for (ri in pm.queryIntentActivities(intent, 0)) {
                try {
                    val pkg = ri.activityInfo.packageName
                    val o = JSONObject()
                    o.put("package", pkg)
                    o.put("label", ri.loadLabel(pm).toString())
                    try { o.put("version", pm.getPackageInfo(pkg, 0).versionName ?: "") } catch (e: Exception) {}
                    arr.put(o)
                } catch (e: Exception) {
                }
            }
        } catch (e: Exception) {
            Log.e(logTag, "installedAppsJson error: $e")
        }
        return arr.toString()
    }

    private fun launchApp(ctx: Context, pkg: String) {
        try {
            val intent = ctx.packageManager.getLaunchIntentForPackage(pkg.trim())
            if (intent != null) {
                intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_RESET_TASK_IF_NEEDED)
                ctx.startActivity(intent)
            } else {
                Log.e(logTag, "launchApp: no launch intent for $pkg")
            }
        } catch (e: Exception) {
            Log.e(logTag, "launchApp error: $e")
        }
    }

    private fun wakeScreen(ctx: Context) {
        try {
            val pm = ctx.getSystemService(Context.POWER_SERVICE) as PowerManager
            @Suppress("DEPRECATION")
            val wl = pm.newWakeLock(
                PowerManager.SCREEN_BRIGHT_WAKE_LOCK or PowerManager.ACQUIRE_CAUSES_WAKEUP,
                "hoptodesk:agent_wake"
            )
            wl.acquire(3000)
        } catch (e: Exception) {
            Log.e(logTag, "wakeScreen error: $e")
        }
    }
}
