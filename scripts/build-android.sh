#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
SDK_PATH="${ANDROID_HOME:-${ANDROID_SDK_ROOT:-$HOME/Library/Android/sdk}}"
NDK_PATH="${ANDROID_NDK_HOME:-$SDK_PATH/ndk/27.1.12297006}"
NDK_HOST="$(ls "$NDK_PATH/toolchains/llvm/prebuilt" | head -1)"
export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$NDK_PATH/toolchains/llvm/prebuilt/$NDK_HOST/bin/aarch64-linux-android30-clang"
export CC_aarch64_linux_android="$CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER"
export AR_aarch64_linux_android="$NDK_PATH/toolchains/llvm/prebuilt/$NDK_HOST/bin/llvm-ar"
export CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}"
cargo +1.95 rustc -p gpui-android-minimal --lib --target aarch64-linux-android --locked -- -C link-arg=-Wl,-z,max-page-size=16384 -C link-arg=-Wl,-z,common-page-size=16384
LIB_DIR=examples/minimal/android/app/src/main/jniLibs/arm64-v8a
mkdir -p "$LIB_DIR"
cp target/aarch64-linux-android/debug/libgpui_minimal.so "$LIB_DIR/"
export ANDROID_HOME="$SDK_PATH"
# AGP 8 / Gradle 8 are tested with Java 17; Homebrew may expose Java 27.
if [[ "$(uname)" == Darwin ]]; then
  export JAVA_HOME="${GPUI_JAVA_HOME:-$(/usr/libexec/java_home -v 17)}"
elif [[ -n "${GPUI_JAVA_HOME:-}" ]]; then
  export JAVA_HOME="$GPUI_JAVA_HOME"
fi
if [[ -n "${GRADLE:-}" ]]; then "$GRADLE" -p examples/minimal/android assembleDebug; else examples/minimal/android/gradlew -p examples/minimal/android assembleDebug; fi

BUILD_TOOLS="${ANDROID_BUILD_TOOLS_VERSION:-36.0.0}"
python3 scripts/check-android-artifact.py \
  "$NDK_PATH/toolchains/llvm/prebuilt/$NDK_HOST/bin/llvm-readelf" \
  "$LIB_DIR/libgpui_minimal.so" \
  "$SDK_PATH/build-tools/$BUILD_TOOLS/zipalign" \
  examples/minimal/android/app/build/outputs/apk/debug/app-debug.apk
