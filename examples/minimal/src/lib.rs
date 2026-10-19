#[cfg(target_os = "android")]
mod mobile {
    use gpui::*;
    use gpui_android::input::InputSnapshot;
    use std::ops::Range;

    /// A deliberately small input model for exercising the native IME contract.
    /// Production controls should paint their own caret and selection.
    struct Demo {
        focus: FocusHandle,
        text: String,
        selection: Range<usize>,
        marked: Option<Range<usize>>,
    }
    impl Demo {
        fn snapshot(&self) -> InputSnapshot {
            InputSnapshot {
                text: self.text.clone(),
                selection: self.selection.clone(),
                composing: self.marked.clone(),
                enabled: true,
                offset: 0,
            }
        }
        fn replace(
            &mut self,
            range: Option<Range<usize>>,
            text: &str,
            marked: bool,
            cx: &mut Context<Self>,
        ) {
            let state = self.snapshot();
            let range = state.clamp(range.unwrap_or_else(|| state.replacement()));
            self.text = format!(
                "{}{}{}",
                state.slice(0..range.start),
                text,
                state.slice(range.end..state.end())
            );
            let end = range.start + text.encode_utf16().count();
            self.selection = end..end;
            self.marked = (marked && !text.is_empty()).then_some(range.start..end);
            cx.notify();
        }
    }
    impl EntityInputHandler for Demo {
        fn text_for_range(
            &mut self,
            range: Range<usize>,
            adjusted: &mut Option<Range<usize>>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<String> {
            let state = self.snapshot();
            let range = state.clamp(range);
            *adjusted = Some(range.clone());
            Some(state.slice(range))
        }
        fn selected_text_range(
            &mut self,
            _: bool,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<UTF16Selection> {
            Some(UTF16Selection {
                range: self.selection.clone(),
                reversed: false,
            })
        }
        fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
            self.marked.clone()
        }
        fn unmark_text(&mut self, _: &mut Window, cx: &mut Context<Self>) {
            self.marked = None;
            cx.notify();
        }
        fn replace_text_in_range(
            &mut self,
            range: Option<Range<usize>>,
            text: &str,
            _: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.replace(range, text, false, cx);
        }
        fn replace_and_mark_text_in_range(
            &mut self,
            range: Option<Range<usize>>,
            text: &str,
            selected: Option<Range<usize>>,
            _: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.replace(range, text, true, cx);
            if let Some(selected) = selected {
                let origin = self
                    .marked
                    .as_ref()
                    .map_or(self.selection.start, |r| r.start);
                self.selection = self
                    .snapshot()
                    .clamp(origin + selected.start..origin + selected.end);
            }
        }
        fn set_selected_text_range(
            &mut self,
            range: Range<usize>,
            _: &mut Window,
            cx: &mut Context<Self>,
        ) {
            self.selection = self.snapshot().clamp(range);
            cx.notify();
        }
        fn text_length_utf16(&mut self, _: &mut Window, _: &mut Context<Self>) -> Option<usize> {
            Some(self.text.encode_utf16().count())
        }
        fn bounds_for_range(
            &mut self,
            _: Range<usize>,
            bounds: Bounds<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<Bounds<Pixels>> {
            Some(bounds)
        }
        fn character_index_for_point(
            &mut self,
            _: Point<Pixels>,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> Option<usize> {
            Some(self.selection.end)
        }
        fn text_input_configuration(
            &mut self,
            _: &mut Window,
            _: &mut Context<Self>,
        ) -> TextInputConfiguration {
            TextInputConfiguration {
                autocorrect: true,
                suggestions: true,
                autocapitalize: Autocapitalize::Sentences,
                input_action: TextInputAction::Done,
            }
        }
    }
    impl Render for Demo {
        fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
            let entity = cx.entity();
            let focus = self.focus.clone();
            div()
                .id("demo")
                .size_full()
                .bg(rgb(0x101827))
                .text_color(rgb(0xffffff))
                .p_8()
                .rtl()
                .flex()
                .flex_col()
                .gap_4()
                .child(div().text_2xl().child("مرحباً من GPUI على أندرويد"))
                .child(div().child("Arabic shaping • Fast scroll tiles • Native touch"))
                .child(
                    div()
                        .id("input")
                        .track_focus(&self.focus)
                        .bg(rgb(0x243247))
                        .rounded_md()
                        .p_3()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| {
                                window.focus(&this.focus, cx);
                                window.request_virtual_keyboard();
                            }),
                        )
                        .child(self.text.clone())
                        .child(
                            canvas(
                                |_, _, _| (),
                                move |bounds, (), window, cx| {
                                    window.handle_input(
                                        &focus,
                                        ElementInputHandler::new(bounds, entity),
                                        cx,
                                    )
                                },
                            )
                            .w_full()
                            .h_1(),
                        ),
                )
                .child(
                    div()
                        .id("scroll")
                        .overflow_y_scroll()
                        .flex_1()
                        .children((0..200).map(|i| {
                            div()
                                .p_3()
                                .border_b_1()
                                .border_color(rgb(0x334155))
                                .child(format!("السطر {i} — Hello مرحباً 😀"))
                        })),
                )
        }
    }
    pub fn launch(cx: &mut App) {
        cx.open_window(WindowOptions::default(), |window, cx| {
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                Demo {
                    focus,
                    text: "اضغط هنا للكتابة — Hello 😀".into(),
                    selection: 0..0,
                    marked: None,
                }
            })
        })
        .expect("Android GPUI window");
    }
}
#[cfg(target_os = "android")]
gpui_android::android_main!(mobile::launch);
