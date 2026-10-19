# GPUI Android

Native Android backend for [Holool GPUI Fast](https://github.com/holool/gpui-fast). GPUI draws directly into an Android `SurfaceView` with the current Fast `WgpuRenderer`. `CosmicTextSystem` loads Android fonts and retains Fast's Arabic shaping and bidirectional text support. No WebView or copied desktop renderer is involved.

This is an early backend, with one Android activity/window per application. Its API and the pinned Fast revision must be updated together. It is not yet a replacement for Android's complete native text editing and accessibility stacks.

## Build the runnable sample

Install Rust 1.95, Android SDK platform/build-tools 36, NDK 27.1.12297006, and JDK 17. Set `ANDROID_HOME` if the SDK is outside the usual macOS location. The build script selects JDK 17 explicitly on macOS (override with `GPUI_JAVA_HOME`); Linux uses the configured JDK.

```sh
scripts/build-android.sh
adb install -r examples/minimal/android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -n org.holool.gpui.minimal/org.holool.gpui.GpuiActivity
```

The final Rust shared library uses 16 KB maximum/common ELF page alignment. The included Gradle wrapper is pinned to 8.14.3. The sample and CI currently build arm64-v8a for Android 11 / API 30 and newer. `cargo test -p gpui_android --lib` also runs the platform-independent UTF-16 regression tests on the host.

The sample also includes an instrumentation runner that drives the real Kotlin `InputConnection` through JNI into a focused GPUI input handler:

```sh
# Use JDK 17 for Gradle, as in scripts/build-android.sh.
examples/minimal/android/gradlew -p examples/minimal/android assembleDebugAndroidTest
adb install -r examples/minimal/android/app/build/outputs/apk/androidTest/debug/app-debug-androidTest.apk
adb shell am instrument -w org.holool.gpui.minimal.test/org.holool.gpui.minimal.NativeSmokeInstrumentation
```

## Embed a Rust GPUI application

Create a Rust `cdylib`, depend on this crate and the **same pinned Fast revision**, and export the launch callback:

```rust,ignore
fn launch(cx: &mut gpui::App) {
    cx.open_window(gpui::WindowOptions::default(), |_, cx| {
        cx.new(|_| MyRootView)
    }).expect("open Android window");
}
gpui_android::android_main!(launch);
```

The macro uses this crate's internal GPUI/JNI reexports, so an application using the macro through a facade need not name `jni` directly. Include `android/src/main/kotlin` in the Android application's source set, copy its compiled shared library to `src/main/jniLibs/arm64-v8a`, and subclass `org.holool.gpui.GpuiActivity`:

```kotlin
class MainActivity : org.holool.gpui.GpuiActivity() {
    override fun nativeLibraryName() = "my_rust_library"
}
```

Use the sample manifest's configuration-change handling. The activity owns the OS event loop. `android_main!` initializes the platform and calls `run`, which retains GPUI's `run_embedded` application handle until the activity is destroyed. Do not also call `Application::run`.

`application()`, `run(callback)`, and `data_directory()` are available **only on the Android UI thread after native initialization**, normally inside the launch callback. `data_directory()` returns `Activity.filesDir` for app-private persistent data. A native surface must exist before opening the single GPUI window. No backend API is available on non-Android targets; the pure `input` module remains available for tests.

## Implemented behavior

- JNI/NDK surface ownership, resize/recreation, GPU context and atlas preservation, Choreographer wakeups without continuous idle polling, activity lifecycle and memory pressure.
- System bar, display cutout, and animated IME insets; keyboard dismissal and GPUI back routing, clipboard, URL opening, and app-private files path.
- Raw multi-touch contacts feed Fast's portable gesture recognizer. Android does not add a second fling or synthetic scroll stream.
- `InputConnection` surrounding text, selected/composing text, UTF-16 absolute offsets, Android commit cursor positioning, composing region changes, finish composition, deletion on both sides, codepoint deletion, extracted-text monitoring, hardware keys, clipboard actions, and GPUI text assistance configuration.
- AccessKit tree exposed as Android virtual accessibility nodes with stable 32-bit mappings for GPUI's 64-bit IDs, reachable-node pruning, labels, values, bounds, focus/click actions, accessibility focus, and touch exploration.

GPUI retains selection painting and interaction. Native Android selection handles/action modes, handwriting, rich content insertion, cursor-anchor updates, and the full AccessKit action/state vocabulary are not implemented. File/credential dialogs, desktop menus, auxiliary executables, and multiple windows are not provided. Validate real Arabic keyboards, TalkBack, device GPU drivers, and application-specific text handlers before shipping. The platform cannot supply selection mutations if an application input handler leaves Fast's `set_selected_text_range` default unimplemented.

## Develop against a local Fast checkout

Create an ignored `.cargo/config.toml`:

```toml
[patch."https://github.com/holool/gpui-fast"]
gpui = { path = "/absolute/path/gpui-fast/crates/gpui" }
gpui_wgpu = { path = "/absolute/path/gpui-fast/crates/gpui_wgpu" }
```

Keep all consumers on the same GPUI source to avoid duplicate crate types. The backend uses Fast's public platform interfaces without copying its renderer, scene, text shaper, or scroll cache. See `NOTICE` for attribution.
