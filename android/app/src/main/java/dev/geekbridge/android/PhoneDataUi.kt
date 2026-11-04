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

    /** A hairline between two switches, with room above it so it does not touch the button or note before it. */
    private fun separator(kit: UiKit, parent: LinearLayout) {
        parent.addView(android.view.View(parent.context), LinearLayout.LayoutParams(Ui.MATCH, kit.dp(10)))
        kit.divider(parent)
    }

    /** [preview]: draw the section for these switch positions instead of the saved ones (the debug design gallery). */
    fun addSection(a: Activity, kit: UiKit, parent: LinearLayout, preview: PhoneDataSettings? = null, rerender: () -> Unit) {
        PhoneDataRuntime.init(a)
        val s = preview ?: PhoneDataRuntime.settings
        parent.addView(kit.text(a.getString(R.string.pd_intro), 13.5f, Tone.Muted).apply { setPadding(0, 0, 0, kit.dp(10)) })

        fun feature(icon: Int, title: Int, sub: Int, enabled: Boolean, perms: Array<String>, grant: GrantFeature, set: (Boolean) -> Unit) {
            val ok = granted(a, perms)
            kit.row(
                parent, icon, a.getString(title), a.getString(R.string.pd_sub_with_state, a.getString(sub), state(enabled, ok, a.strings)),
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
            if (grant == GrantFeature.Sms) {
                // Say exactly what is missing (switch / which permission / restricted setting / computer) in plain words.
                val r = SmsReadinessAndroid.evaluate(a, s.sms)
                parent.addView(
                    kit.text(SmsReadinessLogic.message(r, a.strings), 13.5f, if (r.ready) Tone.Ok else if (r.step == SmsStep.TurnOn) Tone.Muted else Tone.Warn)
                        .apply { setPadding(0, kit.dp(6), 0, 0) },
                )
            }
            if (enabled) GrantsUi.add(a, kit, parent, grant, rerender)
            if (enabled && !ok) {
                kit.button(parent, a.getString(R.string.pd_grant), BtnStyle.Tonal) {
                    perms.forEach { AppState.markAsked(it) }
                    a.requestPermissions(perms, REQUEST_CODE)
                }
                kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
            }
        }
        feature(R.drawable.ic_person, R.string.pd_contacts, R.string.pd_contacts_sub, s.contacts, contactsPerms, GrantFeature.Contacts, PhoneDataRuntime::setContacts)
        separator(kit, parent)
        feature(R.drawable.ic_calendar, R.string.pd_calendar, R.string.pd_calendar_sub, s.calendar, calendarPerms, GrantFeature.Calendar, PhoneDataRuntime::setCalendar)
        if (s.calendar && granted(a, calendarPerms)) {
            val n = s.calendarIds.size
            kit.button(parent, a.getString(
                R.string.pd_pick_with_state,
                a.getString(R.string.pd_calendar_pick),
                if (n == 0) a.getString(R.string.pd_calendar_none) else a.resources.getQuantityString(R.plurals.pd_calendar_shared_count, n, n),
            ), BtnStyle.Tonal) {
                pickCalendars(a, rerender)
            }
            separator(kit, parent)
            feature(
                R.drawable.ic_calendar, R.string.pd_calendar_edit, R.string.pd_calendar_edit_sub,
                s.calendarEdit, calendarEditPerms, GrantFeature.CalendarEdit, PhoneDataRuntime::setCalendarEdit,
            )
            parent.addView(kit.text(a.getString(R.string.pd_calendar_google), 13f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
        }
        separator(kit, parent)
        feature(R.drawable.ic_battery, R.string.pd_battery, R.string.pd_battery_sub, s.battery, emptyArray(), GrantFeature.Battery, PhoneDataRuntime::setBattery)
        separator(kit, parent)
        notificationsSection(a, kit, parent, s, rerender)
        separator(kit, parent)
        photosSection(a, kit, parent, s, rerender)
        separator(kit, parent)
        feature(R.drawable.ic_message, R.string.pd_sms, R.string.pd_sms_sub, s.sms, smsPerms, GrantFeature.Sms, PhoneDataRuntime::setSms)
        simChoice(a, kit, parent)
        if (s.sms) {
            kit.row(
                parent, R.drawable.ic_message, a.getString(R.string.pd_sms_special), a.getString(R.string.pd_sms_special_sub),
                kit.toggle(s.smsSpecialNumbers) { on -> PhoneDataRuntime.setSmsSpecialNumbers(on); rerender() },
            )
        }
        parent.addView(kit.text(a.getString(R.string.pd_sms_honest), 13f, Tone.Muted).apply { setPadding(0, kit.dp(10), 0, 0) })
    }

    /** Which SIM a text leaves from needs READ_PHONE_STATE: asked here, where the user is already setting up Messages. */
    private fun simChoice(a: Activity, kit: UiKit, parent: LinearLayout) {
        val st = SmsReadinessAndroid.simChoice(a)
        val words = SmsSimChoice.message(st, a.strings)
        if (words.isEmpty()) return
        parent.addView(kit.text(words, 13.5f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
        if (st == SimChoiceState.Ask) {
            kit.button(parent, a.getString(R.string.sms_sim_allow), BtnStyle.Tonal) {
                AppState.markAsked(Manifest.permission.READ_PHONE_STATE)
                a.requestPermissions(arrayOf(Manifest.permission.READ_PHONE_STATE), REQUEST_CODE)
            }
        } else kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
    }

    /**
     * "Show recent photos on the computer": the switch, Android's photo access (all photos or only the ones the user
     * picks, Android 14+), a plain line saying what is missing, and who is allowed. Off by default.
     */
    private fun photosSection(a: Activity, kit: UiKit, parent: LinearLayout, s: PhoneDataSettings, rerender: () -> Unit) {
        val perms = PhotoAccessRules.requestPermissions(android.os.Build.VERSION.SDK_INT)
        fun ask() {
            perms.forEach { AppState.markAsked(it) }
            a.requestPermissions(perms, REQUEST_CODE)
        }
        kit.row(
            parent, R.drawable.ic_image, a.getString(R.string.pd_photos), a.getString(R.string.pd_photos_sub),
            kit.toggle(s.photos) { on ->
                PhoneDataRuntime.setPhotos(on)
                GrantsUi.switched(a, GrantFeature.Photos, on, rerender)
                if (on && PhotoAccessRules.evaluate(android.os.Build.VERSION.SDK_INT) { granted(a, arrayOf(it)) } == PhotoAccess.None) ask()
                rerender()
            },
        )
        val access = PhotoAccessRules.evaluate(android.os.Build.VERSION.SDK_INT) { granted(a, arrayOf(it)) }
        ComputerGrantsRuntime.init(a)
        val peers = AppState.pairedPeers
        val allowed = ComputerGrantsRuntime.grants.devicesFor(GrantFeature.Photos)
        val step = PhotosReadiness.evaluate(s.photos, access, peers.size, peers.count { it.first in allowed })
        parent.addView(
            kit.text(PhotosReadiness.message(step, a.strings), 13.5f, if (step == PhotosStep.Ready) Tone.Ok else if (s.photos) Tone.Warn else Tone.Muted)
                .apply { setPadding(0, kit.dp(6), 0, 0) },
        )
        if (!s.photos) return
        if (access == PhotoAccess.None) {
            kit.button(parent, a.getString(R.string.pd_grant), BtnStyle.Tonal) { ask() }
            kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
        } else if (access == PhotoAccess.Partial) {
            kit.button(parent, a.getString(R.string.photos_pick_more), BtnStyle.Tonal) { ask() }
        }
        GrantsUi.add(a, kit, parent, GrantFeature.Photos, rerender)
    }

    /**
     * "Show phone notifications on the computer": the switch, the exact Notification access screen with the
     * restricted-settings help for sideloaded installs, the app list (empty until chosen) and who is allowed.
     */
    private fun notificationsSection(a: Activity, kit: UiKit, parent: LinearLayout, s: PhoneDataSettings, rerender: () -> Unit) {
        kit.row(
            parent, R.drawable.ic_message, a.getString(R.string.pd_notifications), a.getString(R.string.pd_notifications_sub),
            kit.toggle(s.notifications) { on ->
                PhoneDataRuntime.setNotifications(on)
                GrantsUi.switched(a, GrantFeature.Notifications, on, rerender)
                rerender()
            },
        )
        val granted = MirrorAndroid.accessGranted(a)
        val installer = try {
            if (android.os.Build.VERSION.SDK_INT >= 30) a.packageManager.getInstallSourceInfo(a.packageName).installingPackageName else null
        } catch (_: PackageManager.NameNotFoundException) {
            null
        }
        val restricted = SmsRestriction.likely(android.os.Build.VERSION.SDK_INT, true, installer)
        ComputerGrantsRuntime.init(a)
        val peers = AppState.pairedPeers
        val allowed = ComputerGrantsRuntime.grants.devicesFor(GrantFeature.Notifications)
        val step = MirrorReadiness.evaluate(s.notifications, granted, restricted, s.notificationApps.size, peers.size, peers.count { it.first in allowed })
        parent.addView(
            kit.text(MirrorReadiness.message(step, a.strings), 13.5f, if (step == MirrorStep.Ready) Tone.Ok else if (s.notifications) Tone.Warn else Tone.Muted)
                .apply { setPadding(0, kit.dp(6), 0, 0) },
        )
        if (!s.notifications) return
        if (!granted) {
            kit.button(parent, a.getString(R.string.mirror_open_access), BtnStyle.Tonal) {
                try {
                    a.startActivity(MirrorAndroid.accessSettingsIntent(a))
                } catch (_: RuntimeException) {
                    PowerChecks.openAppSettings(a)
                }
            }
            if (restricted) {
                kit.button(parent, a.getString(R.string.setup_open_app_settings), BtnStyle.Plain) { PowerChecks.openAppSettings(a) }
                parent.addView(kit.text(a.getString(R.string.mirror_restricted_help), 13f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
            }
        }
        val n = s.notificationApps.size
        kit.button(
            parent,
            a.getString(
                R.string.pd_pick_with_state,
                a.getString(R.string.mirror_pick_apps),
                if (n == 0) a.getString(R.string.mirror_apps_none) else a.resources.getQuantityString(R.plurals.mirror_apps_count, n, n),
            ),
            BtnStyle.Tonal,
        ) { pickApps(a, rerender) }
        GrantsUi.add(a, kit, parent, GrantFeature.Notifications, rerender)
        parent.addView(kit.text(a.getString(R.string.mirror_honest), 13f, Tone.Muted).apply { setPadding(0, kit.dp(6), 0, 0) })
    }

    private fun pickApps(a: Activity, rerender: () -> Unit) {
        Thread {
            val pm = a.packageManager
            val launch = android.content.Intent(android.content.Intent.ACTION_MAIN).addCategory(android.content.Intent.CATEGORY_LAUNCHER)
            val apps = pm.queryIntentActivities(launch, 0)
                .map { it.activityInfo.packageName to it.loadLabel(pm).toString() }
                .filter { it.first != a.packageName }
                .distinctBy { it.first }
                .sortedBy { it.second.lowercase() }
            a.runOnUiThread {
                if (a.isFinishing) return@runOnUiThread
                val chosen = PhoneDataRuntime.settings.notificationApps.toMutableSet()
                AlertDialog.Builder(a)
                    .setTitle(R.string.mirror_pick_apps)
                    .setMultiChoiceItems(
                        apps.map { PhoneDataProtocol.clean(it.second, 60).ifEmpty { it.first } }.toTypedArray(),
                        BooleanArray(apps.size) { apps[it].first in chosen },
                    ) { _, i, on -> if (on) chosen += apps[i].first else chosen -= apps[i].first }
                    .setPositiveButton(android.R.string.ok) { _, _ -> PhoneDataRuntime.setNotificationApps(chosen); rerender() }
                    .setNegativeButton(android.R.string.cancel, null)
                    .show()
            }
        }.apply { isDaemon = true; start() }
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
