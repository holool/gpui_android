use crate::backend::{self, with_platform};
use gpui::{AppLifecyclePhase, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, PlatformInput};
use jni::{
    JNIEnv,
    objects::{JClass, JObject, JString},
    sys::{jboolean, jfloat, jint, jlong, jstring},
};
use serde::Deserialize;
use std::panic::{AssertUnwindSafe, catch_unwind};
fn guarded(f: impl FnOnce()) {
    if !backend::is_initialized() {
        return;
    }
    if let Err(e) = catch_unwind(AssertUnwindSafe(f)) {
        log::error!("GPUI Android callback panic: {e:?}");
    }
}
#[derive(Deserialize)]
struct Edit {
    op: String,
    #[serde(default)]
    text: String,
    #[serde(default)]
    start: usize,
    #[serde(default)]
    end: usize,
    #[serde(default)]
    cursor: i32,
    #[serde(default)]
    before: usize,
    #[serde(default)]
    after: usize,
}
fn json_snapshot() -> String {
    if !backend::is_initialized() {
        return "{}".into();
    }
    with_platform(|p| {
        p.state()
            .map(|s| {
                let mut v = serde_json::to_value(s.snapshot()).unwrap_or_default();
                v["configuration"] = backend::input_configuration();
                v
            })
            .map(|v| v.to_string())
            .unwrap_or_else(|| "{}".into())
    })
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_frame(_: JNIEnv, _: JClass) {
    guarded(|| with_platform(|p| p.frame()));
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_surface(
    mut env: JNIEnv,
    _: JClass,
    surface: JObject,
    w: jint,
    h: jint,
    scale: jfloat,
) {
    guarded(|| {
        with_platform(|p| {
            if let Err(e) = p.surface(&mut env, surface, w, h, scale) {
                log::error!("surface: {e:#}");
            }
        })
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_detach(_: JNIEnv, _: JClass) {
    guarded(|| with_platform(|p| p.detach()));
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_resize(
    _: JNIEnv,
    _: JClass,
    w: jint,
    h: jint,
    scale: jfloat,
) {
    guarded(|| with_platform(|p| p.resize(w, h, scale)));
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_touch(
    _: JNIEnv,
    _: JClass,
    action: jint,
    id: jint,
    x: jfloat,
    y: jfloat,
    force: jfloat,
) {
    guarded(|| {
        with_platform(|p| {
            if let Some(s) = p.state() {
                s.touch(action, id, x, y, force);
            }
        })
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_lifecycle(
    _: JNIEnv,
    _: JClass,
    phase: jint,
) {
    if !backend::is_initialized() {
        return;
    }
    guarded(|| {
        with_platform(|p| {
            p.lifecycle(match phase {
                0 => AppLifecyclePhase::Foreground,
                1 => AppLifecyclePhase::Active,
                2 => AppLifecyclePhase::Inactive,
                _ => AppLifecyclePhase::Background,
            })
        })
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_memoryWarning(_: JNIEnv, _: JClass) {
    if backend::is_initialized() {
        guarded(|| with_platform(|p| p.memory_warning()));
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_insets(
    _: JNIEnv,
    _: JClass,
    l: jint,
    t: jint,
    r: jint,
    b: jint,
    il: jint,
    it: jint,
    ir: jint,
    ib: jint,
) {
    if backend::is_initialized() {
        guarded(|| {
            with_platform(|p| {
                if let Some(s) = p.state() {
                    s.insets_changed([l, t, r, b, il, it, ir, ib]);
                }
            })
        });
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_back(_: JNIEnv, _: JClass) {
    if backend::is_initialized() {
        guarded(|| with_platform(|p| p.back()));
    }
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_snapshot(env: JNIEnv, _: JClass) -> jstring {
    let value = catch_unwind(AssertUnwindSafe(json_snapshot)).unwrap_or_else(|_| "{}".into());
    env.new_string(value)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_edit(
    mut env: JNIEnv,
    _: JClass,
    command: JString,
) -> jboolean {
    if !backend::is_initialized() {
        return 0;
    }
    let Ok(command) = env.get_string(&command) else {
        return 0;
    };
    let command: String = command.into();
    let Ok(edit) = serde_json::from_str::<Edit>(&command) else {
        return 0;
    };
    let result = catch_unwind(AssertUnwindSafe(|| {
        with_platform(|p| {
            let Some(s) = p.state() else { return false };
            let state = s.snapshot();
            if !state.enabled {
                return false;
            }
            s.with_input(|h| match edit.op.as_str() {
                "commit" | "compose" => {
                    let range = state.replacement();
                    let cursor = state.cursor_after(range.clone(), &edit.text, edit.cursor);
                    if edit.op == "compose" {
                        h.replace_and_mark_text_in_range(Some(range), &edit.text, None);
                    } else {
                        h.replace_text_in_range(Some(range), &edit.text);
                    }
                    h.set_selected_text_range(cursor..cursor);
                }
                "selection" => {
                    let r = state.clamp(edit.start.min(edit.end)..edit.start.max(edit.end));
                    h.set_selected_text_range(r);
                }
                "region" => {
                    let r = state.clamp(edit.start.min(edit.end)..edit.start.max(edit.end));
                    let text = state.slice(r.clone());
                    h.replace_and_mark_text_in_range(Some(r), &text, None);
                    h.set_selected_text_range(state.selection.clone());
                }
                "finish" => h.unmark_text(),
                "delete" | "deleteCodepoints" => {
                    let ranges = state.surrounding_deletions(
                        edit.before,
                        edit.after,
                        edit.op == "deleteCodepoints",
                    );
                    let removed = ranges[1].len();
                    for r in ranges {
                        if !r.is_empty() {
                            h.replace_text_in_range(Some(r), "");
                        }
                    }
                    if let Some(marked) = state.composing.clone() {
                        let text = state.slice(marked.clone());
                        h.replace_and_mark_text_in_range(
                            Some(marked.start - removed..marked.end - removed),
                            &text,
                            None,
                        );
                    }
                    h.set_selected_text_range(
                        state.selection.start.saturating_sub(removed)
                            ..state.selection.end.saturating_sub(removed),
                    );
                }
                _ => {}
            });
            p.frame();
            p.host_wake();
            true
        })
    }))
    .unwrap_or(false);
    result as jboolean
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_key(
    mut env: JNIEnv,
    _: JClass,
    down: jboolean,
    key: JString,
    shift: jboolean,
    ctrl: jboolean,
    alt: jboolean,
    meta: jboolean,
) {
    let Ok(key) = env.get_string(&key) else {
        return;
    };
    let key: String = key.into();
    guarded(|| {
        with_platform(|p| {
            if let Some(s) = p.state() {
                let keystroke = Keystroke {
                    modifiers: Modifiers {
                        shift: shift != 0,
                        control: ctrl != 0,
                        alt: alt != 0,
                        platform: meta != 0,
                        ..Default::default()
                    },
                    key,
                    key_char: None,
                };
                s.dispatch(if down != 0 {
                    PlatformInput::KeyDown(KeyDownEvent {
                        keystroke,
                        is_held: false,
                        prefer_character_input: false,
                    })
                } else {
                    PlatformInput::KeyUp(KeyUpEvent { keystroke })
                });
                p.host_wake();
            }
        })
    });
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_accessibility(
    env: JNIEnv,
    _: JClass,
) -> jstring {
    let data = if backend::is_initialized() {
        with_platform(|p| {
            p.state()
                .map(|s| s.a11y_json())
                .unwrap_or_else(|| "[]".into())
        })
    } else {
        "[]".into()
    };
    env.new_string(data)
        .map(|s| s.into_raw())
        .unwrap_or(std::ptr::null_mut())
}
#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_accessibilityAction(
    _: JNIEnv,
    _: JClass,
    id: jlong,
    action: jint,
) {
    guarded(|| {
        with_platform(|p| {
            if let Some(s) = p.state() {
                s.a11y_action(
                    id as u64,
                    if action == 16 {
                        gpui::accesskit::Action::Click
                    } else {
                        gpui::accesskit::Action::Focus
                    },
                );
            }
        })
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_org_holool_gpui_GpuiNative_shutdown(_: JNIEnv, _: JClass) {
    guarded(backend::shutdown);
}
