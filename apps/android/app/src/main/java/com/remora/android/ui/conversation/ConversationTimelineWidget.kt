package com.remora.android.ui.conversation

import android.annotation.SuppressLint
import android.content.Intent
import com.remora.android.R
import androidx.compose.foundation.ExperimentalFoundationApi
import android.graphics.BitmapFactory
import android.net.Uri
import android.util.Base64
import coil.compose.AsyncImage
import coil.request.ImageRequest
import android.webkit.WebResourceRequest
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.compose.animation.animateContentSize
import androidx.compose.animation.core.RepeatMode
import androidx.compose.animation.core.animateFloat
import androidx.compose.animation.core.infiniteRepeatable
import androidx.compose.animation.core.rememberInfiniteTransition
import androidx.compose.animation.core.tween
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.combinedClickable
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Chat
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.Error
import androidx.compose.material.icons.filled.GridView
import androidx.compose.material.icons.filled.HourglassEmpty
import androidx.compose.material.icons.filled.PhoneAndroid
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.produceState
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.alpha
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.scale
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.viewinterop.AndroidView
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.asImageBitmap
import androidx.compose.ui.layout.ContentScale
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import com.remora.android.state.SavedAppsStore
import com.remora.android.ui.BerkeleyMono
import com.remora.android.ui.LocalAppModel
import com.remora.android.ui.RemoraTextStyle
import com.remora.android.ui.RemoraTheme
import com.remora.android.ui.LocalTextScale
import com.remora.android.ui.scaled
import com.remora.android.state.AppModel
import androidx.compose.runtime.rememberCoroutineScope
import kotlinx.coroutines.launch
import org.json.JSONArray
import org.json.JSONObject
import uniffi.codex_mobile_client.AppMessageRenderBlock
import uniffi.codex_mobile_client.AppOperationStatus
import uniffi.codex_mobile_client.HydratedConversationItem
import uniffi.codex_mobile_client.HydratedConversationItemContent
import uniffi.codex_mobile_client.HydratedPlanStepStatus
import kotlin.math.roundToInt
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.withContext

