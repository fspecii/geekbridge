// SPDX-License-Identifier: GPL-3.0-or-later
package dev.geekbridge.android

import org.json.JSONException
import org.json.JSONObject

/** What came back from one native request. None of these imply paired, trusted or call-ready. */
sealed interface CoreOutcome {
    /** `ok: true`. [result] is whatever the core returned (usually a JSONObject). */
    data class Ok(val result: Any?) : CoreOutcome

    /** `ok: false`, or a limit the app itself enforced before calling the core ([local] true). */
    data class Rejected(val code: String, val message: String, val local: Boolean = false) : CoreOutcome

    /** The core could not be called at all (library missing, symbol missing, call failed). */
    data class Unavailable(val reason: String) : CoreOutcome

    /** The core answered, but not with the documented envelope. */
    data class Malformed(val reason: String) : CoreOutcome
}

/** Request/response envelope from docs/android-contract.md (`version: 1`). */
object CoreProtocol {
    const val VERSION = 1

    private val codePattern = Regex("[A-Za-z0-9_.-]{1,48}")

    fun versionRequest(): String = JSONObject()
        .put("version", VERSION)
        .put("action", "version")
        .toString()

    fun validateSmsRequest(recipient: String, text: String, subscriptionId: Int?): String {
        val o = JSONObject()
            .put("version", VERSION)
            .put("action", "validate_sms")
            .put("recipient", recipient)
            .put("text", text)
        if (subscriptionId != null) o.put("subscription_id", subscriptionId)
        return o.toString()
    }

    fun validateClipboardRequest(text: String): String = JSONObject()
        .put("version", VERSION)
        .put("action", "validate_clipboard")
        .put("text", text)
        .toString()

    /** Strictly parses a native response. Never throws; never echoes unbounded native text. */
    fun parseResponse(raw: String?, maxChars: Int = Limits.MAX_RESPONSE_CHARS): CoreOutcome {
        if (raw == null) return CoreOutcome.Malformed("empty response")
        if (raw.length > maxChars) return CoreOutcome.Malformed("response too large")
        val root = try {
            JSONObject(raw)
        } catch (e: JSONException) {
            return CoreOutcome.Malformed("response is not a JSON object")
        }
        if ((root.opt("version") as? Number)?.toInt() != VERSION) {
            return CoreOutcome.Malformed("unsupported response version")
        }
        return when (root.opt("ok")) {
            true -> CoreOutcome.Ok(root.opt("result").takeUnless { it === JSONObject.NULL })
            false -> {
                val err = root.optJSONObject("error")
                    ?: return CoreOutcome.Malformed("error response without error object")
                val code = err.optString("code", "")
                if (!codePattern.matches(code)) return CoreOutcome.Malformed("error response with invalid code")
                CoreOutcome.Rejected(code, Bounds.clip(err.optString("message", ""), Limits.MAX_ERROR_MESSAGE_CHARS))
            }
            else -> CoreOutcome.Malformed("response without boolean ok")
        }
    }

    /** Best-effort display of the core version from a `version` result; null if not recognisable. */
    fun coreVersionOf(result: Any?): String? {
        val o = result as? JSONObject ?: return null
        for (key in listOf("core_version", "version", "crate_version", "rust_core_version")) {
            val v = o.opt(key)
            if (v is String && v.isNotBlank()) return Bounds.clip(v, 32)
        }
        return null
    }

    /** Bounded one-line rendering of a (non-user-content) `version` result for diagnostics. */
    fun summarize(result: Any?, max: Int = 300): String = Bounds.clip(result?.toString() ?: "", max)
}

/**
 * Calls the core through [transport] (normally [NativeBridge.nativeRequest]) and turns every failure
 * mode into a [CoreOutcome]. Must be called off the main thread.
 */
class CoreClient(
    private val isAvailable: () -> Boolean,
    private val unavailableReason: () -> String?,
    private val transport: (String) -> String,
) {
    fun version(): CoreOutcome = call(CoreProtocol.versionRequest())

    fun validateSms(recipient: String, text: String, subscriptionId: Int?): CoreOutcome {
        if (recipient.length > Limits.SMS_MAX_ADDRESS_CHARS * 4 || text.length > Limits.SMS_MAX_TEXT_CHARS * 4) {
            return CoreOutcome.Rejected("input_too_large", "Input is far over the allowed size.", local = true)
        }
        return call(CoreProtocol.validateSmsRequest(recipient, text, subscriptionId))
    }

    fun validateClipboard(text: String): CoreOutcome {
        // Cheap guard so a huge paste never becomes a huge JNI string. The core stays authoritative
        // for the exact rules (empty, NUL, size).
        if (Bounds.utf8Size(text) > Limits.CLIPBOARD_MAX_BYTES) {
            return CoreOutcome.Rejected(
                "input_too_large",
                "Clipboard text is over ${Limits.CLIPBOARD_MAX_BYTES / 1024} KiB.",
                local = true,
            )
        }
        return call(CoreProtocol.validateClipboardRequest(text))
    }

    /** Any `transport_*` request (see [TransportProtocol]); their status snapshots may be larger than other replies. */
    fun transportAction(request: String): CoreOutcome = call(request, Limits.MAX_TRANSPORT_RESPONSE_CHARS)

    /** `pairing_decode_qr`: one camera picture, far over the usual request cap but bounded by the core (docs/android-contract.md). */
    fun decodeQr(request: String): CoreOutcome = call(request, maxRequestChars = Limits.MAX_DATA_REQUEST_CHARS)

    /** `transport_clipboard_*` requests; replies may carry several clipboard items. */
    fun clipboardAction(request: String): CoreOutcome = call(request, Limits.MAX_CLIPBOARD_RESPONSE_CHARS)

    /** `transport_data_*` requests (docs/phone-data-link.md). Only `transport_data_send` may exceed the usual request cap. */
    fun dataAction(request: String): CoreOutcome =
        call(request, Limits.MAX_CLIPBOARD_RESPONSE_CHARS, maxRequestChars = Limits.MAX_DATA_REQUEST_CHARS)

    private fun call(
        request: String,
        maxResponseChars: Int = Limits.MAX_RESPONSE_CHARS,
        maxRequestChars: Int = Limits.MAX_REQUEST_CHARS,
    ): CoreOutcome {
        if (!isAvailable()) {
            return CoreOutcome.Unavailable(unavailableReason() ?: "native core not loaded")
        }
        if (request.length > maxRequestChars) {
            return CoreOutcome.Rejected("input_too_large", "Request is over the allowed size.", local = true)
        }
        // Reasons below name only the exception class, never its message: messages could echo input.
        return try {
            CoreProtocol.parseResponse(transport(request), maxResponseChars)
        } catch (e: LinkageError) {
            CoreOutcome.Unavailable("native call failed (${e.javaClass.simpleName})")
        } catch (e: RuntimeException) {
            CoreOutcome.Unavailable("native call failed (${e.javaClass.simpleName})")
        }
    }

    companion object {
        fun native(): CoreClient = CoreClient(
            isAvailable = { NativeBridge.isLoaded },
            unavailableReason = { NativeBridge.loadFailure },
            transport = NativeBridge::nativeRequest,
        )
    }
}
