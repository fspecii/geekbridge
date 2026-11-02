// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import android.content.Context
import android.content.res.Resources

/**
 * Where pure, JVM-tested code (HomeModel, SetupModel, UpdateModel, transport error text, ...) gets user-visible text.
 * Those objects take a [Strings] instead of building English literals, so every sentence is a resource a translator
 * can reach; the UI edge passes [ResStrings]. JVM tests pass a fake that reads the English res/values files.
 * Never assemble a sentence from several [get] calls: give the translator one string with positional arguments.
 */
interface Strings {
    fun get(id: Int, vararg args: Any): String
    fun plural(id: Int, quantity: Int, vararg args: Any): String
}

class ResStrings(private val res: Resources) : Strings {
    override fun get(id: Int, vararg args: Any): String =
        if (args.isEmpty()) res.getString(id) else res.getString(id, *args)

    override fun plural(id: Int, quantity: Int, vararg args: Any): String =
        res.getQuantityString(id, quantity, *args)
}

val Context.strings: Strings get() = ResStrings(resources)

/** Process-wide [Strings] for singletons that outlive any Activity (TransportController); set in GeekBridgeApp.onCreate. */
object AppStrings : Strings {
    @Volatile private var app: Context? = null
    fun init(context: Context) { app = context.applicationContext }
    private val res: Resources get() = checkNotNull(app) { "AppStrings.init not called" }.resources
    override fun get(id: Int, vararg args: Any): String = ResStrings(res).get(id, *args)
    override fun plural(id: Int, quantity: Int, vararg args: Any): String = ResStrings(res).plural(id, quantity, *args)
}
