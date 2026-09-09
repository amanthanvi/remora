import org.jetbrains.kotlin.gradle.dsl.JvmTarget

plugins {
    id("com.android.library")
}

fun String.asBuildFlag(): Boolean =
    equals("1") || equals("true", ignoreCase = true) || equals("yes", ignoreCase = true)

val androidAbis = System.getenv("ANDROID_ABIS")
    ?.split(",")
    ?.map { it.trim() }
    ?.filter { it.isNotBlank() }
    ?: listOf("arm64-v8a", "x86_64")

val ghosttyHeader = file("src/main/cpp/include/ghostty.h")
val ghosttyLibrariesAvailable = ghosttyHeader.isFile &&
    androidAbis.all { abi -> file("src/main/jniLibs/$abi/libghostty.so").isFile }
val enableGhosttyJni = System.getenv("REMORA_ENABLE_GHOSTTY_ANDROID")?.asBuildFlag()
    ?: (findProperty("remora.enableGhosttyAndroid") as? String)?.asBuildFlag()
    ?: ghosttyLibrariesAvailable

android {
    namespace = "com.remora.android.core.bridge"
    compileSdk = 37
    ndkVersion = System.getenv("ANDROID_NDK_VERSION")?.takeIf { it.isNotBlank() } ?: "30.0.14904198"

    defaultConfig {
        minSdk = 26
        consumerProguardFiles("consumer-rules.pro")

        ndk {
            abiFilters += androidAbis
        }
    }

    sourceSets {
        getByName("main") {
            jniLibs.directories += "src/main/jniLibs"
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    if (enableGhosttyJni) {
        externalNativeBuild {
            cmake {
                path = file("src/main/cpp/CMakeLists.txt")
            }
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
    }
}

dependencies {
    implementation("androidx.core:core-ktx:1.19.0")
    implementation("androidx.security:security-crypto:1.1.0")
    implementation("org.jetbrains.kotlinx:kotlinx-coroutines-android:1.11.0")
    // 5.17 completes Android 16 KiB page-size support in jnidispatch.
    api("net.java.dev.jna:jna:5.17.0@aar")
}
