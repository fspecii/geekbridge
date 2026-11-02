// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import java.util.Calendar
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class MonthLayoutTest {
    // November 2026 starts on a Sunday and has 30 days.
    @Test fun sundayFirstMonthStartingOnSundayHasNoBlanks() {
        val m = MonthLayout.of(2026, Calendar.NOVEMBER, Calendar.SUNDAY)
        assertEquals(0, m.leadingBlanks)
        assertEquals(30, m.days)
        assertEquals(5, m.rows)
        assertEquals(1, m.dayAt(0))
        assertEquals(30, m.dayAt(29))
        assertNull(m.dayAt(30))
    }

    @Test fun mondayFirstShiftsTheStart() {
        val m = MonthLayout.of(2026, Calendar.NOVEMBER, Calendar.MONDAY)
        assertEquals(6, m.leadingBlanks)
        assertEquals(6, m.rows)
        assertNull(m.dayAt(5))
        assertEquals(1, m.dayAt(6))
    }

    @Test fun leapFebruaryHasTwentyNineDays() {
        val m = MonthLayout.of(2028, Calendar.FEBRUARY, Calendar.MONDAY)
        assertEquals(29, m.days)
        assertEquals(1, m.leadingBlanks) // 1 Feb 2028 is a Tuesday
    }

    @Test fun exactlyFourRowsWhenFebruaryFitsAWeekGrid() {
        val m = MonthLayout.of(2026, Calendar.FEBRUARY, Calendar.SUNDAY) // 1 Feb 2026 is a Sunday, 28 days
        assertEquals(4, m.rows)
    }
}
