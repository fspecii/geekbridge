// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class SmsManagerLookupTest {
    @Test fun classLookupOnlyFromAndroid12() {
        // API 26-30 return null from getSystemService(SmsManager::class.java); those use SmsManager.getDefault().
        for (sdk in 26..30) assertFalse("API $sdk", smsManagerIsSystemService(sdk))
        for (sdk in 31..36) assertTrue("API $sdk", smsManagerIsSystemService(sdk))
    }
}
