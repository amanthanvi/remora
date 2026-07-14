# Android Quickstart

## Prerequisites

- Java 17 or newer
- Android SDK (API 35 + build-tools 35.0.0)
- Gradle 8.x
- Rust toolchain (`rustup`)
- Android NDK (`ANDROID_NDK_HOME` or `ANDROID_NDK_ROOT`)
- `cargo-ndk` (`cargo install cargo-ndk`)
- Zig (`brew install zig`; CI pins 0.15.2)

## Build Steps

1. Build the Rust JNI bridge and debug APK:
   - `make android-emulator-fast`
2. Run unit tests:
   - `cd apps/android && ./gradlew :app:testDebugUnitTest`
3. Build only the Kotlin/Compose app against existing JNI artifacts:
   - `cd apps/android && ./gradlew :app:assembleDebug`

## Modules
- `:app`
- `:core:network`
- `:core:bridge`
- `:feature:discovery`
- `:feature:sessions`
- `:feature:conversation`
