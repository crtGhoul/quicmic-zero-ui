plugins {
    id("com.android.application")
    id("org.jetbrains.kotlin.android")
}

import java.io.File

android {
    namespace = "com.quicmic.android"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.quicmic.android"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.5.0"
    }

    // Release signing: set these env vars when building a shippable APK.
    //   QUICMIC_KEYSTORE_PATH     - path to the release keystore
    //   QUICMIC_KEYSTORE_PASSWORD - keystore password
    //   QUICMIC_KEY_ALIAS         - key alias (default "quicmic")
    //   QUICMIC_KEY_PASSWORD      - key password (defaults to the keystore password)
    // When unset, no release signing config is created and assembleRelease
    // produces an unsigned APK (local devs: use the debug build instead).
    val releaseKeystore = System.getenv("QUICMIC_KEYSTORE_PATH")
        ?.let { File(it) }
        ?.takeIf { it.isFile }

    signingConfigs {
        if (releaseKeystore != null) {
            create("release") {
                storeFile = releaseKeystore
                storePassword = System.getenv("QUICMIC_KEYSTORE_PASSWORD")
                keyAlias = System.getenv("QUICMIC_KEY_ALIAS") ?: "quicmic"
                keyPassword = System.getenv("QUICMIC_KEY_PASSWORD")
                    ?: System.getenv("QUICMIC_KEYSTORE_PASSWORD")
            }
        }
    }

    buildTypes {
        release {
            if (releaseKeystore != null) {
                signingConfig = signingConfigs.getByName("release")
            }
            isMinifyEnabled = true
            proguardFiles(
                getDefaultProguardFile("proguard-android-optimize.txt"),
                "proguard-rules.pro"
            )
        }
        debug {
            isMinifyEnabled = false
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
    // AndroidX UI + compat (minSdk 26-safe APIs only; see code for guards).
    implementation("androidx.appcompat:appcompat:1.7.0")
    implementation("androidx.core:core-ktx:1.13.1")

    // WebSocket transport for /ws and /speaker-ws (Maven Central, no Play Services).
    implementation("com.squareup.okhttp3:okhttp:4.12.0")

    // QR scanning for the pairing screen (NOT ML Kit — sideloaded, no Play Services).
    implementation("com.journeyapps:zxing-android-embedded:4.3.0")

    // EncryptedSharedPreferences for token / PIN / pinned cert / settings.
    implementation("androidx.security:security-crypto:1.1.0")
}