@SuppressLint("SetJavaScriptEnabled")
@Composable
internal fun WidgetRow(
    data: uniffi.codex_mobile_client.HydratedWidgetData,
    originThreadId: String?,
    onOpenSavedApp: ((String) -> Unit)?,
    onWidgetPrompt: ((String) -> Unit)?,
) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    val slug = data.appId?.takeIf { it.isNotBlank() }

    // Dynamic height: seeded from the widget's declared height (same
    // `coerceIn(200, 720)` floor/ceiling as before) and then updated by the
    // shell's `_reportHeight` bridge calls once morphdom has rendered.
    val minDp = 200.dp
    val maxDp = 720.dp
    val initialDp = remember(data.height) {
        data.height.coerceIn(200.0, 720.0).roundToInt().dp
    }
    var widgetHeight by remember(minDp, maxDp) { mutableStateOf(initialDp) }
    val density = androidx.compose.ui.platform.LocalDensity.current

    // Callbacks captured once at factory time still see the latest state
    // via these rememberUpdatedState proxies.
    val currentOnWidgetPrompt by androidx.compose.runtime.rememberUpdatedState(onWidgetPrompt)
    val currentIsFinalized by androidx.compose.runtime.rememberUpdatedState(data.isFinalized)

    Column(
        modifier = Modifier
            .fillMaxWidth()
            .background(RemoraTheme.surface, RoundedCornerShape(12.dp))
            .padding(horizontal = 10.dp, vertical = 6.dp),
        verticalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Row(
            modifier = Modifier.fillMaxWidth(),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            Text(
                text = data.title.ifBlank { "Widget" },
                color = RemoraTheme.textPrimary,
                fontSize = RemoraTextStyle.footnote.scaled,
                fontWeight = FontWeight.SemiBold,
            )
            Text(
                text = data.status,
                color = statusTint(
                    when (data.status.lowercase()) {
                        "completed" -> AppOperationStatus.COMPLETED
                        "failed" -> AppOperationStatus.FAILED
                        else -> AppOperationStatus.IN_PROGRESS
                    }
                ),
                fontSize = RemoraTextStyle.caption2.scaled,
                fontWeight = FontWeight.Medium,
            )
        }

        AndroidView(
            factory = { ctx ->
                val mainHandler = android.os.Handler(android.os.Looper.getMainLooper())
                val bridge = WidgetBridge(
                    onHeight = { reportedPx ->
                        mainHandler.post {
                            val dp = with(density) { reportedPx.toDp() }
                                .coerceIn(minDp, maxDp)
                            if (kotlin.math.abs((dp - widgetHeight).value) > 1f) {
                                widgetHeight = dp
                            }
                        }
                    },
                    onSendPrompt = { text ->
                        mainHandler.post { currentOnWidgetPrompt?.invoke(text) }
                    },
                    onOpenLink = { url ->
                        mainHandler.post {
                            try {
                                ctx.startActivity(
                                    Intent(Intent.ACTION_VIEW, Uri.parse(url)),
                                )
                            } catch (_: Exception) {}
                        }
                    },
                    onReady = {
                        // `onPageFinished` also flips the ready flag; this
                        // bridge callback is informational. No-op here.
                    },
                )
                WebView(ctx).apply {
                    setBackgroundColor(android.graphics.Color.TRANSPARENT)
                    settings.javaScriptEnabled = true
                    settings.domStorageEnabled = true
                    settings.allowFileAccess = false
                    settings.allowContentAccess = false
                    settings.loadsImagesAutomatically = true
                    overScrollMode = WebView.OVER_SCROLL_NEVER
                    addJavascriptInterface(bridge, WidgetBridge.INTERFACE_NAME)
                    webViewClient = object : WebViewClient() {
                        override fun onPageFinished(view: WebView?, url: String?) {
                            super.onPageFinished(view, url)
                            if (view == null) return
                            view.setTag(R.id.widget_webview_shell_ready, true)
                            val pending = view.getTag(R.id.widget_webview_pending_html) as? String
                            if (pending != null) {
                                view.setTag(R.id.widget_webview_pending_html, null)
                                pushWidgetContent(view, pending, runScripts = currentIsFinalized)
                            }
                        }

                        override fun shouldOverrideUrlLoading(
                            view: WebView?,
                            request: WebResourceRequest?,
                        ): Boolean {
                            val url = request?.url?.toString().orEmpty()
                            if (url.isBlank() || url.startsWith("about:")) {
                                return false
                            }
                            return try {
                                ctx.startActivity(Intent(Intent.ACTION_VIEW, Uri.parse(url)))
                                true
                            } catch (_: Exception) {
                                false
                            }
                        }
                    }
                    loadDataWithBaseURL(
                        "https://widget.local/",
                        wrapWidgetHtml(""),
                        "text/html",
                        "utf-8",
                        null,
                    )
                }
            },
            modifier = Modifier
                .fillMaxWidth()
                .height(widgetHeight)
                .clip(RoundedCornerShape(10.dp)),
            update = { webView ->
                val html = data.widgetHtml
                val lastEscaped = webView.getTag(R.id.widget_webview_last_escaped) as? String
                val hasFinalized = webView.getTag(R.id.widget_webview_document) as? Boolean ?: false
                val shellReady = webView.getTag(R.id.widget_webview_shell_ready) as? Boolean ?: false
                val escaped = escapeJsString(html)
                val shouldPush = escaped != lastEscaped || (data.isFinalized && !hasFinalized)
                if (!shouldPush) return@AndroidView
                webView.setTag(R.id.widget_webview_last_escaped, escaped)
                if (data.isFinalized) {
                    webView.setTag(R.id.widget_webview_document, true)
                }
                if (!shellReady) {
                    // Store raw html — `onPageFinished` will push it through
                    // `window._setContent` once morphdom is ready.
                    webView.setTag(R.id.widget_webview_pending_html, html)
                } else {
                    pushWidgetContent(webView, html, runScripts = data.isFinalized)
                }
            },
        )

        if (slug != null && data.isFinalized && originThreadId != null) {
            SavedAsAppChip(
                slug = slug,
                onClick = {
                    scope.launch {
                        val app = try {
                            SavedAppsStore.appForSlug(context, slug, originThreadId)
                        } catch (_: Exception) { null }
                        if (app != null) {
                            onOpenSavedApp?.invoke(app.id)
                        }
                    }
                },
            )
        }
    }
}

