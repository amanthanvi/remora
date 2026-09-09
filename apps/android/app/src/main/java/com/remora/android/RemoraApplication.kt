package com.remora.android

import android.app.Application
import com.remora.android.state.CurrentSecurityCutover
import com.remora.android.state.AndroidRelayRuntime
import com.remora.android.util.LLog

/**
 * Process-wide security preflight for every Android component entry point.
 *
 * AppModel.init repeats this gate so a failed cutover remains fail-closed even
 * when Android starts a receiver, widget, or future component before an
 * activity.
 */
class RemoraApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        if (!CurrentSecurityCutover.apply(this)) {
            LLog.e("RemoraApplication", "Remora 1.6 security cutover did not complete")
        } else {
            AndroidRelayRuntime.start(this)
        }
    }
}
