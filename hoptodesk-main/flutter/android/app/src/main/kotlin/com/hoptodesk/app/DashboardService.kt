package com.hoptodesk.app

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.ServiceInfo
import android.os.BatteryManager
import android.os.Build
import android.os.IBinder
import android.util.Log
import androidx.annotation.Keep
import androidx.core.app.NotificationCompat
import ffi.FFI

class DashboardService : Service() {
    private val logTag = "DashboardService"

    companion object {
        const val CHANNEL_ID = "hoptodesk_dashboard"
        const val NOTIFY_ID = 8472

        @Volatile
        var isRunning = false

        @Volatile
        var coreInited = false
    }

    private val batteryReceiver = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            val level = intent?.getIntExtra(BatteryManager.EXTRA_LEVEL, -1) ?: -1
            val scale = intent?.getIntExtra(BatteryManager.EXTRA_SCALE, -1) ?: -1
            if (level >= 0 && scale > 0) {
                try {
                    FFI.setDeviceBattery(level * 100 / scale)
                } catch (e: Exception) {
                    Log.e(logTag, "setDeviceBattery failed: $e")
                }
            }
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        startInForeground()
        try {
            FFI.init(this)
        } catch (e: Exception) {
            Log.e(logTag, "FFI.init failed: $e")
        }
        try {
            val sticky = registerReceiver(
                batteryReceiver, IntentFilter(Intent.ACTION_BATTERY_CHANGED)
            )
            batteryReceiver.onReceive(this, sticky)
        } catch (e: Exception) {
            Log.e(logTag, "battery receiver register failed: $e")
        }
        isRunning = true
    }

    @Keep
    fun rustGetByName(name: String): String {
        return AgentCommands.getByName(applicationContext, name)
    }

    @Keep
    fun rustSetByName(name: String, arg1: String, arg2: String) {
        AgentCommands.setByName(applicationContext, name, arg1)
    }

    @Keep
    fun rustAgentCall(name: String, argsJson: String): String {
        return AgentScreen.call(applicationContext, name, argsJson)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        startInForeground()
        isRunning = true
        if (intent?.getBooleanExtra(EXT_INIT_FROM_BOOT, false) == true && !coreInited) {
            coreInited = true
            try {
                FFI.init(this)
                val restrictions = (getSystemService(Context.RESTRICTIONS_SERVICE) as? android.content.RestrictionsManager)?.applicationRestrictions
                restrictions?.getString("custom_api_url")?.trim()?.takeIf { it.isNotEmpty() }?.let { FFI.setManagedApiUrl(it) }
                val configPath = getSharedPreferences(KEY_SHARED_PREFERENCES, Context.MODE_PRIVATE)
                    .getString(KEY_APP_DIR_CONFIG_PATH, "") ?: ""
                FFI.startServer(configPath, "")
            } catch (e: Exception) {
                Log.e(logTag, "boot core init failed: $e")
            }
        }
        return START_STICKY
    }

    override fun onTaskRemoved(rootIntent: Intent?) {
        FFI.closeAllSessions()
        super.onTaskRemoved(rootIntent)
    }

    private fun startInForeground() {
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val channel = NotificationChannel(
                CHANNEL_ID, "Dashboard connection", NotificationManager.IMPORTANCE_LOW
            )
            channel.setShowBadge(false)
            nm.createNotificationChannel(channel)
        }
        val notification: Notification = NotificationCompat.Builder(this, CHANNEL_ID)
            .setContentTitle(getString(R.string.app_name))
            .setContentText("Connected to dashboard")
            .setSmallIcon(R.mipmap.ic_stat_logo)
            .setOngoing(true)
            .setPriority(NotificationCompat.PRIORITY_LOW)
            .build()
        try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.UPSIDE_DOWN_CAKE) {
                startForeground(
                    NOTIFY_ID, notification,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE
                )
            } else {
                startForeground(NOTIFY_ID, notification)
            }
        } catch (e: Exception) {
            Log.e(logTag, "startForeground failed: $e")
        }
    }

    override fun onDestroy() {
        try {
            unregisterReceiver(batteryReceiver)
        } catch (e: Exception) {
        }
        isRunning = false
        super.onDestroy()
    }
}
