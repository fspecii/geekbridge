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
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import java.net.Inet4Address

/**
 * Keeps the link to paired computers reachable while the app is closed.
 *
 * Lightweight by design: it only runs while at least one computer is paired and the user has not turned
 * "Stay connected" off; it holds one TCP listener and one UDP announce (no multicast lock, no wake lock;
 * a Wi-Fi lock only while linked, see [WifiLockPolicy]),
 * has no timers or polling loops, and reacts only to events: a change of Wi-Fi network re-announces the phone
 * once (debounced; a default-network flip between Wi-Fi and mobile data is ignored, see [ServicePolicy.onWifiAvailable]),
 * and screen on / Doze windows nudge the core (ping now, wake a sleeping redial); dead peers are detected by the
 * heartbeat and kernel TCP keepalive in the Rust transport. It is a
 * `connectedDevice` foreground service because it keeps a connection to a device on the local network;
 * that type may be started from BOOT_COMPLETED on Android 14+ (dataSync may not).
 */
class LinkService : Service() {
    private val main = Handler(Looper.getMainLooper())
    private lateinit var controller: TransportController
    private var callback: ConnectivityManager.NetworkCallback? = null
    private var lastNetwork: Long? = null
    private var lastV4: Set<String>? = null
    private val wifiAddresses = HashMap<Long, List<String>>()
    private var pushedBind: String? = null
    private var bindPushed = false
    private val restart = Runnable { restartGen++; restartLink(0, restartGen) }
    private lateinit var wifi: WifiLinkLock
    private var wasConnected = false
    private var lostAt: Long? = null
    private val wifiObserver: () -> Unit = { syncWifiLock() }
    private val wifiRecheck = Runnable { syncWifiLock() }

