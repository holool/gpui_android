use crate::input::InputSnapshot;
use anyhow::{Result, anyhow, bail};
use futures::channel::oneshot;
use gpui::*;
use gpui_wgpu::{CosmicTextSystem, GpuContext, WgpuRenderer, WgpuSurfaceConfig};
use jni::{
    JNIEnv, JavaVM,
    objects::{GlobalRef, JObject, JValue},
};
use ndk::native_window::NativeWindow;
use raw_window_handle::{
    AndroidDisplayHandle, AndroidNdkWindowHandle, DisplayHandle, HandleError, HasDisplayHandle,
    HasWindowHandle, RawDisplayHandle, RawWindowHandle, WindowHandle,
};
use std::{
    borrow::Cow,
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet},
    ffi::{OsString, c_void},
    path::{Path, PathBuf},
    ptr::NonNull,
    rc::Rc,
    sync::{Arc, mpsc},
    thread,
    time::{Duration, Instant},
};

thread_local! {
    static PLATFORM: RefCell<Option<Rc<AndroidPlatform>>> = const { RefCell::new(None) };
    static APP: RefCell<Option<ApplicationHandle>> = const { RefCell::new(None) };
}
/// GPUI application on the Android UI thread. Call only after native initialization.
pub fn application() -> Application {
    with_platform(|p| Application::with_platform(p.clone()))
}
/// Retains GPUI while Android owns the process event loop.
pub fn run(on_launch: impl FnOnce(&mut App) + 'static) {
    let handle = application().run_embedded(on_launch);
    APP.with(|app| *app.borrow_mut() = Some(handle));
}
pub(crate) fn with_platform<R>(f: impl FnOnce(&Rc<AndroidPlatform>) -> R) -> R {
    PLATFORM.with(|p| {
        f(p.borrow()
            .as_ref()
            .expect("initialize GPUI from GpuiActivity before calling application/run"))
    })
}
pub(crate) fn is_initialized() -> bool {
    PLATFORM.with(|p| p.borrow().is_some())
}
/// Called by `android_main!` with the first real Surface. Fonts and the GPU atlas
/// are ready before the application's first `open_window` paints a glyph.
pub fn initialize(
    env: &mut JNIEnv,
    activity: JObject,
    surface: JObject,
    width: i32,
    height: i32,
    scale: f32,
) -> Result<()> {
    android_logger::init_once(
        android_logger::Config::default()
            .with_tag("HoloolGPUI")
            .with_max_level(log::LevelFilter::Info),
    );
    let host = Arc::new(Host {
        vm: env.get_java_vm()?,
        activity: env.new_global_ref(activity)?,
    });
    let surface =
        unsafe { NativeWindow::from_surface(env.get_native_interface(), surface.as_raw()) }
            .ok_or_else(|| anyhow!("Android Surface has no native window"))?;
    let platform = Rc::new(AndroidPlatform::new(host, surface, width, height, scale));
    PLATFORM.with(|p| *p.borrow_mut() = Some(platform));
    Ok(())
}
pub(crate) struct Host {
    vm: JavaVM,
    activity: GlobalRef,
}
impl Host {
    pub fn call(&self, name: &str, signature: &str, args: &[JValue]) {
        if let Ok(mut env) = self.vm.attach_current_thread() {
            if let Err(e) = env.call_method(self.activity.as_obj(), name, signature, args) {
                log::error!("Android {name}: {e}");
                let _ = env.exception_clear();
            }
        }
    }
    pub fn text(&self, method: &str, text: &str) {
        if let Ok(mut env) = self.vm.attach_current_thread() {
            if let Ok(value) = env.new_string(text) {
                if let Err(e) = env.call_method(
                    self.activity.as_obj(),
                    method,
                    "(Ljava/lang/String;)V",
                    &[JValue::Object(&value)],
                ) {
                    log::error!("{method}: {e}");
                    let _ = env.exception_clear();
                }
            }
        }
    }
    pub fn wake(&self) {
        self.call("wake", "()V", &[]);
    }
}
struct Dispatcher {
    main: mpsc::Sender<RunnableVariant>,
    background: mpsc::Sender<RunnableVariant>,
    timers: mpsc::Sender<(Instant, RunnableVariant)>,
    host: Arc<Host>,
    main_id: thread::ThreadId,
}
impl Dispatcher {
    fn new(host: Arc<Host>) -> (Arc<Self>, mpsc::Receiver<RunnableVariant>) {
        let (main, receiver) = mpsc::channel();
        let (background, jobs) = mpsc::channel::<RunnableVariant>();
        let jobs = Arc::new(parking_lot::Mutex::new(jobs));
        for n in 0..thread::available_parallelism().map_or(2, |n| n.get().clamp(2, 4)) {
            let jobs = jobs.clone();
            thread::Builder::new()
                .name(format!("gpui-worker-{n}"))
                .spawn(move || {
                    loop {
                        let task = jobs.lock().recv();
                        match task {
                            Ok(task) => {
                                task.run();
                            }
                            Err(_) => break,
                        }
                    }
                })
                .expect("GPUI worker");
        }
        let (timers, timer_rx) = mpsc::channel::<(Instant, RunnableVariant)>();
        thread::Builder::new()
            .name("gpui-timers".into())
            .spawn(move || {
                let mut pending: BTreeMap<(Instant, u64), RunnableVariant> = BTreeMap::new();
                let mut serial = 0u64;
                loop {
                    while pending
                        .first_key_value()
                        .is_some_and(|((at, _), _)| *at <= Instant::now())
                    {
                        let (_, task) = pending.pop_first().unwrap();
                        RunnableVariant::run(task);
                    }
                    let next = match pending.first_key_value() {
                        Some(((at, _), _)) => {
                            timer_rx.recv_timeout(at.saturating_duration_since(Instant::now()))
                        }
                        None => match timer_rx.recv() {
                            Ok(v) => Ok(v),
                            Err(_) => break,
                        },
                    };
                    match next {
                        Ok((at, task)) => {
                            serial += 1;
                            pending.insert((at, serial), task);
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
            })
            .expect("GPUI timer");
        (
            Arc::new(Self {
                main,
                background,
                timers,
                host,
                main_id: thread::current().id(),
            }),
            receiver,
        )
    }
}
impl PlatformDispatcher for Dispatcher {
    fn is_main_thread(&self) -> bool {
        thread::current().id() == self.main_id
    }
    fn dispatch(&self, r: RunnableVariant, _p: Priority) {
        let _ = self.background.send(r);
    }
    fn dispatch_on_main_thread(&self, r: RunnableVariant, _: Priority) {
        let _ = self.main.send(r);
        self.host.wake();
    }
    fn dispatch_after(&self, d: Duration, r: RunnableVariant) {
        let _ = self.timers.send((Instant::now() + d, r));
    }
    fn spawn_realtime(&self, f: Box<dyn FnOnce() + Send>) {
        let _ = thread::Builder::new().name("gpui-realtime".into()).spawn(f);
    }
}
#[derive(Default)]
struct PlatformCallbacks {
    lifecycle: Option<Box<dyn FnMut(AppLifecyclePhase)>>,
    memory: Option<Box<dyn FnMut()>>,
    sleep: Option<Box<dyn FnMut()>>,
    wake: Option<Box<dyn FnMut()>>,
    displays: Option<Box<dyn FnMut()>>,
    quit: Option<Box<dyn FnMut() -> bool>>,
    reopen: Option<Box<dyn FnMut()>>,
    urls: Option<Box<dyn FnMut(Vec<String>)>>,
}
pub struct AndroidPlatform {
    host: Arc<Host>,
    background: BackgroundExecutor,
    foreground: ForegroundExecutor,
    text: Arc<CosmicTextSystem>,
    main: mpsc::Receiver<RunnableVariant>,
    gpu: GpuContext,
    native: RefCell<Option<NativeWindow>>,
    dimensions: Cell<(i32, i32, f32)>,
    window: RefCell<Option<Rc<State>>>,
    callbacks: RefCell<PlatformCallbacks>,
}
impl AndroidPlatform {
    fn new(host: Arc<Host>, native: NativeWindow, width: i32, height: i32, scale: f32) -> Self {
        let (dispatcher, main) = Dispatcher::new(host.clone());
        let text = Arc::new(CosmicTextSystem::new_without_system_fonts("Roboto"));
        // Load a coherent set, including Arabic and emoji, before publishing any font IDs.
        // Fast's shared engine owns all shaping, bidi metadata, and rasterization.
        for dir in ["/system/fonts", "/product/fonts", "/vendor/fonts"] {
            if let Ok(files) = std::fs::read_dir(dir) {
                for file in files.flatten() {
                    let path = file.path();
                    if path
                        .extension()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| matches!(s, "ttf" | "otf" | "ttc"))
                    {
                        if let Ok(bytes) = std::fs::read(path) {
                            let _ = text.add_fonts(vec![Cow::Owned(bytes)]);
                        }
                    }
                }
            }
        }
        Self {
            host,
            background: BackgroundExecutor::new(dispatcher.clone()),
            foreground: ForegroundExecutor::new(dispatcher),
            text,
            main,
            gpu: Rc::new(RefCell::new(None)),
            native: RefCell::new(Some(native)),
            dimensions: Cell::new((width, height, scale.max(0.1))),
            window: RefCell::new(None),
            callbacks: RefCell::new(PlatformCallbacks::default()),
        }
    }
    pub(crate) fn state(&self) -> Option<Rc<State>> {
        self.window.borrow().clone()
    }
    pub(crate) fn frame(&self) {
        for _ in 0..1000 {
            match self.main.try_recv() {
                Ok(task) => {
                    task.run();
                }
                Err(_) => break,
            }
        }
        if let Some(s) = self.state() {
            if s.visible.get() == WindowVisibility::Visible
                && s.native.borrow().is_some()
                && s.renderer.borrow().is_some()
            {
                let mut force_render = s.force_render.replace(false);
                {
                    let mut renderer = s.renderer.borrow_mut();
                    if let Some(renderer) = renderer.as_mut() {
                        if renderer.device_lost() {
                            let native = s.native.borrow();
                            let raw = RawWindow(native.as_ref().unwrap().ptr().as_ptr() as usize);
                            if let Err(error) = renderer.recover(&raw) {
                                log::warn!("Android GPU recovery failed: {error:#}");
                                self.host.wake();
                                return;
                            }
                            force_render = true;
                        }
                        force_render |= renderer.needs_redraw();
                    }
                }
                let callback = s.callbacks.borrow_mut().frame.take();
                if let Some(mut callback) = callback {
                    callback(RequestFrameOptions {
                        force_render,
                        ..Default::default()
                    });
                    s.callbacks.borrow_mut().frame = Some(callback);
                }
            }
        }
    }
    pub(crate) fn surface(
        &self,
        env: &mut JNIEnv,
        surface: JObject,
        w: i32,
        h: i32,
        scale: f32,
    ) -> Result<()> {
        let native =
            unsafe { NativeWindow::from_surface(env.get_native_interface(), surface.as_raw()) }
                .ok_or_else(|| anyhow!("surface unavailable"))?;
        self.dimensions.set((w, h, scale));
        *self.native.borrow_mut() = Some(native.clone());
        if let Some(s) = self.state() {
            s.attach(native, self.gpu.clone(), w, h, scale)?;
            s.resized(w, h, scale);
        }
        self.host.wake();
        Ok(())
    }
    pub(crate) fn detach(&self) {
        if let Some(s) = self.state() {
            s.cancel_contacts();
            if let Some(r) = s.renderer.borrow_mut().as_mut() {
                r.unconfigure_surface();
            }
            s.native.borrow_mut().take();
        }
        self.native.borrow_mut().take();
    }
    pub(crate) fn resize(&self, w: i32, h: i32, scale: f32) {
        self.dimensions.set((w, h, scale));
        if let Some(s) = self.state() {
            s.resized(w, h, scale);
        }
    }
    pub(crate) fn lifecycle(&self, phase: AppLifecyclePhase) {
        if let Some(s) = self.state() {
            let visible = if phase == AppLifecyclePhase::Background {
                WindowVisibility::Hidden
            } else {
                WindowVisibility::Visible
            };
            let changed = s.visible.replace(visible) != visible;
            if changed {
                let cb = s.callbacks.borrow_mut().visibility.take();
                if let Some(mut cb) = cb {
                    cb(visible);
                    s.callbacks.borrow_mut().visibility = Some(cb);
                }
            }
            let active = phase == AppLifecyclePhase::Active;
            let changed = s.active.replace(active) != active;
            if !active {
                s.cancel_contacts();
            }
            if changed {
                let cb = s.callbacks.borrow_mut().active.take();
                if let Some(mut cb) = cb {
                    cb(active);
                    s.callbacks.borrow_mut().active = Some(cb);
                }
            }
        }
        let cb = self.callbacks.borrow_mut().lifecycle.take();
        if let Some(mut cb) = cb {
            cb(phase);
            self.callbacks.borrow_mut().lifecycle = Some(cb);
        }
        if phase == AppLifecyclePhase::Active {
            self.host.wake();
        }
    }
    pub(crate) fn memory_warning(&self) {
        let cb = self.callbacks.borrow_mut().memory.take();
        if let Some(mut cb) = cb {
            cb();
            self.callbacks.borrow_mut().memory = Some(cb);
        }
    }
}
struct KeyboardLayout;
impl PlatformKeyboardLayout for KeyboardLayout {
    fn id(&self) -> &str {
        "android"
    }
    fn name(&self) -> &str {
        "Android system keyboard"
    }
}
impl Platform for AndroidPlatform {
    fn background_executor(&self) -> BackgroundExecutor {
        self.background.clone()
    }
    fn foreground_executor(&self) -> ForegroundExecutor {
        self.foreground.clone()
    }
    fn text_system(&self) -> Arc<dyn PlatformTextSystem> {
        self.text.clone()
    }
    fn run(&self, launch: Box<dyn FnOnce()>) {
        launch();
    }
    fn quit(&self) {
        self.host.call("finish", "()V", &[]);
    }
    fn restart(&self, _: Option<PathBuf>, _: Vec<OsString>) {
        log::warn!("Android restart is OS-managed");
    }
    fn activate(&self, _: bool) {}
    fn hide(&self) {
        self.host.call("moveToBackground", "()V", &[]);
    }
    fn hide_other_apps(&self) {}
    fn unhide_other_apps(&self) {}
    fn displays(&self) -> Vec<Rc<dyn PlatformDisplay>> {
        vec![]
    }
    fn primary_display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        None
    }
    fn on_displays_changed(&self, cb: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().displays = Some(cb);
    }
    fn active_window(&self) -> Option<AnyWindowHandle> {
        self.state().map(|s| s.handle)
    }
    fn open_window(
        &self,
        handle: AnyWindowHandle,
        _: WindowParams,
    ) -> Result<Box<dyn PlatformWindow>> {
        if self.window.borrow().is_some() {
            bail!(
                "one Android Activity hosts one GPUI window; create an additional Activity for another window"
            );
        }
        let (w, h, scale) = self.dimensions.get();
        let state = Rc::new(State::new(handle, self.host.clone(), w, h, scale));
        state.attach(
            self.native
                .borrow()
                .as_ref()
                .ok_or_else(|| anyhow!("open_window requires a live Android Surface"))?
                .clone(),
            self.gpu.clone(),
            w,
            h,
            scale,
        )?;
        *self.window.borrow_mut() = Some(state.clone());
        Ok(Box::new(AndroidWindow(state)))
    }
    fn window_appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }
    fn open_url(&self, url: &str) {
        self.host.text("openUrl", url);
    }
    fn on_open_urls(&self, cb: Box<dyn FnMut(Vec<String>)>) {
        self.callbacks.borrow_mut().urls = Some(cb);
    }
    fn register_url_scheme(&self, _: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow!("declare URL schemes in AndroidManifest.xml")))
    }
    fn prompt_for_paths(
        &self,
        _: PathPromptOptions,
    ) -> oneshot::Receiver<Result<Option<Vec<PathBuf>>>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(anyhow!(
            "use Android Storage Access Framework host integration"
        )));
        rx
    }
    fn prompt_for_new_path(
        &self,
        _: &Path,
        _: Option<&str>,
    ) -> oneshot::Receiver<Result<Option<PathBuf>>> {
        let (tx, rx) = oneshot::channel();
        let _ = tx.send(Err(anyhow!(
            "use Android Storage Access Framework host integration"
        )));
        rx
    }
    fn can_select_mixed_files_and_dirs(&self) -> bool {
        false
    }
    fn reveal_path(&self, _: &Path) {}
    fn open_with_system(&self, p: &Path) {
        self.open_url(&p.to_string_lossy());
    }
    fn on_quit(&self, cb: Box<dyn FnMut() -> bool>) {
        self.callbacks.borrow_mut().quit = Some(cb);
    }
    fn on_reopen(&self, cb: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().reopen = Some(cb);
    }
    fn on_system_sleep(&self, cb: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().sleep = Some(cb);
    }
    fn on_system_wake(&self, cb: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().wake = Some(cb);
    }
    fn on_app_lifecycle(&self, cb: Box<dyn FnMut(AppLifecyclePhase)>) {
        self.callbacks.borrow_mut().lifecycle = Some(cb);
    }
    fn on_memory_warning(&self, cb: Box<dyn FnMut()>) {
        self.callbacks.borrow_mut().memory = Some(cb);
    }
    fn set_menus(&self, _: Vec<Menu>, _: &Keymap) {}
    fn set_dock_menu(&self, _: Vec<MenuItem>, _: &Keymap) {}
    fn on_app_menu_action(&self, _: Box<dyn FnMut(&dyn Action)>) {}
    fn on_will_open_app_menu(&self, _: Box<dyn FnMut()>) {}
    fn on_validate_app_menu_command(&self, _: Box<dyn FnMut(&dyn Action) -> bool>) {}
    fn thermal_state(&self) -> ThermalState {
        ThermalState::Nominal
    }
    fn on_thermal_state_change(&self, _: Box<dyn FnMut()>) {}
    fn prevent_idle_sleep(&self, _: &str) -> Task<Result<ActivityGuard>> {
        Task::ready(Err(anyhow!(
            "use Activity FLAG_KEEP_SCREEN_ON with explicit user intent"
        )))
    }
    fn app_path(&self) -> Result<PathBuf> {
        Ok(PathBuf::from("."))
    }
    fn path_for_auxiliary_executable(&self, _: &str) -> Result<PathBuf> {
        bail!("Android does not support auxiliary executables")
    }
    fn set_cursor_style(&self, _: CursorStyle) {}
    fn hide_cursor_until_mouse_moves(&self) {}
    fn is_cursor_visible(&self) -> bool {
        true
    }
    fn should_auto_hide_scrollbars(&self) -> bool {
        true
    }
    fn read_from_clipboard(&self) -> Option<ClipboardItem> {
        let mut env = self.host.vm.attach_current_thread().ok()?;
        let result = env
            .call_method(
                self.host.activity.as_obj(),
                "clipboardText",
                "()Ljava/lang/String;",
                &[],
            )
            .ok()?
            .l()
            .ok()?;
        if result.is_null() {
            return None;
        }
        let text: String = env
            .get_string(&jni::objects::JString::from(result))
            .ok()?
            .into();
        Some(ClipboardItem::new_string(text))
    }
    fn write_to_clipboard(&self, item: ClipboardItem) {
        if let Some(text) = item.text() {
            self.host.text("setClipboardText", &text);
        }
    }
    fn write_credentials(&self, _: &str, _: &str, _: &[u8]) -> Task<Result<()>> {
        Task::ready(Err(anyhow!(
            "configure an Android Keystore credential store in the application"
        )))
    }
    fn read_credentials(&self, _: &str) -> Task<Result<Option<(String, Vec<u8>)>>> {
        Task::ready(Err(anyhow!("no Android credential store configured")))
    }
    fn delete_credentials(&self, _: &str) -> Task<Result<()>> {
        Task::ready(Err(anyhow!("no Android credential store configured")))
    }
    fn keyboard_layout(&self) -> Box<dyn PlatformKeyboardLayout> {
        Box::new(KeyboardLayout)
    }
    fn keyboard_mapper(&self) -> Rc<dyn PlatformKeyboardMapper> {
        Rc::new(DummyKeyboardMapper)
    }
    fn on_keyboard_layout_change(&self, _: Box<dyn FnMut()>) {}
}

