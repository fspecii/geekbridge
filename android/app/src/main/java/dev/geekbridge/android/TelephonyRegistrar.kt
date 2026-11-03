// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.annotation.TargetApi
import android.content.Context
import android.os.Build
import android.telephony.PhoneStateListener
import android.telephony.TelephonyCallback
import android.telephony.TelephonyManager
import java.util.concurrent.Executor

/**
 * Android binding for [CallStateMonitor]. Only the coarse call state is requested. The legacy listener
 * also receives an incoming number (empty without READ_CALL_LOG); it is ignored here and never stored.
 * Requires READ_PHONE_STATE, checked by the caller before [register].
 */
@Suppress("UseRequiresApi") // no AndroidX in this project, so @TargetApi is the available annotation
class TelephonyRegistrar(
    context: Context,
    private val sdk: Int = Build.VERSION.SDK_INT,
) : CallStateRegistrar {
    /** The default-subscription manager; per-subscription ones are derived from it. */
    val telephony: TelephonyManager? = context.applicationContext.getSystemService(TelephonyManager::class.java)

    override fun register(executor: Executor, onRaw: (Int) -> Unit): AutoCloseable {
        val tm = telephony ?: throw IllegalStateException("No telephony service")
        return registerOn(tm, executor, onRaw)
    }

    /** Observe [tm]: the default subscription, or one made with `createForSubscriptionId`. */
    fun registerOn(tm: TelephonyManager, executor: Executor, onRaw: (Int) -> Unit): AutoCloseable {
        return when (ListenerStrategy.forSdk(sdk)) {
            ListenerStrategy.TelephonyCallback -> registerCallback(tm, executor, onRaw)
            ListenerStrategy.ListenerWithExecutor -> registerListener(tm, onRaw) { legacyWithExecutor(executor, it) }
            ListenerStrategy.ListenerOnMainLooper -> registerListener(tm, onRaw) { legacyOnLooper(it) }
        }
    }

    @TargetApi(31)
    private fun registerCallback(tm: TelephonyManager, executor: Executor, onRaw: (Int) -> Unit): AutoCloseable {
        val cb = object : TelephonyCallback(), TelephonyCallback.CallStateListener {
            override fun onCallStateChanged(state: Int) = onRaw(state)
        }
        tm.registerTelephonyCallback(executor, cb)
        return AutoCloseable { tm.unregisterTelephonyCallback(cb) }
    }

    @Suppress("DEPRECATION")
    private fun registerListener(
        tm: TelephonyManager,
        onRaw: (Int) -> Unit,
        make: ((Int) -> Unit) -> PhoneStateListener,
    ): AutoCloseable {
        val listener = make(onRaw)
        tm.listen(listener, PhoneStateListener.LISTEN_CALL_STATE)
        return AutoCloseable { tm.listen(listener, PhoneStateListener.LISTEN_NONE) }
    }

    @Suppress("DEPRECATION")
    @TargetApi(29)
    private fun legacyWithExecutor(executor: Executor, onRaw: (Int) -> Unit): PhoneStateListener =
        object : PhoneStateListener(executor) {
            @Deprecated("Legacy listener; the number argument is intentionally ignored.")
            override fun onCallStateChanged(state: Int, phoneNumber: String?) = onRaw(state)
        }

    /** Must be created on a thread with a Looper (the main thread, from onStart). */
    @Suppress("DEPRECATION")
    private fun legacyOnLooper(onRaw: (Int) -> Unit): PhoneStateListener =
        object : PhoneStateListener() {
            @Deprecated("Legacy listener; the number argument is intentionally ignored.")
            override fun onCallStateChanged(state: Int, phoneNumber: String?) = onRaw(state)
        }
}
