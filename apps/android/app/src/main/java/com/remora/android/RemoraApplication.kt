package com.remora.android

import android.app.Application
import com.remora.android.state.LegacyV1SecretPurge

class RemoraApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        LegacyV1SecretPurge.purge(this)
    }
}
