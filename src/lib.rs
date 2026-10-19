//! Android host for GPUI Fast. Rendering and Arabic shaping stay in Fast's shared engines.
//! Initialize on Android's UI thread from the host Activity; all callbacks remain on that thread.
#[cfg(target_os = "android")]
mod backend;
#[cfg(target_os = "android")]
mod ffi;
pub mod input;
#[cfg(target_os = "android")]
pub use backend::{AndroidPlatform, initialize};
#[cfg(target_os = "android")]
pub use backend::{application, data_directory, on_back, run, run_with};
#[doc(hidden)]
#[cfg(target_os = "android")]
pub mod __private {
    pub use gpui;
    pub use jni;
}
/// Export the Activity's native entry point. `launch` receives GPUI's App context;
/// the backend retains its ApplicationHandle until the host Activity is destroyed.
#[macro_export]
macro_rules! android_main {
    ($launch:path) => {
        $crate::android_main!($launch, |app| app);
    };
    ($launch:path, $configure:expr) => {
        #[unsafe(no_mangle)]
        pub extern "system" fn Java_org_holool_gpui_GpuiNative_start(
            mut env: $crate::__private::jni::JNIEnv,
            _: $crate::__private::jni::objects::JClass,
            activity: $crate::__private::jni::objects::JObject,
            surface: $crate::__private::jni::objects::JObject,
            width: i32,
            height: i32,
            scale: f32,
        ) -> u8 {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                $crate::initialize(&mut env, activity, surface, width, height, scale)?;
                $crate::run_with(($configure)($crate::application()), $launch);
                Ok::<(), Box<dyn std::error::Error>>(())
            })) {
                Ok(Ok(())) => 1,
                other => {
                    eprintln!("GPUI Android startup failed: {other:?}");
                    0
                }
            }
        }
    };
}