/**
 * Push a body-HTML payload into a loaded widget shell via
 * `window._setContent(...)`. When [runScripts] is true (finalized widgets),
 * also invokes `window._runScripts()` so user `<script>` tags inside the
 * widget execute. Mirrors iOS's `coordinator.sendContent(..., runScripts:)`.
 */
internal fun pushWidgetContent(
    webView: WebView,
    html: String,
    runScripts: Boolean,
) {
    val escaped = escapeJsString(html)
    val js = if (runScripts) {
        "window._setContent('$escaped'); window._runScripts();"
    } else {
        "window._setContent('$escaped');"
    }
    webView.evaluateJavascript(js, null)
}

@Composable
private fun SavedAsAppChip(
    slug: String,
    onClick: () -> Unit,
) {
    Row(
        modifier = Modifier
            .background(
                RemoraTheme.surfaceLight.copy(alpha = 0.5f),
                RoundedCornerShape(6.dp),
            )
            .clickable(onClick = onClick)
            .padding(horizontal = 10.dp, vertical = 5.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(4.dp),
    ) {
        Icon(
            imageVector = Icons.Filled.GridView,
            contentDescription = null,
            tint = RemoraTheme.accent,
            modifier = Modifier.size(10.dp),
        )
        Text(
            text = "Saved as",
            color = RemoraTheme.accent,
            fontSize = 11.sp,
            fontWeight = FontWeight.Medium,
        )
        Text(
            text = slug,
            color = RemoraTheme.accent,
            fontSize = 11.sp,
            fontFamily = RemoraTheme.monoFont,
            fontWeight = FontWeight.SemiBold,
        )
    }
}

/**
 * Optional app-mode state injection. When provided, [wrapWidgetHtml] splices a
 * JS block that exposes `window.loadAppState()` / `window.saveAppState(obj)`
 * to the widget script. Leaving it `null` produces the plain timeline shell.
 */
data class AppStateInjection(
    val stateJson: String?,
    val schemaVersion: UInt,
)

/**
 * Timeline widget WebView shell, byte-equivalent (modulo theme tokens and
 * bridge routing) to iOS's [WidgetWebView.buildShellHTML]. The shell is
 * loaded once per WebView; content is pushed in through
 * `window._setContent(html)` / `window._runScripts()` via `evaluateJavascript`
 * — never a full reload. Height reports back through the
 * `__RemoraWidgetBridge.height` bridge; `sendPrompt` and `openLink` likewise.
 *
 * When [appState] is provided, a second inline script block is spliced before
 * `window._morphReady = false;` (matching iOS's `buildAppModeShellHTML`
 * splice point) so the saved-app JS bridge (`loadAppState`/`saveAppState`)
 * is available synchronously to user widget scripts on first render.
 */
internal fun wrapWidgetHtml(
    widgetHtml: String,
    appState: AppStateInjection? = null,
): String {
    // Note: `widgetHtml` is not spliced into `<body>` anymore. Callers drive
    // content in through `window._setContent(...)` after the shell loads —
    // see `WidgetRow`/`SavedAppScreen`. We keep the parameter for API
    // compatibility and as a one-shot initial payload via `_pending` for
    // callers who prefer the declarative path.
    val body = widgetHtml.trim()
    val initialPending = if (body.isEmpty()) "null" else "'${escapeJsString(body)}'"
    val appInjection = appState?.let { buildAppStateInjection(it) } ?: ""
    return """
        <!DOCTYPE html><html><head><meta charset="utf-8">
        <meta name="viewport" content="width=device-width,initial-scale=1.0">
        <style>
        :root {
            --color-background-primary: #000000;
            --color-background-secondary: #111111;
            --color-background-tertiary: #1a1a1a;
            --color-background-info: #0d253a;
            --color-background-danger: #3a1414;
            --color-background-success: #0d2a14;
            --color-background-warning: #3a2a0d;
            --color-text-primary: #F3F3F3;
            --color-text-secondary: #B3B3B3;
            --color-text-tertiary: #8A8A8A;
            --color-text-info: #00FF9C;
            --color-text-danger: #FF6B6B;
            --color-text-success: #00FF9C;
            --color-text-warning: #FFD166;
            --color-info: var(--color-text-info);
            --color-danger: var(--color-text-danger);
            --color-success: var(--color-text-success);
            --color-warning: var(--color-text-warning);
            --color-border-tertiary: rgba(255,255,255,0.08);
            --color-border-secondary: rgba(255,255,255,0.16);
            --color-border-primary: rgba(255,255,255,0.24);
            --color-border-info: rgba(0,255,156,0.4);
            --color-border-danger: rgba(255,107,107,0.4);
            --color-border-success: rgba(0,255,156,0.4);
            --color-border-warning: rgba(255,209,102,0.4);
            --font-sans: -apple-system, system-ui, Roboto, sans-serif;
            --font-serif: Georgia, 'Times New Roman', serif;
            --font-mono: ui-monospace, SFMono-Regular, Menlo, monospace;
            --border-radius-md: 8px;
            --border-radius-lg: 12px;
            --border-radius-xl: 16px;
            color-scheme: dark;
        }
        * { box-sizing: border-box; }
        body {
            margin: 0;
            padding: 6px;
            font-family: var(--font-sans);
            background: transparent;
            color: var(--color-text-primary);
            font-size: 14px;
            line-height: 1.5;
            -webkit-text-size-adjust: none;
        }
        @keyframes _fadeIn {
            from { opacity: 0; transform: translateY(4px); }
            to { opacity: 1; transform: none; }
        }
        svg { max-width: 100%; height: auto; }
        .t { font-family: var(--font-sans); font-size: 14px; font-weight: 400; fill: var(--color-text-primary); }
        .ts { font-family: var(--font-sans); font-size: 12px; font-weight: 400; fill: var(--color-text-secondary); }
        .th { font-family: var(--font-sans); font-size: 14px; font-weight: 500; fill: var(--color-text-primary); }
        .box { fill: var(--color-background-secondary); stroke: var(--color-border-tertiary); stroke-width: 0.5; }
        .arr { stroke: var(--color-text-tertiary); stroke-width: 1.5; fill: none; }
        .leader { stroke: var(--color-border-tertiary); stroke-width: 0.5; stroke-dasharray: 4 3; fill: none; }
        .node { cursor: pointer; }
        .node:hover { opacity: 0.85; }
        .c-blue > rect, .c-blue > circle, .c-blue > ellipse { fill: #1e3a5f; stroke: rgba(96,165,250,0.4); }
        .c-blue > .t, .c-blue > .th { fill: #93c5fd; }
        .c-blue > .ts { fill: #60a5fa; }
        .c-teal > rect, .c-teal > circle, .c-teal > ellipse { fill: #134e4a; stroke: rgba(45,212,191,0.4); }
        .c-teal > .t, .c-teal > .th { fill: #5eead4; }
        .c-teal > .ts { fill: #2dd4bf; }
        .c-amber > rect, .c-amber > circle, .c-amber > ellipse { fill: #451a03; stroke: rgba(251,191,36,0.4); }
        .c-amber > .t, .c-amber > .th { fill: #fcd34d; }
        .c-amber > .ts { fill: #fbbf24; }
        .c-green > rect, .c-green > circle, .c-green > ellipse { fill: #14532d; stroke: rgba(74,222,128,0.4); }
        .c-green > .t, .c-green > .th { fill: #86efac; }
        .c-green > .ts { fill: #4ade80; }
        .c-red > rect, .c-red > circle, .c-red > ellipse { fill: #450a0a; stroke: rgba(248,113,113,0.4); }
        .c-red > .t, .c-red > .th { fill: #fca5a5; }
        .c-red > .ts { fill: #f87171; }
        .c-purple > rect, .c-purple > circle, .c-purple > ellipse { fill: #2e1065; stroke: rgba(168,85,247,0.4); }
        .c-purple > .t, .c-purple > .th { fill: #c4b5fd; }
        .c-purple > .ts { fill: #a78bfa; }
        .c-coral > rect, .c-coral > circle, .c-coral > ellipse { fill: #431407; stroke: rgba(251,146,60,0.4); }
        .c-coral > .t, .c-coral > .th { fill: #fdba74; }
        .c-coral > .ts { fill: #fb923c; }
        .c-pink > rect, .c-pink > circle, .c-pink > ellipse { fill: #500724; stroke: rgba(244,114,182,0.4); }
        .c-pink > .t, .c-pink > .th { fill: #f9a8d4; }
        .c-pink > .ts { fill: #f472b6; }
        .c-gray > rect, .c-gray > circle, .c-gray > ellipse { fill: var(--color-background-tertiary); stroke: var(--color-border-secondary); }
        .c-gray > .t, .c-gray > .th { fill: var(--color-text-primary); }
        .c-gray > .ts { fill: var(--color-text-secondary); }
        </style>
        </head><body><div id="root"></div>
        <script>
        // Shared message router. Defined before any other shell script (and
        // before the optional app-mode injection) so both paths can call it
        // synchronously. Routes {_type,...} payloads through whichever
        // JS-to-native bridge is present:
        //   - __RemoraAppBridge: saved-app saveAppState channel.
        //   - __RemoraWidgetBridge: height / sendPrompt / openLink / ready.
        //   - webkit.messageHandlers.widget: iOS fallback (no-op on Android).
        function __postWidgetMessage(msg) {
            try {
                if (msg && msg._type === 'saveAppState'
                    && window.__RemoraAppBridge
                    && typeof window.__RemoraAppBridge.saveAppState === 'function') {
                    window.__RemoraAppBridge.saveAppState(msg.value, msg.schema|0);
                    return true;
                }
                if (msg && msg._type === 'structuredResponse'
                    && window.__RemoraAppBridge
                    && typeof window.__RemoraAppBridge.structuredResponse === 'function') {
                    // Android @JavascriptInterface only accepts primitives —
                    // stringify the schema object before handing it off. iOS
                    // goes through postMessage and can pass the object as-is.
                    var schemaStr;
                    try { schemaStr = JSON.stringify(msg.responseFormat); }
                    catch (_) { schemaStr = 'null'; }
                    window.__RemoraAppBridge.structuredResponse(
                        String(msg.requestId || ''),
                        String(msg.prompt || ''),
                        schemaStr
                    );
                    return true;
                }
                if (window.__RemoraWidgetBridge) {
                    var b = window.__RemoraWidgetBridge;
                    if (msg._type === 'height' && typeof b.height === 'function') {
                        b.height(msg.value|0);
                        return true;
                    }
                    if (msg._type === 'sendPrompt' && typeof b.sendPrompt === 'function') {
                        b.sendPrompt(String(msg.text || ''));
                        return true;
                    }
                    if (msg._type === 'openLink' && typeof b.openLink === 'function') {
                        b.openLink(String(msg.url || ''));
                        return true;
                    }
                    if (msg._type === 'ready' && typeof b.ready === 'function') {
                        b.ready();
                        return true;
                    }
                }
                if (window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.widget) {
                    window.webkit.messageHandlers.widget.postMessage(msg);
                    return true;
                }
            } catch (_) {}
            return false;
        }
        </script>
        $appInjection
        <script>
        window._morphReady = false;
        window._pending = $initialPending;
        window._lastHeight = 0;
        window._heightObserver = null;
        window._reportHeight = function() {
            var r = document.getElementById('root');
            if (!r) return;
            var next = Math.ceil(Math.max(r.offsetHeight, r.scrollHeight)) + 12;
            if (!next || Math.abs(next - window._lastHeight) < 1) return;
            window._lastHeight = next;
            __postWidgetMessage({_type:'height', value: next});
        };
        window._attachHeightObserver = function() {
            var r = document.getElementById('root');
            if (!r || window._heightObserver) return;
            window._heightObserver = new ResizeObserver(function() {
                window._reportHeight();
            });
            window._heightObserver.observe(r);
        };
        window._setContent = function(html) {
            var root = document.getElementById('root');
            if (!root) return;
            if (!window._morphReady || typeof morphdom !== 'function') {
                try { root.innerHTML = html; } catch (_) {}
                window._attachHeightObserver();
                setTimeout(function() {
                    window._reportHeight();
                }, 60);
                return;
            }
            var target = document.createElement('div');
            target.id = 'root';
            // Tolerate mid-stream HTML: unclosed tags or half-parsed
            // attributes must not blow up morphdom. Fall back to innerHTML
            // replacement; the next delta that closes the tag will
            // re-diff cleanly.
            try {
                target.innerHTML = html;
                morphdom(root, target, {
                    onBeforeElUpdated: function(from, to) {
                        if (from.isEqualNode(to)) return false;
                        return true;
                    },
                    onNodeAdded: function(node) {
                        if (node.nodeType === 1 && node.tagName !== 'STYLE' && node.tagName !== 'SCRIPT') {
                            node.style.animation = '_fadeIn 0.3s ease both';
                        }
                        return node;
                    }
                });
            } catch (e) {
                try { root.innerHTML = html; } catch (_) {}
            }
            window._attachHeightObserver();
            setTimeout(function() {
                window._reportHeight();
            }, 60);
        };
        window._runScripts = function() {
            document.querySelectorAll('#root script').forEach(function(old) {
                var s = document.createElement('script');
                if (old.src) { s.src = old.src; } else { s.textContent = old.textContent; }
                old.parentNode.replaceChild(s, old);
            });
            window._attachHeightObserver();
            setTimeout(function() {
                window._reportHeight();
            }, 250);
        };
        window.sendPrompt = function(text) {
            __postWidgetMessage({_type:'sendPrompt', text: text});
        };
        window.openLink = function(url) {
            __postWidgetMessage({_type:'openLink', url: url});
        };
        </script>
        <script src="https://cdn.jsdelivr.net/npm/morphdom@2.7.4/dist/morphdom-umd.min.js"
            onload="window._morphReady=true;if(window._pending){window._setContent(window._pending);window._pending=null;}__postWidgetMessage({_type:'ready'});"></script>
        </body></html>
    """.trimIndent()
}

/**
 * JS block providing the `loadAppState` / `saveAppState` bridge consumed by
 * saved-app-mode widgets. Spliced into the shell before
 * `window._morphReady = false;` (mirrors iOS's `buildAppModeShellHTML`) so
 * user widget scripts can call the bridge synchronously during first render.
 *
 * The raw state JSON is sanitized by replacing `</` with `<\/` before being
 * spliced into the inline script to prevent a stray `</script>` sequence in
 * user data from closing our tag.
 */
private fun buildAppStateInjection(appState: AppStateInjection): String {
    val escapedJson = appState.stateJson
        ?.let { org.json.JSONObject.quote(it) }
        ?.replace("</", "<\\/")
        ?: "null"
    val schema = appState.schemaVersion.toLong()
    return """
        <script>
          window._initialAppState = $escapedJson;
          window._appStateSchemaVersion = $schema;
          window.loadAppState = function() {
            try {
              if (window._initialAppState == null) return null;
              return JSON.parse(window._initialAppState);
            } catch (_) { return null; }
          };
          window.saveAppState = function(obj) {
            var payload;
            try { payload = JSON.stringify(obj); } catch (_) { return false; }
            return __postWidgetMessage({
              _type: 'saveAppState',
              value: payload,
              schema: window._appStateSchemaVersion,
            });
          };
          (function(){
            var nextId = 1;
            var pending = new Map();
            window.structuredResponse = function(req) {
              var id = 'sr-' + (nextId++);
              return new Promise(function(resolve, reject) {
                pending.set(id, { resolve: resolve, reject: reject });
                var fmt = (req && req.responseFormat) || null;
                __postWidgetMessage({
                  _type: 'structuredResponse',
                  requestId: id,
                  prompt: String((req && req.prompt) || ''),
                  responseFormat: fmt,
                });
              });
            };
            window.__resolveStructuredResponse = function(id, jsonText) {
              var p = pending.get(id); if (!p) return;
              pending.delete(id);
              try { p.resolve(JSON.parse(jsonText)); }
              catch (e) { p.reject(new Error('invalid structured response JSON: ' + (e && e.message))); }
            };
            window.__rejectStructuredResponse = function(id, message) {
              var p = pending.get(id); if (!p) return;
              pending.delete(id);
              p.reject(new Error(message || 'structuredResponse failed'));
            };
          })();
        </script>
    """.trimIndent()
}

/**
 * Escape a string so it can be embedded as a JS single-quoted literal inside
 * `evaluateJavascript("window._setContent('...'); ...")`. Matches iOS's
 * `WidgetWebView.escapeJS` character-for-character.
 */
internal fun escapeJsString(s: String): String =
    s.replace("\\", "\\\\")
        .replace("'", "\\'")
        .replace("\n", "\\n")
        .replace("\r", "\\r")
        .replace("</script>", "<\\/script>")

internal fun timelineWorkspaceTitle(path: String): String {
    return path
        .trimEnd('/')
        .substringAfterLast('/')
        .ifBlank { path }
}
