// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android.callaudio

/*
 * DEBUG BUILDS ONLY, EXPERIMENT. What the self-test found, decided from numbers only. Kotlin twin of
 * crates/bridge-callwifi/src/selftest.rs (same vectors in SelfTestVerdictTest).
 */
enum class Verdict(val wire: String) {
    NotRun("not_run"),
    Works("works"),
    Silent("silent"),
    Refused("refused"),
    Unsure("unsure"),
    ;

    companion object {
        fun fromWire(s: String?): Verdict = entries.firstOrNull { it.wire == s } ?: NotRun
    }
}

/** One capture attempt: [opened] false = Android refused; [frames] samples received; [rms] of 16-bit samples. */
data class Capture(val opened: Boolean, val rms: Int, val silenced: Boolean = false, val frames: Int = if (opened) 48_000 else 0)

object SelfTestVerdict {
    const val SILENT_RMS = 30

    fun classify(control: Capture?, call: Capture?): Verdict {
        if (call == null) return Verdict.NotRun
        if (!call.opened) return Verdict.Refused
        if (call.silenced || call.rms < SILENT_RMS) return Verdict.Silent
        // The control proves nothing unless it really ran: opened and delivered samples. Refused, empty or missing: unsure.
        if (control == null || !control.opened || control.frames <= 0) return Verdict.Unsure
        if (!control.silenced && control.rms >= SILENT_RMS && control.rms.toLong() * 2 >= call.rms.toLong()) {
            return Verdict.Unsure
        }
        return Verdict.Works
    }

    /** Better is lower; used to pick the best source. */
    fun rank(v: Verdict) = when (v) {
        Verdict.Works -> 0
        Verdict.Unsure -> 1
        Verdict.Silent -> 2
        Verdict.Refused -> 3
        Verdict.NotRun -> 4
    }
}
