// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.net.ConnectivityManager
import android.net.LinkProperties
import android.net.Network
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import java.net.Inet4Address

/**
 * Keeps the link to paired computers reachable while the app is closed.
 *
 * Lightweight by design: it only runs while at least one computer is paired and the user has not turned
 * "Stay connected" off; it holds one TCP listener and one UDP announce (no multicast lock, no wake lock),
 * has no timers or polling loops, and reacts only to events: a default-network change re-announces the phone
 * once (debounced); dead peers are detected by kernel TCP keepalive in the Rust transport. It is a
 * `connectedDevice` foreground service because it keeps a connection to a device on the local network;
 * that type may be started from BOOT_COMPLETED on Android 14+ (dataSync may not).
 */
class LinkService : Service() {
    private val main = Handler(Looper.getMainLooper())
    private lateinit var controller: TransportController
    private var callback: ConnectivityManager.NetworkCallback? = null
    private var lastNetwork: Long? = null
    private var lastV4: Set<String>? = null
    private val restart = Runnable { restartLink(0) }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        AppState.init(this)
        controller = LinkRuntime.controller()
        isRunning = true
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        AppState.init(this)
        // startForegroundService() obliges us to call startForeground() within seconds, even if we stop right away.
        // A sticky restart from the background can be refused on Android 12+; stop quietly rather than crash.
        if (!ServicePolicy.promoteOrStop(::promote, ::stopSelf)) return START_NOT_STICKY
        if (intent?.action == ACTION_STOP) {
            AppState.keepConnected = false
            stopSelf()
            return START_NOT_STICKY
        }
        if (!ServicePolicy.shouldRun(AppState.pairedCount, AppState.keepConnected)) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (controller.phase == LinkPhase.Stopped) controller.start(serviceOptions())
        watchNetwork()
        return START_STICKY
    }

    override fun onDestroy() {
        isRunning = false
        main.removeCallbacksAndMessages(null)
        callback?.let { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(it) }
        callback = null
        controller.stop(R.string.link_stop_background)
        super.onDestroy()
    }

    private fun serviceOptions() = LinkRuntime.startOptions(
        stateDir = filesDir.absolutePath,
        deviceName = DeviceName.current(this),
        discoveryListen = false, // listening needs a multicast lock; computers find us from our announcement
        announce = true,
    )

    private fun watchNetwork() {
        if (callback != null) return
        val cm = getSystemService(ConnectivityManager::class.java) ?: return
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                val id = network.networkHandle
                val change = ServicePolicy.shouldRestartOnNetwork(lastNetwork, id)
                if (id != lastNetwork) lastV4 = null // addresses of the new network arrive in onLinkPropertiesChanged
                lastNetwork = id
                if (change) scheduleRestart()
            }

            override fun onLinkPropertiesChanged(network: Network, lp: LinkProperties) {
                if (network.networkHandle != lastNetwork) return
                val v4 = lp.linkAddresses.mapNotNull { it.address.takeIf { a -> a is Inet4Address }?.hostAddress }.toSet()
                val changed = ServicePolicy.addressesChanged(lastV4, v4)
                lastV4 = v4
                if (changed) scheduleRestart()
            }
        }
        try {
            cm.registerDefaultNetworkCallback(cb, main)
            callback = cb
        } catch (_: RuntimeException) {
            // Too many callbacks or missing permission: the link still works, it just will not re-announce.
        }
    }

    private fun scheduleRestart() {
        main.removeCallbacks(restart)
        main.postDelayed(restart, ServicePolicy.RESTART_DEBOUNCE_MS)
    }

    /** Re-announce on the new network: stop, then start again once the native side reports stopped. Bounded. */
    private fun restartLink(attempt: Int) {
        if (!isRunning) return
        if (!ServicePolicy.shouldRun(AppState.pairedCount, AppState.keepConnected)) return stopSelf()
        when (controller.phase) {
            LinkPhase.Running -> { controller.stop(R.string.link_stop_network); retryStart(attempt) }
            LinkPhase.Stopped -> controller.start(serviceOptions())
            else -> retryStart(attempt)
        }
    }

    private fun retryStart(attempt: Int) {
        if (attempt < MAX_RESTART_ATTEMPTS) main.postDelayed({ restartLink(attempt + 1) }, 500)
    }

    private fun promote() {
        val n = buildNotification()
        if (Build.VERSION.SDK_INT >= 29) {
            startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIFICATION_ID, n)
        }
    }

    private fun buildNotification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(
            NotificationChannel(CHANNEL, getString(R.string.channel_link), NotificationManager.IMPORTANCE_LOW).apply {
                description = getString(R.string.channel_link_desc)
                setShowBadge(false)
            },
        )
        val open = PendingIntent.getActivity(
            this, 0, Intent(this, HomeActivity::class.java), PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val stop = PendingIntent.getService(
            this, 1, Intent(this, LinkService::class.java).setAction(ACTION_STOP),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        val paired = AppState.pairedCount
        var text = resources.getQuantityString(R.plurals.notif_ready, paired, paired)
        if (PowerChecks.batteryState(this) != BatteryState.Unrestricted) text += " · " + getString(R.string.notif_battery)
        return Notification.Builder(this, CHANNEL)
            .setSmallIcon(R.drawable.ic_stat_link)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(text)
            .setContentIntent(open)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setShowWhen(false)
            .setCategory(Notification.CATEGORY_SERVICE)
            .addAction(Notification.Action.Builder(null, getString(R.string.notif_stop), stop).build())
            .build()
    }

    companion object {
        private const val CHANNEL = "link"
        private const val NOTIFICATION_ID = 1
        private const val MAX_RESTART_ATTEMPTS = 6
        const val ACTION_STOP = "dev.geekbridge.android.action.STOP_LINK"

        @Volatile var isRunning = false
            private set

        /** Starts or stops the service to match [ServicePolicy]. Safe to call from any entry point. */
        fun sync(ctx: Context) {
            AppState.init(ctx)
            val app = ctx.applicationContext
            val intent = Intent(app, LinkService::class.java)
            if (ServicePolicy.shouldRun(AppState.pairedCount, AppState.keepConnected)) {
                try {
                    app.startForegroundService(intent)
                } catch (_: RuntimeException) {
                    // Android refused a background start (e.g. app in a restricted bucket). The next visit retries.
                }
            } else if (isRunning) {
                app.stopService(intent)
            }
        }
    }
}
