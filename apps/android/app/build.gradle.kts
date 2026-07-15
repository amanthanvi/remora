plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
    id("org.jetbrains.kotlin.plugin.compose") version "2.0.21"
}

fun projectPropOrEnv(name: String): String? =
    (findProperty(name) as? String)?.takeIf { it.isNotBlank() }
        ?: System.getenv(name)?.takeIf { it.isNotBlank() }

val firebaseResourceValues = linkedMapOf(
    "google_app_id" to projectPropOrEnv("REMORA_FIREBASE_APP_ID"),
    "project_id" to projectPropOrEnv("REMORA_FIREBASE_PROJECT_ID"),
    "google_api_key" to projectPropOrEnv("REMORA_FIREBASE_API_KEY"),
    "gcm_defaultSenderId" to projectPropOrEnv("REMORA_FIREBASE_SENDER_ID"),
)
val hasAnyFirebaseConfiguration = firebaseResourceValues.values.any { it != null }
val hasCompleteFirebaseConfiguration = firebaseResourceValues.values.all { it != null }
check(!hasAnyFirebaseConfiguration || hasCompleteFirebaseConfiguration) {
    "FCM configuration is partial; set all REMORA_FIREBASE_APP_ID, " +
        "REMORA_FIREBASE_PROJECT_ID, REMORA_FIREBASE_API_KEY, and " +
        "REMORA_FIREBASE_SENDER_ID values"
}
val releaseBuildRequested = gradle.startParameter.taskNames.any { taskName ->
    taskName.contains("release", ignoreCase = true)
}
check(!releaseBuildRequested || hasCompleteFirebaseConfiguration) {
    "Release builds require the complete REMORA_FIREBASE_* resource injection; " +
        "refusing to ship inert background awareness"
}

android {
    namespace = "com.remora.android"
    compileSdk = 35
    ndkVersion = projectPropOrEnv("ANDROID_NDK_VERSION") ?: "30.0.14904198"

    defaultConfig {
        applicationId = "com.remora.android"
        minSdk = 26
        targetSdk = 35
        versionCode = 11
        versionName = "1.5.0"
        buildConfigField("boolean", "ENABLE_ON_DEVICE_BRIDGE", "true")
        buildConfigField("String", "RUNTIME_STARTUP_MODE", "\"hybrid\"")
        buildConfigField("String", "APP_RUNTIME_TRANSPORT", "\"app_bridge_rpc_transport\"")
        buildConfigField(
            "boolean",
            "BACKGROUND_AWARENESS_CONFIGURED",
            hasCompleteFirebaseConfiguration.toString(),
        )
        if (hasCompleteFirebaseConfiguration) {
            firebaseResourceValues.forEach { (resourceName, value) ->
                resValue("string", resourceName, checkNotNull(value))
            }
        }
        manifestPlaceholders["runtimeStartupMode"] = "hybrid"
        manifestPlaceholders["enableOnDeviceBridge"] = "true"
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
            ndk {
                debugSymbolLevel = "NONE"
            }
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }

    buildFeatures {
        compose = true
        buildConfig = true
    }

    sourceSets {
        getByName("main") {
            java.srcDir("../../../shared/rust-bridge/generated/kotlin")
            assets.srcDir("../../ios/Sources/Remora/Resources/Themes")
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

dependencies {
    implementation(project(":core:bridge"))

    implementation("androidx.core:core-ktx:1.13.1")
    implementation("androidx.core:core-splashscreen:1.0.1")
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.browser:browser:1.8.0")
    implementation("com.google.android.material:material:1.12.0")
    implementation(platform("androidx.compose:compose-bom:2024.09.00"))
    implementation("androidx.activity:activity-compose:1.9.2")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.8.6")
    implementation("androidx.lifecycle:lifecycle-service:2.8.6")
    implementation("androidx.work:work-runtime-ktx:2.11.2")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.8.1")
    implementation("io.coil-kt:coil-compose:2.7.0")
    implementation("io.noties.markwon:core:4.6.2")
    implementation("io.noties.markwon:ext-latex:4.6.2")
    implementation("io.noties.markwon:inline-parser:4.6.2")
    implementation("io.noties.markwon:syntax-highlight:4.6.2") {
        exclude(group = "org.jetbrains", module = "annotations-java5")
    }
    implementation("io.noties:prism4j:2.0.0") {
        exclude(group = "org.jetbrains", module = "annotations-java5")
    }
    implementation("androidx.security:security-crypto:1.1.0-alpha06")
    implementation("com.google.code.gson:gson:2.8.9")
    implementation("com.android.billingclient:billing-ktx:7.0.0")

    // FCM is an opaque wake transport only. Firebase project configuration is
    // injected through REMORA_FIREBASE_* Gradle properties/environment values
    // by the signed deployment; no provider credentials live here.
    implementation(platform("com.google.firebase:firebase-bom:34.15.0"))
    implementation("com.google.firebase:firebase-messaging")

    implementation("androidx.media3:media3-exoplayer:1.4.1")
    implementation("androidx.media3:media3-ui:1.4.1")
    implementation("androidx.media3:media3-transformer:1.4.1")

    implementation("io.github.webrtc-sdk:android:144.7559.04")

    // Alleycat remote-host pairing QR scanner
    implementation("androidx.camera:camera-core:1.3.4")
    implementation("androidx.camera:camera-camera2:1.3.4")
    implementation("androidx.camera:camera-lifecycle:1.3.4")
    implementation("androidx.camera:camera-view:1.3.4")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    implementation("androidx.glance:glance-appwidget:1.1.0")
    implementation("androidx.glance:glance-material3:1.1.0")

    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20240303")
    androidTestImplementation("androidx.test.ext:junit:1.2.1")
    androidTestImplementation("androidx.test:runner:1.6.2")
    androidTestImplementation("androidx.test:rules:1.6.1")
    androidTestImplementation(platform("androidx.compose:compose-bom:2024.09.00"))
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}
