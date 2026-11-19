import java.io.FileInputStream
import java.util.Properties

plugins {
    id("com.android.application")
    // The Flutter Gradle Plugin must be applied after the Android and Kotlin Gradle plugins.
    id("dev.flutter.flutter-gradle-plugin")
}

val keyProps = Properties()
val keyPropsFile = rootProject.file("key.properties")
if (keyPropsFile.exists()) {
    FileInputStream(keyPropsFile).use { keyProps.load(it) }
}

val releaseStoreFilePath: String? =
    System.getenv("BITCHAT_KEYSTORE_PATH") ?: keyProps.getProperty("storeFile")
val releaseStorePassword: String? =
    System.getenv("BITCHAT_KEYSTORE_PASSWORD") ?: keyProps.getProperty("storePassword")
val releaseKeyAlias: String? =
    System.getenv("BITCHAT_KEY_ALIAS") ?: keyProps.getProperty("keyAlias")
val releaseKeyPassword: String? =
    System.getenv("BITCHAT_KEY_PASSWORD") ?: keyProps.getProperty("keyPassword")

val hasExternalReleaseSigning: Boolean =
    !releaseStoreFilePath.isNullOrBlank() &&
        !releaseStorePassword.isNullOrBlank() &&
        !releaseKeyAlias.isNullOrBlank() &&
        !releaseKeyPassword.isNullOrBlank() &&
        rootProject.file(releaseStoreFilePath).exists()

val allowDebugTestSigning: Boolean =
    (project.findProperty("allowDebugTestSigning")?.toString()?.equals("true", ignoreCase = true) == true)

val isReleaseTaskRequested: Boolean =
    gradle.startParameter.taskNames.any { taskName ->
        taskName.contains("Release", ignoreCase = true) ||
            taskName.contains("release", ignoreCase = false)
    }

if (isReleaseTaskRequested && !hasExternalReleaseSigning && !allowDebugTestSigning) {
    throw GradleException(
        "FAIL-CLOSED RELEASE SIGNING: Production release build requires external signing secrets " +
            "(BITCHAT_KEYSTORE_PATH / BITCHAT_KEYSTORE_PASSWORD / BITCHAT_KEY_ALIAS / BITCHAT_KEY_PASSWORD " +
            "or untracked app/android/key.properties). Production releases must NEVER silently fall back to debug signing. " +
            "See docs/RELEASE_SIGNING.md. For local/CI test APKs only, pass -PallowDebugTestSigning=true."
    )
}

android {
    namespace = "com.bitchat.bitchat2"
    compileSdk = flutter.compileSdkVersion
    ndkVersion = "30.0.16248370"

    buildFeatures {
        buildConfig = true
    }

    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }

    defaultConfig {
        applicationId = "com.bitchat.bitchat2"
        minSdk = flutter.minSdkVersion
        targetSdk = flutter.targetSdkVersion
        versionCode = flutter.versionCode
        versionName = flutter.versionName
        buildConfigField("int", "EXPECTED_NATIVE_ABI_VERSION", "2")
    }

    signingConfigs {
        if (hasExternalReleaseSigning) {
            create("release") {
                storeFile = rootProject.file(releaseStoreFilePath!!)
                storePassword = releaseStorePassword
                keyAlias = releaseKeyAlias
                keyPassword = releaseKeyPassword
            }
        }
    }

    buildTypes {
        debug {
            buildConfigField("boolean", "ALLOW_DEV_TOPOLOGY_OVERRIDES", "true")
            buildConfigField("String", "SIGNING_PROVENANCE", "\"DEBUG_DEV_KEY\"")
        }
        release {
            buildConfigField("boolean", "ALLOW_DEV_TOPOLOGY_OVERRIDES", "false")
            if (hasExternalReleaseSigning) {
                signingConfig = signingConfigs.getByName("release")
                buildConfigField("String", "SIGNING_PROVENANCE", "\"EXTERNAL_RELEASE_KEY\"")
            } else {
                // Only reachable when -PallowDebugTestSigning=true (enforced by fail-closed guard above)
                signingConfig = signingConfigs.getByName("debug")
                buildConfigField(
                    "String",
                    "SIGNING_PROVENANCE",
                    "\"DEBUG_TEST_KEY_NOT_FOR_PRODUCTION\""
                )
            }
        }
    }
}

kotlin {
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_17
    }
}

flutter {
    source = "../.."
}