    private val screenReceiver = object : android.content.BroadcastReceiver() {
        override fun onReceive(context: Context, intent: Intent) {
            when (intent.action) {
                Intent.ACTION_SCREEN_OFF -> controller.setScreen(false)
                Intent.ACTION_SCREEN_ON -> {
                    controller.setScreen(true)
                    // The user is back: ping now so a dead link shows itself at once, and wake a redial or an
                    // announcement whose timer stood still while the CPU slept.
                    if (ScreenRules.nudgeOnScreenOn) controller.heartbeatNow()
                }
                android.os.PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED -> {
                    val idle = getSystemService(android.os.PowerManager::class.java)?.isDeviceIdleMode == true
                    GbLog.i("link", "doze idle=$idle")
                    if (DozeRules.pingNow(idle)) controller.heartbeatNow()
                }
            }
        }
    }
    private var screenWatched = false

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        AppState.init(this)
        controller = LinkRuntime.controller()
        wifi = WifiLinkLock(this)
        controller.observers += wifiObserver
        isRunning = true
        watchScreen()
    }

    /** Screen off slows our heartbeat (the core keeps the link; the unlock ping and the redial are unchanged). */
    private fun watchScreen() {
        val pm = getSystemService(android.os.PowerManager::class.java)
        controller.setScreen(pm?.isInteractive != false)
        registerReceiver(screenReceiver, android.content.IntentFilter().apply {
            addAction(Intent.ACTION_SCREEN_OFF)
            addAction(Intent.ACTION_SCREEN_ON)
            addAction(android.os.PowerManager.ACTION_DEVICE_IDLE_MODE_CHANGED)
        })
        screenWatched = true
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        AppState.init(this)
        // startForegroundService() obliges us to call startForeground() within seconds, even if we stop right away.
        // A sticky restart from the background can be refused on Android 12+; stop quietly rather than crash.
        if (!ServicePolicy.promoteOrStop(::promote, ::stopSelf)) {
            AppState.serviceError = "startForeground refused"
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_STOP) {
            AppState.keepConnected = false
            stopSelf()
            return START_NOT_STICKY
        }
        if (!AppState.wantService()) {
            stopSelf()
            return START_NOT_STICKY
        }
        if (controller.phase == LinkPhase.Stopped) controller.start(serviceOptions())
        watchNetwork()
        scheduleGraceCheck()
        return START_STICKY
    }

    /**
     * Holds the Wi-Fi lock while a computer is linked (and briefly after a loss, so the redial is quick). A loss
     * is logged with how long the link had lasted; the reason itself comes from the transport's `link_closed` event.
     */
    private fun syncWifiLock() {
        val connected = controller.snapshot?.connections?.any { it.trusted } == true
        val now = android.os.SystemClock.elapsedRealtime()
        if (wasConnected && !connected) {
            lostAt = now
            GbLog.i("link", "link lost (service running, phase=${controller.phase})")
        } else if (connected) {
            lostAt = null
        }
        wasConnected = connected
        val since = lostAt?.let { now - it }
        wifi.set(WifiLockPolicy.wanted(AppState.pairedCount, connected, since))
        main.removeCallbacks(wifiRecheck)
        if (since != null && since < WifiLockPolicy.LINGER_MS) main.postDelayed(wifiRecheck, WifiLockPolicy.LINGER_MS - since + 100)
    }

    override fun onDestroy() {
        GbLog.i("link", "service destroyed (paired=${AppState.pairedCount}, keepConnected=${AppState.keepConnected})")
        controller.observers -= wifiObserver
        if (screenWatched) try { unregisterReceiver(screenReceiver) } catch (_: IllegalArgumentException) { }
        screenWatched = false
        controller.setScreen(true)
        wifi.set(false)
        isRunning = false
        main.removeCallbacksAndMessages(null)
        callback?.let { getSystemService(ConnectivityManager::class.java)?.unregisterNetworkCallback(it) }
        callback = null
        wifiAddresses.clear()
        pushWifiBind()
        controller.stop(R.string.link_stop_background)
        super.onDestroy()
    }

    private fun serviceOptions() = LinkRuntime.startOptions(
        stateDir = filesDir.absolutePath,
        deviceName = DeviceName.current(this),
        discoveryListen = false, // listening needs a multicast lock; computers find us from our announcement
        announce = true,
    )

    /** Watches the Wi-Fi network (not the default network, see [ServicePolicy.onWifiAvailable]). */
    private fun watchNetwork() {
        if (callback != null) return
        val cm = getSystemService(ConnectivityManager::class.java) ?: return
        val cb = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) {
                val id = network.networkHandle
                val action = ServicePolicy.onWifiAvailable(lastNetwork, id)
                if (id != lastNetwork) lastV4 = null // addresses of the new network arrive in onLinkPropertiesChanged
                lastNetwork = id
                GbLog.i("link", "wifi network available (action=$action)")
                when (action) {
                    NetworkAction.Restart -> scheduleRestart()
                    NetworkAction.Nudge -> controller.heartbeatNow()
                    NetworkAction.None -> Unit
                }
            }

            override fun onLost(network: Network) {
                GbLog.i("link", "wifi network lost")
                wifiAddresses.remove(network.networkHandle)
                pushWifiBind()
            }

            override fun onLinkPropertiesChanged(network: Network, lp: LinkProperties) {
                val v4 = lp.linkAddresses.mapNotNull { it.address.takeIf { a -> a is Inet4Address }?.hostAddress }.toSet()
                wifiAddresses[network.networkHandle] = v4.toList()
                pushWifiBind()
                if (network.networkHandle != lastNetwork) return
                val changed = ServicePolicy.addressesChanged(lastV4, v4)
                lastV4 = v4
                if (changed) {
                    GbLog.i("link", "IPv4 address set changed, re-announcing")
                    scheduleRestart()
                }
            }
        }
        // Any Wi-Fi network, with or without internet access: a LAN-only access point is a fine place for the link.
        val wifi = NetworkRequest.Builder()
            .addTransportType(NetworkCapabilities.TRANSPORT_WIFI)
            .removeCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
            .build()
        try {
            cm.registerNetworkCallback(wifi, cb, main)
            callback = cb
        } catch (_: RuntimeException) {
            // Too many callbacks or missing permission: the link still works, it just will not re-announce.
        }
    }

    /** Keeps the core's dials and announcements on Wi-Fi when mobile data is the default network ([WifiBindPolicy]). */
    private fun pushWifiBind() {
        val ip = WifiBindPolicy.choose(wifiAddresses.values)
        if (!WifiBindPolicy.shouldPush(!bindPushed, pushedBind, ip)) return
        bindPushed = true
        pushedBind = ip
        GbLog.i("link", "wifi bind ${if (ip != null) "set" else "cleared"}")
        controller.setWifiBind(ip)
    }

    /** A pairing that never finished must not keep the service alive forever: look again when its grace ends. */
    private fun scheduleGraceCheck() {
        main.removeCallbacks(graceCheck)
        ServicePolicy.graceCheckDelayMs(AppState.pairedCount, AppState.pairingGraceLeftMs())?.let { main.postDelayed(graceCheck, it) }
    }

    // The grace may have been extended since this was scheduled (another pairing prompt): look again until it is over.
    private val graceCheck = Runnable { if (!AppState.wantService()) stopSelf() else scheduleGraceCheck() }

    /** Swiping the app away from Recents must not end the link: this service is not tied to the task. */
    override fun onTaskRemoved(rootIntent: Intent?) {
        if (AppState.wantService() && controller.phase == LinkPhase.Stopped) controller.start(serviceOptions())
        super.onTaskRemoved(rootIntent)
    }

    // Identifies the restart chain. A newer network change, or a service stop, supersedes the retries a chain has queued.
    private var restartGen = 0

    private fun scheduleRestart() {
        main.removeCallbacks(restart)
        main.postDelayed(restart, ServicePolicy.RESTART_DEBOUNCE_MS)
    }

    /** Re-announce on the new network: stop, then start again once the native side reports stopped. Bounded. */
    private fun restartLink(attempt: Int, gen: Int) {
        if (!isRunning || gen != restartGen) return
        if (!AppState.wantService()) return stopSelf()
        when (ServicePolicy.restartStep(attempt, controller.phase)) {
            ServicePolicy.RestartStep.Stop -> {
                GbLog.i("link", "closing the link to re-announce on the new network")
                controller.stop(R.string.link_stop_network)
                retryStart(attempt, gen)
            }
            ServicePolicy.RestartStep.Start -> controller.start(serviceOptions())
            ServicePolicy.RestartStep.Wait -> retryStart(attempt, gen)
            ServicePolicy.RestartStep.Done -> Unit
        }
    }

    private fun retryStart(attempt: Int, gen: Int) {
        if (attempt < MAX_RESTART_ATTEMPTS) main.postDelayed({ restartLink(attempt + 1, gen) }, 500)
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
            if (AppState.wantService()) {
                try {
                    app.startForegroundService(intent)
                    AppState.serviceError = null
                } catch (e: RuntimeException) {
                    // Android refused a background start (e.g. app in a restricted bucket). The next visit retries.
                    AppState.serviceError = e.javaClass.simpleName
                }
            } else if (isRunning) {
                app.stopService(intent)
            }
        }
    }
}
