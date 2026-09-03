plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.android)
    alias(libs.plugins.kotlin.compose)
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
            // Package the Rust cdylib for these ABIs. arm64-v8a is a handset,
            // x86_64 is the emulator. Whichever `.so` files are present under
            // `src/main/jniLibs` get packaged; a missing ABI is not an error, so
            // building one of the two is enough for a local run.
            abiFilters += listOf("arm64-v8a", "x86_64")
        }
    }

    buildFeatures {
        compose = true
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
    implementation(libs.lifecycle.runtime)
    implementation(libs.coroutines.android)

    implementation(libs.activity.compose)
    implementation(platform(libs.compose.bom))
    implementation(libs.compose.ui)
    implementation(libs.compose.ui.graphics)
    implementation(libs.compose.material3)

    implementation(libs.camerax.core)
    implementation(libs.camerax.camera2)
    implementation(libs.camerax.lifecycle)
    implementation(libs.camerax.view)

    // Generating the publish ticket QR code.
    implementation(libs.zxing.core)
    // Scanning a ticket QR code in watch mode.
    implementation(libs.zxing.embedded)
}
