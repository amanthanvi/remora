import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.plugin.compose")
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
val validateReleaseFirebaseConfiguration = tasks.register("validateReleaseFirebaseConfiguration") {
    inputs.property("firebaseConfigured", hasCompleteFirebaseConfiguration)
    doLast {
        check(inputs.properties["firebaseConfigured"] == true) {
            "Release builds require the complete REMORA_FIREBASE_* resource injection"
        }
    }
}
tasks.matching { it.name == "preReleaseBuild" }.configureEach {
    dependsOn(validateReleaseFirebaseConfiguration)
}

android {
    namespace = "com.remora.android"
    compileSdk = 37
    ndkVersion = projectPropOrEnv("ANDROID_NDK_VERSION") ?: "30.0.14904198"

    defaultConfig {
        applicationId = "com.remora.android"
        minSdk = 26
        targetSdk = 36
        versionCode = 12
        versionName = "1.6.0"
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

    buildFeatures {
        compose = true
        buildConfig = true
        resValues = true
    }

    sourceSets {
        getByName("main") {
            kotlin.directories += "../../../shared/rust-bridge/generated/kotlin"
            assets.directories += "../../ios/Sources/Remora/Resources/Themes"
        }
    }

    packaging {
        jniLibs {
            useLegacyPackaging = true
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    val composeBom = platform("androidx.compose:compose-bom:2026.08.00")
    implementation(project(":core:bridge"))

    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.core:core-splashscreen:1.2.0")
    implementation("androidx.appcompat:appcompat:1.8.0")
    implementation("androidx.browser:browser:1.10.0")
    implementation("com.google.android.material:material:1.14.0")
    implementation(composeBom)
    implementation("androidx.activity:activity-compose:1.13.0")
    implementation("androidx.compose.ui:ui")
    implementation("androidx.compose.ui:ui-tooling-preview")
    implementation("androidx.compose.foundation:foundation")
    implementation("androidx.compose.material3:material3")
    implementation("androidx.compose.material:material-icons-extended")
    implementation("androidx.lifecycle:lifecycle-runtime-compose:2.11.0")
    implementation("androidx.work:work-runtime-ktx:2.11.2")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
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
    implementation("androidx.security:security-crypto:1.1.0")
    implementation("com.google.code.gson:gson:2.14.0")
    implementation("com.android.billingclient:billing-ktx:9.1.0")

    // FCM is an opaque wake transport only. Firebase project configuration is
    // injected through REMORA_FIREBASE_* Gradle properties/environment values
    // by the signed deployment; no provider credentials live here.
    implementation(platform("com.google.firebase:firebase-bom:34.18.0"))
    implementation("com.google.firebase:firebase-messaging")

    implementation("androidx.media3:media3-exoplayer:1.11.0")
    implementation("androidx.media3:media3-ui:1.11.0")

    implementation("io.github.webrtc-sdk:android:150.7871.01")

    // Remora Link remote-host pairing QR scanner
    implementation("androidx.camera:camera-core:1.6.2")
    implementation("androidx.camera:camera-camera2:1.6.2")
    implementation("androidx.camera:camera-lifecycle:1.6.2")
    implementation("androidx.camera:camera-view:1.6.2")
    implementation("com.google.mlkit:barcode-scanning:17.3.0")

    implementation("androidx.glance:glance-appwidget:1.2.0")
    implementation("androidx.glance:glance-material3:1.2.0")

    debugImplementation("androidx.compose.ui:ui-tooling")
    debugImplementation("androidx.compose.ui:ui-test-manifest")
    testImplementation("junit:junit:4.13.2")
    testImplementation("org.json:json:20260814")
    androidTestImplementation("androidx.test.ext:junit:1.3.0")
    androidTestImplementation("androidx.test:runner:1.7.0")
    androidTestImplementation("androidx.test:rules:1.7.0")
    androidTestImplementation(composeBom)
    androidTestImplementation("androidx.compose.ui:ui-test-junit4")
}