#[derive(Clone, Debug)]
struct RawWindow(usize);
impl HasWindowHandle for RawWindow {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        let pointer = NonNull::new(self.0 as *mut c_void).ok_or(HandleError::Unavailable)?;
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(
                pointer,
            )))
        })
    }
}
impl HasDisplayHandle for RawWindow {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Android(AndroidDisplayHandle::new()))
        })
    }
}
#[derive(Default)]
struct Callbacks {
    frame: Option<Box<dyn FnMut(RequestFrameOptions)>>,
    input: Option<Box<dyn FnMut(PlatformInput) -> DispatchEventResult>>,
    active: Option<Box<dyn FnMut(bool)>>,
    visibility: Option<Box<dyn FnMut(WindowVisibility)>>,
    resize: Option<Box<dyn FnMut(Size<Pixels>, f32)>>,
    viewport: Option<Box<dyn FnMut()>>,
    insets: Option<Box<dyn FnMut(WindowInsets)>>,
    back: Option<Box<dyn FnMut()>>,
}
pub(crate) struct State {
    handle: AnyWindowHandle,
    host: Arc<Host>,
    renderer: RefCell<Option<WgpuRenderer>>,
    native: RefCell<Option<NativeWindow>>,
    bounds: Cell<Bounds<Pixels>>,
    scale: Cell<f32>,
    active: Cell<bool>,
    visible: Cell<WindowVisibility>,
    insets: RefCell<WindowInsets>,
    callbacks: RefCell<Callbacks>,
    input: RefCell<Option<PlatformInputHandler>>,
    epoch: Cell<u64>,
    config: RefCell<TextInputConfiguration>,
    contacts: RefCell<HashMap<i32, (TouchId, Point<Pixels>)>>,
    next_contact: Cell<u64>,
    force_render: Cell<bool>,
    a11y: RefCell<Option<A11yCallbacks>>,
    nodes: RefCell<BTreeMap<u64, accesskit::Node>>,
    root: Cell<u64>,
}
impl State {
    fn new(handle: AnyWindowHandle, host: Arc<Host>, w: i32, h: i32, scale: f32) -> Self {
        Self {
            handle,
            host,
            renderer: RefCell::new(None),
            native: RefCell::new(None),
            bounds: Cell::new(Bounds::new(
                Point::default(),
                size(px(w as f32 / scale), px(h as f32 / scale)),
            )),
            scale: Cell::new(scale),
            active: Cell::new(true),
            visible: Cell::new(WindowVisibility::Visible),
            insets: RefCell::new(WindowInsets::default()),
            callbacks: RefCell::new(Callbacks::default()),
            input: RefCell::new(None),
            epoch: Cell::new(0),
            config: RefCell::new(TextInputConfiguration::default()),
            contacts: RefCell::new(HashMap::new()),
            next_contact: Cell::new(0),
            force_render: Cell::new(false),
            a11y: RefCell::new(None),
            nodes: RefCell::new(BTreeMap::new()),
            root: Cell::new(0),
        }
    }
    fn attach(&self, native: NativeWindow, gpu: GpuContext, w: i32, h: i32, _: f32) -> Result<()> {
        let raw = RawWindow(native.ptr().as_ptr() as usize);
        let config = WgpuSurfaceConfig {
            size: size(DevicePixels(w.max(1)), DevicePixels(h.max(1))),
            transparent: false,
            preferred_present_mode: None,
        };
        let mut renderer = self.renderer.borrow_mut();
        if let Some(renderer) = renderer.as_mut() {
            if renderer.device_lost() {
                renderer.recover(&raw)?;
                renderer.update_drawable_size(config.size);
                self.force_render.set(true);
                *self.native.borrow_mut() = Some(native);
                return Ok(());
            }
            let context = gpu.borrow();
            renderer.replace_surface(
                &raw,
                config,
                &context
                    .as_ref()
                    .ok_or_else(|| anyhow!("missing GPU context"))?
                    .instance,
            )?;
        } else {
            *renderer = Some(WgpuRenderer::new(gpu, &raw, config, None)?);
        }
        *self.native.borrow_mut() = Some(native);
        Ok(())
    }
    fn resized(&self, w: i32, h: i32, scale: f32) {
        let scale = scale.max(0.1);
        self.scale.set(scale);
        let size = size(px(w as f32 / scale), px(h as f32 / scale));
        self.bounds.set(Bounds::new(Point::default(), size));
        if let Some(r) = self.renderer.borrow_mut().as_mut() {
            r.update_drawable_size(size.map(|v| DevicePixels((f32::from(v) * scale) as i32)));
        }
        let cb = self.callbacks.borrow_mut().resize.take();
        if let Some(mut cb) = cb {
            cb(size, scale);
            self.callbacks.borrow_mut().resize = Some(cb);
        }
        self.host.wake();
    }
    pub(crate) fn dispatch(&self, event: PlatformInput) -> DispatchEventResult {
        let cb = self.callbacks.borrow_mut().input.take();
        if let Some(mut cb) = cb {
            let result = cb(event);
            self.callbacks.borrow_mut().input = Some(cb);
            result
        } else {
            DispatchEventResult::default()
        }
    }
    pub(crate) fn touch(&self, action: i32, native_id: i32, x: f32, y: f32, force: f32) {
        let pos = point(px(x / self.scale.get()), px(y / self.scale.get()));
        if action == 3 {
            self.cancel_contacts();
            return;
        }
        let (id, phase) = if action == 0 {
            let id = TouchId(self.next_contact.get() + 1);
            self.next_contact.set(id.0);
            self.contacts.borrow_mut().insert(native_id, (id, pos));
            (id, TouchPhase::Started)
        } else {
            let value = self.contacts.borrow().get(&native_id).cloned();
            let Some((id, _)) = value else { return };
            if action == 1 {
                self.contacts.borrow_mut().remove(&native_id);
                (id, TouchPhase::Ended)
            } else {
                self.contacts.borrow_mut().insert(native_id, (id, pos));
                (id, TouchPhase::Moved)
            }
        };
        self.dispatch(PlatformInput::Touch(TouchEvent {
            id,
            phase,
            position: pos,
            predicted_position: None,
            force: Some(force.clamp(0., 1.)),
        }));
        self.host.wake();
    }
    fn cancel_contacts(&self) {
        let contacts = std::mem::take(&mut *self.contacts.borrow_mut());
        for (_, (id, pos)) in contacts {
            self.dispatch(PlatformInput::Touch(TouchEvent {
                id,
                phase: TouchPhase::Cancelled,
                position: pos,
                predicted_position: None,
                force: None,
            }));
        }
    }
    pub(crate) fn with_input<R>(
        &self,
        f: impl FnOnce(&mut PlatformInputHandler) -> R,
    ) -> Option<R> {
        let epoch = self.epoch.get();
        let handler = self.input.borrow_mut().take();
        let mut handler = handler?;
        let value = f(&mut handler);
        if self.epoch.get() == epoch && self.input.borrow().is_none() {
            *self.input.borrow_mut() = Some(handler);
        }
        Some(value)
    }
    pub(crate) fn snapshot(&self) -> InputSnapshot {
        self.with_input(|handler| {
            if !handler.query_accepts_text_input() {
                return InputSnapshot::default();
            }
            let Some(selection) = handler.selected_text_range(false).map(|s| s.range) else {
                return InputSnapshot::default();
            };
            let document_end = handler
                .text_length_utf16()
                .unwrap_or_else(|| selection.end.saturating_add(8192));
            let editable = handler
                .text_input_editable_range()
                .unwrap_or(0..document_end);
            let mut adjusted = None;
            let text = handler
                .text_for_range(editable.clone(), &mut adjusted)
                .unwrap_or_default();
            let range = adjusted.unwrap_or(editable);
            if selection.start < range.start || selection.end > range.end {
                return InputSnapshot::default();
            }
            InputSnapshot {
                text,
                offset: range.start,
                selection,
                composing: handler.marked_text_range(),
                enabled: true,
            }
        })
        .unwrap_or_default()
    }
    pub(crate) fn insets_changed(&self, values: [i32; 8]) {
        let s = self.scale.get();
        let edge = |a: &[i32]| Edges {
            left: px(a[0] as f32 / s),
            top: px(a[1] as f32 / s),
            right: px(a[2] as f32 / s),
            bottom: px(a[3] as f32 / s),
        };
        let value = WindowInsets {
            safe_area: edge(&values[..4]),
            ime: edge(&values[4..]),
        };
        *self.insets.borrow_mut() = value.clone();
        let cb = self.callbacks.borrow_mut().insets.take();
        if let Some(mut cb) = cb {
            cb(value);
            self.callbacks.borrow_mut().insets = Some(cb);
        }
        let cb = self.callbacks.borrow_mut().viewport.take();
        if let Some(mut cb) = cb {
            cb();
            self.callbacks.borrow_mut().viewport = Some(cb);
        }
        self.host.wake();
    }
    pub(crate) fn back(&self) {
        let cb = self.callbacks.borrow_mut().back.take();
        if let Some(mut cb) = cb {
            cb();
            self.callbacks.borrow_mut().back = Some(cb);
        } else {
            self.host.call("finish", "()V", &[]);
        }
    }
    fn a11y_update(&self, update: accesskit::TreeUpdate) {
        if let Some(tree) = update.tree {
            self.root.set(tree.root.0);
        }
        for (id, node) in update.nodes {
            self.nodes.borrow_mut().insert(id.0, node);
        }
        // AccessKit updates are incremental; drop nodes no longer reachable from root.
        let mut nodes = self.nodes.borrow_mut();
        let mut reachable = HashSet::new();
        let mut pending = vec![self.root.get()];
        while let Some(id) = pending.pop() {
            if reachable.insert(id) {
                if let Some(node) = nodes.get(&id) {
                    pending.extend(node.children().iter().map(|child| child.0));
                }
            }
        }
        nodes.retain(|id, _| reachable.contains(id));
        drop(nodes);
        self.host.call("accessibilityChanged", "()V", &[]);
    }
    pub(crate) fn a11y_json(&self) -> String {
        let nodes = self.nodes.borrow();
        serde_json::to_string(&nodes.iter().map(|(id,n)|{let b=n.bounds().unwrap_or(accesskit::Rect::ZERO);serde_json::json!({"id":id.to_string(),"root":*id == self.root.get(),"role":format!("{:?}",n.role()),"label":n.label(),"value":n.value(),"children":n.children().iter().map(|n|n.0.to_string()).collect::<Vec<_>>(),"bounds":[b.x0,b.y0,b.x1,b.y1],"click":n.supports_action(accesskit::Action::Click),"focus":n.supports_action(accesskit::Action::Focus)})}).collect::<Vec<_>>()).unwrap_or_else(|_|"[]".into())
    }
    pub(crate) fn a11y_action(&self, id: u64, action: accesskit::Action) {
        if let Some(callbacks) = self.a11y.borrow().as_ref() {
            (callbacks.action)(accesskit::ActionRequest {
                action,
                target_tree: accesskit::TreeId::ROOT,
                target_node: accesskit::NodeId(id),
                data: None,
            });
        }
        self.host.wake();
    }
}
struct AndroidWindow(Rc<State>);
impl HasWindowHandle for AndroidWindow {
    fn window_handle(&self) -> std::result::Result<WindowHandle<'_>, HandleError> {
        let ptr = self
            .0
            .native
            .borrow()
            .as_ref()
            .map(|n| n.ptr())
            .ok_or(HandleError::Unavailable)?;
        Ok(unsafe {
            WindowHandle::borrow_raw(RawWindowHandle::AndroidNdk(AndroidNdkWindowHandle::new(
                ptr.cast(),
            )))
        })
    }
}
impl HasDisplayHandle for AndroidWindow {
    fn display_handle(&self) -> std::result::Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe {
            DisplayHandle::borrow_raw(RawDisplayHandle::Android(AndroidDisplayHandle::new()))
        })
    }
}
impl PlatformWindow for AndroidWindow {
    fn bounds(&self) -> Bounds<Pixels> {
        self.0.bounds.get()
    }
    fn is_maximized(&self) -> bool {
        true
    }
    fn window_bounds(&self) -> WindowBounds {
        WindowBounds::Fullscreen(self.bounds())
    }
    fn content_size(&self) -> Size<Pixels> {
        self.bounds().size
    }
    fn visual_viewport_bounds(&self) -> Bounds<Pixels> {
        let edge = self.0.insets.borrow().effective();
        Bounds::new(
            point(edge.left, edge.top),
            size(
                (self.content_size().width - edge.left - edge.right).max(px(0.)),
                (self.content_size().height - edge.top - edge.bottom).max(px(0.)),
            ),
        )
    }
    fn on_visual_viewport_changed(&self, cb: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().viewport = Some(cb);
    }
    fn resize(&mut self, _: Size<Pixels>) {}
    fn scale_factor(&self) -> f32 {
        self.0.scale.get()
    }
    fn appearance(&self) -> WindowAppearance {
        WindowAppearance::Light
    }
    fn display(&self) -> Option<Rc<dyn PlatformDisplay>> {
        None
    }
    fn mouse_position(&self) -> Point<Pixels> {
        Point::default()
    }
    fn modifiers(&self) -> Modifiers {
        Modifiers::default()
    }
    fn capslock(&self) -> Capslock {
        Capslock::default()
    }
    fn set_input_handler(&mut self, handler: PlatformInputHandler) {
        self.0.epoch.set(self.0.epoch.get() + 1);
        *self.0.input.borrow_mut() = Some(handler);
    }
    fn take_input_handler(&mut self) -> Option<PlatformInputHandler> {
        self.0.input.borrow_mut().take()
    }
    fn set_text_input_configuration(&mut self, config: TextInputConfiguration) {
        *self.0.config.borrow_mut() = config;
        self.0.host.call("inputConfigurationChanged", "()V", &[]);
    }
    fn prompt(
        &self,
        _: PromptLevel,
        msg: &str,
        detail: Option<&str>,
        answers: &[PromptButton],
    ) -> Option<oneshot::Receiver<usize>> {
        log::warn!(
            "Android native prompt not configured: {msg} {detail:?} ({} choices)",
            answers.len()
        );
        None
    }
    fn activate(&self) {}
    fn is_active(&self) -> bool {
        self.0.active.get()
    }
    fn visibility(&self) -> WindowVisibility {
        self.0.visible.get()
    }
    fn is_hovered(&self) -> bool {
        false
    }
    fn background_appearance(&self) -> WindowBackgroundAppearance {
        WindowBackgroundAppearance::Opaque
    }
    fn set_title(&mut self, title: &str) {
        self.0.host.text("setWindowTitle", title);
    }
    fn set_background_appearance(&self, _: WindowBackgroundAppearance) {}
    fn minimize(&self) {
        self.0.host.call("moveToBackground", "()V", &[]);
    }
    fn zoom(&self) {}
    fn toggle_fullscreen(&self) {}
    fn is_fullscreen(&self) -> bool {
        true
    }
    fn on_request_frame(&self, cb: Box<dyn FnMut(RequestFrameOptions)>) {
        self.0.callbacks.borrow_mut().frame = Some(cb);
        self.0.host.wake();
    }
    fn on_input(&self, cb: Box<dyn FnMut(PlatformInput) -> DispatchEventResult>) {
        self.0.callbacks.borrow_mut().input = Some(cb);
    }
    fn on_active_status_change(&self, cb: Box<dyn FnMut(bool)>) {
        self.0.callbacks.borrow_mut().active = Some(cb);
    }
    fn on_visibility_change(&self, cb: Box<dyn FnMut(WindowVisibility)>) {
        self.0.callbacks.borrow_mut().visibility = Some(cb);
    }
    fn on_hover_status_change(&self, _: Box<dyn FnMut(bool)>) {}
    fn on_resize(&self, cb: Box<dyn FnMut(Size<Pixels>, f32)>) {
        self.0.callbacks.borrow_mut().resize = Some(cb);
    }
    fn on_moved(&self, _: Box<dyn FnMut()>) {}
    fn on_display_changed(&self, _: Box<dyn FnMut()>) {}
    fn on_should_close(&self, _: Box<dyn FnMut() -> bool>) {}
    fn on_hit_test_window_control(&self, _: Box<dyn FnMut() -> Option<WindowControlArea>>) {}
    fn on_close(&self, _: Box<dyn FnOnce()>) {}
    fn on_appearance_changed(&self, _: Box<dyn FnMut()>) {}
    fn draw(&self, scene: &Scene) {
        if let Some(r) = self.0.renderer.borrow_mut().as_mut() {
            r.draw(scene);
            if r.needs_redraw() || r.device_lost() {
                self.0.force_render.set(true);
                self.0.host.wake();
            }
        }
    }
    fn schedule_frame(&self) {
        self.0.host.wake();
    }
    fn frame_waker(&self) -> Option<Rc<dyn Fn()>> {
        let host = self.0.host.clone();
        Some(Rc::new(move || host.wake()))
    }
    fn sprite_atlas(&self) -> Arc<dyn PlatformAtlas> {
        self.0
            .renderer
            .borrow()
            .as_ref()
            .expect("live GPU atlas before GPUI window creation")
            .sprite_atlas()
            .clone()
    }
    fn is_subpixel_rendering_supported(&self) -> bool {
        false
    }
    fn gpu_specs(&self) -> Option<GpuSpecs> {
        self.0
            .renderer
            .borrow()
            .as_ref()
            .and_then(WgpuRenderer::gpu_specs)
    }
    fn update_ime_position(&self, _: Bounds<Pixels>) {}
    fn insets(&self) -> WindowInsets {
        self.0.insets.borrow().clone()
    }
    fn on_insets_changed(&self, cb: Box<dyn FnMut(WindowInsets)>) {
        self.0.callbacks.borrow_mut().insets = Some(cb);
    }
    fn set_back_handler(&self, cb: Box<dyn FnMut()>) {
        self.0.callbacks.borrow_mut().back = Some(cb);
    }
    fn set_back_enabled(&self, enabled: bool) {
        self.0
            .host
            .call("backEnabled", "(Z)V", &[JValue::Bool(enabled as u8)]);
    }
    fn show_soft_keyboard(&self) {
        self.0.host.call("showKeyboard", "()V", &[]);
    }
    fn hide_soft_keyboard(&self) {
        self.0.host.call("hideKeyboard", "()V", &[]);
    }
    fn text_input_state_changed(&self, _: TextInputStateChange) {
        self.0.host.wake();
    }
    fn a11y_init(&self, callbacks: A11yCallbacks) {
        if let Some(tree) = (callbacks.activation)() {
            self.0.a11y_update(tree);
        }
        *self.0.a11y.borrow_mut() = Some(callbacks);
    }
    fn a11y_tree_update(&self, update: accesskit::TreeUpdate) {
        self.0.a11y_update(update);
    }
    fn a11y_update_window_bounds(&self) {
        self.0.host.call("accessibilityChanged", "()V", &[]);
    }
}
pub(crate) fn input_configuration() -> serde_json::Value {
    with_platform(|p| {
        p.state().map(|s|{let c=s.config.borrow();serde_json::json!({"autocorrect":c.autocorrect,"suggestions":c.suggestions,"capitalize":format!("{:?}",c.autocapitalize),"action":format!("{:?}",c.input_action)})}).unwrap_or_default()
    })
}
impl AndroidPlatform {
    pub(crate) fn host_wake(&self) {
        self.host.wake();
    }
}
/// Internal app files directory, available after `android_main!` initialization.
pub fn data_directory() -> Result<PathBuf> {
    with_platform(|p| {
        let mut env = p.host.vm.attach_current_thread()?;
        let dir = env
            .call_method(
                p.host.activity.as_obj(),
                "getFilesDir",
                "()Ljava/io/File;",
                &[],
            )?
            .l()?;
        let path = env
            .call_method(dir, "getAbsolutePath", "()Ljava/lang/String;", &[])?
            .l()?;
        let path: String = env.get_string(&jni::objects::JString::from(path))?.into();
        Ok(PathBuf::from(path))
    })
}

/// Releases the embedded application on Android's main thread before its host dies.
pub(crate) fn shutdown() {
    if is_initialized() {
        with_platform(|p| p.detach());
    }
    APP.with(|app| {
        app.borrow_mut().take();
    });
    PLATFORM.with(|platform| {
        platform.borrow_mut().take();
    });
}
