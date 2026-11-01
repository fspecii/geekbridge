package dev.geekbridge.android

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
import android.content.pm.PackageManager
import android.widget.LinearLayout

/** The "Contacts, calendar and messages" settings, shared by the setup wizard step and any other host screen. */
object PhoneDataUi {
    const val REQUEST_CODE = 9100

    val contactsPerms = arrayOf(Manifest.permission.READ_CONTACTS)
    val calendarPerms = arrayOf(Manifest.permission.READ_CALENDAR)
    val smsPerms = arrayOf(Manifest.permission.READ_SMS, Manifest.permission.SEND_SMS, Manifest.permission.RECEIVE_SMS)

    private fun granted(a: Activity, perms: Array<String>) = perms.all { a.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    /** Wording for one feature row's state; pure so it is tested. */
    fun state(enabled: Boolean, permitted: Boolean): String = when {
        !enabled -> "Off"
        permitted -> "On"
        else -> "Waiting for permission"
    }

    fun addSection(a: Activity, kit: UiKit, parent: LinearLayout, rerender: () -> Unit) {
        PhoneDataRuntime.init(a)
        val s = PhoneDataRuntime.settings
        parent.addView(kit.text(a.getString(R.string.pd_intro), 13.5f, Tone.Muted).apply { setPadding(0, 0, 0, kit.dp(10)) })

        fun feature(icon: Int, title: Int, sub: Int, enabled: Boolean, perms: Array<String>, set: (Boolean) -> Unit) {
            val ok = granted(a, perms)
            kit.row(
                parent, icon, a.getString(title), a.getString(sub) + "\n" + state(enabled, ok),
                kit.toggle(enabled) { on ->
                    set(on)
                    if (on && !granted(a, perms)) {
                        perms.forEach { AppState.markAsked(it) }
                        a.requestPermissions(perms, REQUEST_CODE)
                    }
                    rerender()
                },
            )
            if (enabled && !ok) {
                kit.button(parent, a.getString(R.string.pd_grant), BtnStyle.Tonal) {
                    a.requestPermissions(perms, REQUEST_CODE)
                }
                kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
            }
        }
        feature(R.drawable.ic_person, R.string.pd_contacts, R.string.pd_contacts_sub, s.contacts, contactsPerms, PhoneDataRuntime::setContacts)
        kit.divider(parent)
        feature(R.drawable.ic_calendar, R.string.pd_calendar, R.string.pd_calendar_sub, s.calendar, calendarPerms, PhoneDataRuntime::setCalendar)
        if (s.calendar && granted(a, calendarPerms)) {
            val n = s.calendarIds.size
            kit.button(parent, a.getString(R.string.pd_calendar_pick) + " · " +
                (if (n == 0) a.getString(R.string.pd_calendar_none) else a.getString(R.string.pd_calendar_picked, n)), BtnStyle.Tonal) {
                pickCalendars(a, rerender)
            }
        }
        kit.divider(parent)
        feature(R.drawable.ic_message, R.string.pd_sms, R.string.pd_sms_sub, s.sms, smsPerms, PhoneDataRuntime::setSms)
        parent.addView(kit.text(a.getString(R.string.pd_sms_honest), 13f, Tone.Muted).apply { setPadding(0, kit.dp(10), 0, 0) })
    }

    private fun pickCalendars(a: Activity, rerender: () -> Unit) {
        Thread {
            val cals = PhoneDataRuntime.calendars()
            a.runOnUiThread {
                if (a.isFinishing) return@runOnUiThread
                if (cals.isEmpty()) {
                    android.widget.Toast.makeText(a, R.string.pd_calendar_loading, android.widget.Toast.LENGTH_LONG).show()
                    return@runOnUiThread
                }
                val chosen = PhoneDataRuntime.settings.calendarIds.toMutableSet()
                AlertDialog.Builder(a)
                    .setTitle(R.string.pd_calendar)
                    .setMultiChoiceItems(
                        cals.map { PhoneDataProtocol.clean(it.name, 60).ifEmpty { "Calendar ${it.id}" } }.toTypedArray(),
                        BooleanArray(cals.size) { cals[it].id in chosen },
                    ) { _, i, on -> if (on) chosen += cals[i].id else chosen -= cals[i].id }
                    .setPositiveButton(android.R.string.ok) { _, _ -> PhoneDataRuntime.setCalendarIds(chosen); rerender() }
                    .setNegativeButton(android.R.string.cancel, null)
                    .show()
            }
        }.apply { isDaemon = true; start() }
    }
}
