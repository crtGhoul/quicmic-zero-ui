plugins {
    id("com.android.application")
}

android {
    namespace = "com.quicmic.webview"
    compileSdk = 34

    defaultConfig {
        applicationId = "com.quicmic.webview"
        minSdk = 26
        targetSdk = 34
        versionCode = 1
        versionName = "0.6.2"
    }

    buildTypes {
        release {
            isMinifyEnabled = false
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
}
