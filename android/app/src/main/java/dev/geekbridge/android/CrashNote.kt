package dev.geekbridge.android

import android.content.Context
import java.io.File

/**
 * Records one redacted note about the last uncaught exception so Diagnostics can show it after the restart.
 * Only class names, GeekBridge/Android frames and the thread name are kept: exception messages are dropped
 * because they can echo a file name, a number or text from the phone.
 */
object CrashNote {
    private const val FILE = "last-crash.txt"
    private const val MAX_FRAMES = 8
    private const val MAX_CAUSES = 3
    private const val MAX_CHARS = 1200

    /** Pure so it is unit-tested. [versionName] and [nowMs] are passed in; nothing is read from the throwable's message. */
    fun render(t: Throwable, threadName: String, nowMs: Long, versionName: String): String = buildString {
        appendLine("crash at $nowMs on thread ${threadName.filter { it.isLetterOrDigit() || it in "-_" }.take(32)} (app $versionName)")
        var cur: Throwable? = t
        var depth = 0
        while (cur != null && depth < MAX_CAUSES) {
            appendLine((if (depth == 0) "" else "caused by ") + cur.javaClass.name)
            if (depth == 0) {
                for (f in cur.stackTrace.take(MAX_FRAMES)) appendLine("  at ${f.className}.${f.methodName}(${f.fileName ?: "?"}:${f.lineNumber})")
            }
            cur = cur.cause?.takeIf { it !== cur }
            depth++
        }
    }.take(MAX_CHARS)

    fun install(app: Context, versionName: String) {
        val previous = Thread.getDefaultUncaughtExceptionHandler()
        Thread.setDefaultUncaughtExceptionHandler { thread, error ->
            try {
                File(app.filesDir, FILE).writeText(render(error, thread.name, System.currentTimeMillis(), versionName))
            } catch (_: Throwable) {
                // Recording must never hide the original crash.
            }
            previous?.uncaughtException(thread, error)
        }
    }

    fun read(ctx: Context): String? = try {
        File(ctx.filesDir, FILE).takeIf { it.isFile }?.readText()?.takeIf { it.isNotBlank() }
    } catch (_: Exception) {
        null
    }

    fun clear(ctx: Context) {
        try { File(ctx.filesDir, FILE).delete() } catch (_: Exception) { }
    }
}
