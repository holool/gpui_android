plugins { id("com.android.application"); id("org.jetbrains.kotlin.android") }
android {
    namespace="org.holool.gpui.minimal"
    compileSdk=36
    ndkVersion="27.1.12297006"
    defaultConfig { applicationId="org.holool.gpui.minimal";minSdk=30;targetSdk=36;versionCode=1;versionName="0.1.0";testInstrumentationRunner="org.holool.gpui.minimal.NativeSmokeInstrumentation";ndk { abiFilters += "arm64-v8a" } }
    sourceSets["main"].java.srcDir("../../../../android/src/main/kotlin")
    compileOptions {sourceCompatibility=JavaVersion.VERSION_17;targetCompatibility=JavaVersion.VERSION_17}
    kotlinOptions {jvmTarget="17"}
    packaging {jniLibs.useLegacyPackaging=false}
}
