package com.remora.android.util

import android.content.Context
import android.util.Log
import com.remora.android.BuildConfig
import com.remora.android.core.bridge.UniffiInit
import org.json.JSONObject

object LLog {
    internal fun interface Sink {
        fun write(priority: Int, tag: String, message: String, throwable: Throwable?)
    }

    private object AndroidLogSink : Sink {
        override fun write(priority: Int, tag: String, message: String, throwable: Throwable?) {
            when (priority) {
                Log.VERBOSE -> Log.v(tag, message, throwable)
                Log.DEBUG -> Log.d(tag, message, throwable)
                Log.INFO -> Log.i(tag, message, throwable)
                Log.WARN -> Log.w(tag, message, throwable)
                Log.ERROR -> Log.e(tag, message, throwable)
                else -> Log.println(priority, tag, message)
            }
        }
    }

    @Volatile private var bootstrapped = false
    @Volatile private var sink: Sink = AndroidLogSink

    internal fun setSinkForTesting(replacement: Sink?) {
        sink = replacement ?: AndroidLogSink
    }

    fun bootstrap(context: Context) {
        if (bootstrapped) return
        synchronized(this) {
            if (bootstrapped) return
            UniffiInit.ensure(context)
            bootstrapped = true
        }
    }

    fun t(tag: String, message: String, fields: Map<String, Any?> = emptyMap(), payloadJson: String? = null) {
        if (!BuildConfig.DEBUG) return
        sink.write(Log.VERBOSE, tag, render(message, fields, payloadJson), null)
    }

    fun d(tag: String, message: String, fields: Map<String, Any?> = emptyMap(), payloadJson: String? = null) {
        if (!BuildConfig.DEBUG) return
        sink.write(Log.DEBUG, tag, render(message, fields, payloadJson), null)
    }

    inline fun debug(tag: String, message: () -> String) {
        if (BuildConfig.DEBUG) {
            emitDebug(tag, message(), null)
        }
    }

    inline fun debug(tag: String, throwable: Throwable, message: () -> String) {
        if (BuildConfig.DEBUG) {
            emitDebug(tag, message(), throwable)
        }
    }

    @PublishedApi
    internal fun emitDebug(tag: String, message: String, throwable: Throwable?) {
        sink.write(Log.DEBUG, tag, message, throwable)
    }

    fun i(tag: String, message: String, fields: Map<String, Any?> = emptyMap(), payloadJson: String? = null) {
        sink.write(Log.INFO, tag, render(message, fields, payloadJson), null)
    }

    fun w(tag: String, message: String, fields: Map<String, Any?> = emptyMap(), payloadJson: String? = null) {
        sink.write(Log.WARN, tag, render(message, fields, payloadJson), null)
    }

    fun e(
        tag: String,
        message: String,
        throwable: Throwable? = null,
        fields: Map<String, Any?> = emptyMap(),
        payloadJson: String? = null,
    ) {
        val mergedFields = fields.toMutableMap()
        if (throwable != null) {
            if (BuildConfig.DEBUG) {
                mergedFields["error"] = throwable.message ?: throwable.javaClass.simpleName
            } else {
                mergedFields.putIfAbsent("errorType", throwable.javaClass.simpleName)
            }
        }

        val rendered = render(message, mergedFields, payloadJson)
        sink.write(Log.ERROR, tag, rendered, throwable.takeIf { BuildConfig.DEBUG })
    }

    private fun render(message: String, fields: Map<String, Any?>, payloadJson: String?): String {
        val safeMessage = if (BuildConfig.DEBUG) message else releaseSafeMessage(message)
        val parts = mutableListOf(safeMessage)
        val renderedFields = if (BuildConfig.DEBUG) fields else releaseSafeFields(fields)
        fieldsJson(renderedFields)?.let { parts += "fields=$it" }
        if (BuildConfig.DEBUG) {
            payloadJson?.takeIf { it.isNotBlank() }?.let { parts += "payload=$it" }
        }
        return parts.joinToString(separator = " ")
    }

    private fun releaseSafeMessage(message: String): String {
        val singleLine = message.lineSequence().firstOrNull().orEmpty().take(MAX_RELEASE_MESSAGE_LENGTH)
        val detailSeparator = listOf(
            singleLine.indexOf(": "),
            singleLine.indexOf(" = "),
        ).filter { it >= 0 }.minOrNull()
        return detailSeparator?.let { separator -> singleLine.substring(0, separator) } ?: singleLine
    }

    private fun releaseSafeFields(fields: Map<String, Any?>): Map<String, Any?> =
        buildMap {
            releaseIdentifier(fields["operation"])?.let { put("operation", it) }
            releaseIdentifier(fields["errorType"])?.let { put("errorType", it) }
            releaseIdentifier(fields["errorDomain"])?.let { put("errorDomain", it) }
            releaseInteger(fields["errorCode"])?.let { put("errorCode", it) }
            releaseInteger(fields["status"])?.let { put("status", it) }
            releaseInteger(fields["count"])?.let { put("count", it) }
            releaseInteger(fields["attempt"])?.let { put("attempt", it) }
            releaseInteger(fields["loaded"])?.let { put("loaded", it) }
        }

    private fun releaseIdentifier(value: Any?): String? =
        (value as? String)
            ?.takeIf { candidate ->
                candidate.isNotEmpty() &&
                    candidate.length <= MAX_RELEASE_IDENTIFIER_LENGTH &&
                    candidate.all { it.isLetterOrDigit() || it in "._-" }
            }

    private fun releaseInteger(value: Any?): Long? = when (value) {
        is Byte -> value.toLong()
        is Short -> value.toLong()
        is Int -> value.toLong()
        is Long -> value
        else -> null
    }

    private fun fieldsJson(fields: Map<String, Any?>): String? {
        if (fields.isEmpty()) return null
        val filtered = fields.filterValues { it != null }
        if (filtered.isEmpty()) return null
        return JSONObject(filtered).toString()
    }

    private const val MAX_RELEASE_MESSAGE_LENGTH = 160
    private const val MAX_RELEASE_IDENTIFIER_LENGTH = 80
}
