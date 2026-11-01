package dev.geekbridge.android

import android.app.Activity

/**
 * Hook points for features built elsewhere. The home screen shows a quick-action button for each hook and
 * enables it only when the implementing class exists, so features can land independently.
 *
 * To plug in, add a Kotlin `object` with the exact name below that implements [QuickActionHandler]:
 *  - `dev.geekbridge.android.SendFileAction`      -> "Send file"
 *  - `dev.geekbridge.android.SendClipboardAction` -> "Send clipboard"
 *  - `dev.geekbridge.android.UpdateAction`        -> "Check for updates" (About section)
 *  - `dev.geekbridge.android.ContactsAction`      -> opens the Contacts screen (Home "Contacts" tile)
 *  - `dev.geekbridge.android.CalendarAction`      -> opens the Calendar/Agenda screen (Home "Calendar" tile)
 * Lookup is by class name, so if release builds ever enable R8, keep these classes.
 */
interface QuickActionHandler {
    /** Cheap check (no I/O): false greys the button and shows [unavailableReason] instead. */
    fun isAvailable(activity: Activity): Boolean = true
    fun unavailableReason(activity: Activity): String? = null
    fun launch(activity: Activity)
}

object QuickActions {
    private const val PKG = "dev.geekbridge.android."
    val sendFile: QuickActionHandler? by lazy { load("SendFileAction") }
    val sendClipboard: QuickActionHandler? by lazy { load("SendClipboardAction") }
    val update: QuickActionHandler? by lazy { load("UpdateAction") }
    val contacts: QuickActionHandler? by lazy { load("ContactsAction") }
    val calendar: QuickActionHandler? by lazy { load("CalendarAction") }

    private fun load(simpleName: String): QuickActionHandler? = try {
        Class.forName(PKG + simpleName).getField("INSTANCE").get(null) as? QuickActionHandler
    } catch (_: ReflectiveOperationException) {
        null
    } catch (_: LinkageError) {
        null
    }
}
