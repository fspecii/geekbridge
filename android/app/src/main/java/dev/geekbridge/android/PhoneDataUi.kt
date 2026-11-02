// SPDX-License-Identifier: GPL-3.0-or-later
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
    /** Asked for only when the user switches editing on; reading never needs it. */
    val calendarEditPerms = arrayOf(Manifest.permission.WRITE_CALENDAR)
    val smsPerms = arrayOf(Manifest.permission.READ_SMS, Manifest.permission.SEND_SMS, Manifest.permission.RECEIVE_SMS)

    private fun granted(a: Activity, perms: Array<String>) = perms.all { a.checkSelfPermission(it) == PackageManager.PERMISSION_GRANTED }

    /** Wording for one feature row's state; pure so it is tested. */
    fun state(enabled: Boolean, permitted: Boolean, s: Strings): String = when {
        !enabled -> s.get(R.string.pd_state_off)
        permitted -> s.get(R.string.pd_state_on)
        else -> s.get(R.string.pd_state_waiting)
    }

    fun addSection(a: Activity, kit: UiKit, parent: LinearLayout, rerender: () -> Unit) {
        PhoneDataRuntime.init(a)
        val s = PhoneDataRuntime.settings
        parent.addView(kit.text(a.getString(R.string.pd_intro), 13.5f, Tone.Muted).apply { setPadding(0, 0, 0, kit.dp(10)) })

        fun feature(icon: Int, title: Int, sub: Int, enabled: Boolean, perms: Array<String>, grant: GrantFeature, set: (Boolean) -> Unit) {
            val ok = granted(a, perms)
            kit.row(
                parent, icon, a.getString(title), a.getString(sub) + "\n" + state(enabled, ok, a.strings),
                kit.toggle(enabled) { on ->
                    set(on)
                    GrantsUi.switched(a, grant, on, rerender)
                    if (on && !granted(a, perms)) {
                        perms.forEach { AppState.markAsked(it) }
                        a.requestPermissions(perms, REQUEST_CODE)
                    }
                    rerender()
                },
            )
            if (enabled) GrantsUi.add(a, kit, parent, grant, rerender)
            if (enabled && !ok) {
                kit.button(parent, a.getString(R.string.pd_grant), BtnStyle.Tonal) {
                    a.requestPermissions(perms, REQUEST_CODE)
                }
                kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
            }
        }
        feature(R.drawable.ic_person, R.string.pd_contacts, R.string.pd_contacts_sub, s.contacts, contactsPerms, GrantFeature.Contacts, PhoneDataRuntime::setContacts)
        kit.divider(parent)
        feature(R.drawable.ic_calendar, R.string.pd_calendar, R.string.pd_calendar_sub, s.calendar, calendarPerms, GrantFeature.Calendar, PhoneDataRuntime::setCalendar)
        if (s.calendar && granted(a, calendarPerms)) {
            val n = s.calendarIds.size
            kit.button(parent, a.getString(R.string.pd_calendar_pick) + " · " +
                (if (n == 0) a.getString(R.string.pd_calendar_none) else a.resources.getQuantityString(R.plurals.pd_calendar_shared_count, n, n)), BtnStyle.Tonal) {
                pickCalendars(a, rerender)
            }
            kit.divider(parent)
            feature(
                R.drawable.ic_calendar, R.string.pd_calendar_edit, R.string.pd_calendar_edit_sub,
                s.calendarEdit, calendarEditPerms, GrantFeature.CalendarEdit, PhoneDataRuntime::setCalendarEdit,
            )
            parent.addView(kit.text(a.getString(R.string.pd_calendar_google), 13f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
        }
        kit.divider(parent)
        feature(R.drawable.ic_message, R.string.pd_sms, R.string.pd_sms_sub, s.sms, smsPerms, GrantFeature.Sms, PhoneDataRuntime::setSms)
        if (s.sms) {
            kit.row(
                parent, R.drawable.ic_message, a.getString(R.string.pd_sms_special), a.getString(R.string.pd_sms_special_sub),
                kit.toggle(s.smsSpecialNumbers) { on -> PhoneDataRuntime.setSmsSpecialNumbers(on); rerender() },
            )
        }
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
                        cals.map { PhoneDataProtocol.clean(it.name, 60).ifEmpty { a.getString(R.string.pd_calendar_fallback_name, it.id) } }.toTypedArray(),
                        BooleanArray(cals.size) { cals[it].id in chosen },
                    ) { _, i, on -> if (on) chosen += cals[i].id else chosen -= cals[i].id }
                    .setPositiveButton(android.R.string.ok) { _, _ -> PhoneDataRuntime.setCalendarIds(chosen); rerender() }
                    .setNegativeButton(android.R.string.cancel, null)
                    .show()
            }
        }.apply { isDaemon = true; start() }
    }
}

/** Per-computer choice for one opt-in: which paired computer may use it. Shared by Settings and the setup wizard. */
object GrantsUi {
    /** The master switch changed: off withdraws it from every computer, on allows a single paired computer at once. */
    fun switched(a: Activity, f: GrantFeature, on: Boolean, rerender: () -> Unit) {
        ComputerGrantsRuntime.init(a)
        if (!on) return ComputerGrantsRuntime.update { it.clear(f) }
        val peers = AppState.pairedPeers
        if (peers.size == 1) ComputerGrantsRuntime.allow(peers[0].first, f, true)
        else if (peers.size > 1) choose(a, f, rerender)
    }

    fun add(a: Activity, kit: UiKit, parent: LinearLayout, f: GrantFeature, rerender: () -> Unit) {
        ComputerGrantsRuntime.init(a)
        val peers = AppState.pairedPeers
        val ids = ComputerGrantsRuntime.grants.devicesFor(f)
        val names = peers.filter { it.first in ids }.map { it.second }
        parent.addView(
            kit.text(GrantText.summary(names, peers.size, a.strings), 13f, if (names.isEmpty()) Tone.Warn else Tone.Muted)
                .apply { setPadding(0, kit.dp(4), 0, 0) },
        )
        if (peers.isNotEmpty()) kit.button(parent, a.getString(R.string.grant_choose), BtnStyle.Tonal) { choose(a, f, rerender) }
    }

    private fun choose(a: Activity, f: GrantFeature, rerender: () -> Unit) {
        val peers = AppState.pairedPeers
        if (peers.isEmpty() || a.isFinishing) return
        val chosen = ComputerGrantsRuntime.grants.devicesFor(f).toMutableSet()
        AlertDialog.Builder(a)
            .setTitle(R.string.grant_title)
            .setMessage(R.string.grant_body)
            .setMultiChoiceItems(
                peers.map { it.second }.toTypedArray(),
                BooleanArray(peers.size) { peers[it].first in chosen },
            ) { _, i, on -> if (on) chosen += peers[i].first else chosen -= peers[i].first }
            .setPositiveButton(android.R.string.ok) { _, _ ->
                ComputerGrantsRuntime.update { g -> peers.fold(g) { acc, p -> acc.set(p.first, f, p.first in chosen) } }
                rerender()
            }
            .setNegativeButton(android.R.string.cancel, null)
            .show()
    }
}
