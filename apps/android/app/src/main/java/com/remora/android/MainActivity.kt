package com.remora.android

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.animation.AnimatedVisibility
import androidx.compose.animation.fadeOut
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material3.Text
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.unit.dp
import androidx.core.splashscreen.SplashScreen.Companion.installSplashScreen
import androidx.core.view.WindowCompat
import androidx.lifecycle.lifecycleScope
import com.remora.android.state.AppLifecycleController
import com.remora.android.state.AppModel
import com.remora.android.state.OpenAIApiKeyStore
import com.remora.android.state.PetOverlayController
import com.remora.android.ui.AnimatedSplashScreen
import com.remora.android.ui.ExperimentalFeatures
import com.remora.android.ui.RemoraApp
import com.remora.android.ui.RemoraAppTheme
import com.remora.android.ui.WallpaperManager
import com.remora.android.util.LLog
import kotlinx.coroutines.delay
import kotlinx.coroutines.launch
import uniffi.codex_mobile_client.ThreadKey

class MainActivity : ComponentActivity() {
    companion object {
        const val EXTRA_OPEN_SERVER_ID = "remora.open.serverId"
        const val EXTRA_OPEN_THREAD_ID = "remora.open.threadId"
        const val EXTRA_OPEN_PET_SETTINGS = "remora.openPetSettings"
    }

    private var appModel: AppModel? = null
    private val lifecycleController = AppLifecycleController()
    private var openPetSettingsRequest by mutableStateOf(0)

    override fun onCreate(savedInstanceState: Bundle?) {
        // Must be called before super.onCreate to hand off the system splash
        // (Theme.App.Starting) to the Compose AnimatedSplashScreen without a
        // theme-background flash between them.
        installSplashScreen()
        enableEdgeToEdge()
        super.onCreate(savedInstanceState)
        WindowCompat.setDecorFitsSystemWindows(window, false)
        OpenAIApiKeyStore(applicationContext).applyToEnvironment()
        ExperimentalFeatures.initialize(applicationContext)
        PetOverlayController.initialize(applicationContext)

        try {
            appModel = AppModel.init(this)
            WallpaperManager.initialize(this)
            appModel?.start()
        } catch (e: Exception) {
            LLog.e("MainActivity", "AppModel.start() failed", e)
        }
        var showSplash by mutableStateOf(true)
        var contentReady by mutableStateOf(false)
        var minTimeElapsed by mutableStateOf(false)

        setContent {
            RemoraAppTheme {
                Box(Modifier.fillMaxSize()) {
                    val model = appModel
                    if (model != null) {
                        RemoraApp(
                            appModel = model,
                            openPetSettingsRequest = openPetSettingsRequest,
                        )
                    } else {
                        Text(
                            text = "Remora couldn't finish starting.",
                            modifier = Modifier.padding(horizontal = 24.dp),
                        )
                    }

                    // Signal content ready when RemoraApp composes
                    LaunchedEffect(model) {
                        if (model != null) {
                            contentReady = true
                        }
                    }

                    // Minimum display time
                    LaunchedEffect(Unit) {
                        delay(800)
                        minTimeElapsed = true
                    }

                    // Dismiss when both ready and min time elapsed (or hard max 3s)
                    LaunchedEffect(contentReady, minTimeElapsed) {
                        if (contentReady && minTimeElapsed) showSplash = false
                    }
                    LaunchedEffect(Unit) {
                        delay(3000)
                        showSplash = false
                    }

                    AnimatedVisibility(
                        visible = showSplash,
                        exit = fadeOut(),
                    ) {
                        AnimatedSplashScreen()
                    }
                }
            }
        }

        handleOpenThreadIntent(intent)
        consumeOverlayNavigationIntent(intent)
    }

    override fun onResume() {
        super.onResume()
        val model = appModel ?: return
        lifecycleScope.launch {
            lifecycleController.onResume(this@MainActivity, model)
            PetOverlayController.syncOverlayService(this@MainActivity)
        }
    }

    override fun onPause() {
        super.onPause()
        val model = appModel ?: return
        lifecycleController.onPause(model)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleOpenThreadIntent(intent)
        consumeOverlayNavigationIntent(intent)
    }

    override fun onDestroy() {
        appModel?.stop()
        super.onDestroy()
    }

    private fun handleOpenThreadIntent(intent: Intent?) {
        val threadKey = consumeOpenThreadKey(intent) ?: return
        val model = appModel ?: return
        lifecycleScope.launch {
            model.activateThread(threadKey)

            val resolvedKey = model.ensureThreadLoaded(threadKey) ?: threadKey
            model.activateThread(resolvedKey)
            model.refreshThreadSnapshot(resolvedKey)
        }
    }

    private fun consumeOpenThreadKey(intent: Intent?): ThreadKey? {
        intent ?: return null
        val serverId = intent.getStringExtra(EXTRA_OPEN_SERVER_ID)?.trim().orEmpty()
        val threadId = intent.getStringExtra(EXTRA_OPEN_THREAD_ID)?.trim().orEmpty()
        if (serverId.isEmpty() || threadId.isEmpty()) {
            return null
        }

        intent.removeExtra(EXTRA_OPEN_SERVER_ID)
        intent.removeExtra(EXTRA_OPEN_THREAD_ID)
        return ThreadKey(serverId = serverId, threadId = threadId)
    }

    private fun consumeOverlayNavigationIntent(intent: Intent?) {
        intent ?: return
        if (intent.getBooleanExtra(EXTRA_OPEN_PET_SETTINGS, false)) {
            openPetSettingsRequest += 1
            intent.removeExtra(EXTRA_OPEN_PET_SETTINGS)
        }
    }

}
