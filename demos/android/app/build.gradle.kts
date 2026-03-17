plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
}

android {
    namespace = "com.n0.irohlive.demo"
    compileSdk = 35

    defaultConfig {
        applicationId = "com.n0.irohlive.demo"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.1.0"

        ndk {
            // Build the Rust cdylib for these ABIs.
            abiFilters += listOf("arm64-v8a")
        }
    }

    buildTypes {
        release {
            isMinifyEnabled = false
            // Use the debug signing key so the release APK can be installed
            // directly on device without setting up a keystore.
            signingConfig = signingConfigs.getByName("debug")
        }
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    kotlinOptions {
        jvmTarget = "17"
    }
}

dependencies {
    implementation(libs.core.ktx)
    implementation(libs.appcompat)
    implementation(libs.activity.ktx)
    implementation(libs.lifecycle.runtime)
    implementation(libs.coroutines.android)
    implementation(libs.camerax.core)
    implementation(libs.camerax.camera2)
    implementation(libs.camerax.lifecycle)
    implementation(libs.camerax.view)
    implementation(libs.zxing.embedded)
}

// cargo-ndk integration: builds the Rust cdylib before assembling the APK.
// Install with: cargo install cargo-ndk
// Then run: ./gradlew assembleDebug
//
// The task below is a placeholder. For a real build, use the
// org.niclas-van-eyk.cargo-ndk Gradle plugin or wire up a custom Exec task
// that calls: cargo ndk -t arm64-v8a -t x86_64 -o app/src/main/jniLibs build -p iroh-live-android
