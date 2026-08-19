use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Render, Window,
};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use ui::prelude::*;
use ui::{
    Button, Color, CommonAnimationExt, Icon, IconName, IconSize, Label, LabelCommon, LabelSize,
    ProgressBar,
};

use crate::{DismissDecision, ModalView, Workspace};

/// Blocking progress dialog for user-triggered operations that may continue
/// while Zdroid-B is in the background. The matching Android foreground task
/// notification is managed by the caller.
pub struct BackgroundOperationModal {
    title: SharedString,
    message: SharedString,
    focus_handle: FocusHandle,
    progress_percent: Option<u8>,
    cancel_flag: Option<Arc<AtomicBool>>,
    completed: bool,
}

impl BackgroundOperationModal {
    pub fn show(
        workspace: &mut Workspace,
        title: impl Into<SharedString>,
        message: impl Into<SharedString>,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Entity<Self>> {
        let title = title.into();
        let message = message.into();
        workspace.toggle_modal(window, cx, move |_, cx| Self {
            title,
            message,
            focus_handle: cx.focus_handle(),
            progress_percent: None,
            cancel_flag: None,
            completed: false,
        });
        let modal = workspace.active_modal::<Self>(cx);
        if let Some(modal) = &modal {
            let focus_handle = modal.read(cx).focus_handle.clone();
            window.focus(&focus_handle, cx);
            if window.soft_keyboard_visible() {
                window.toggle_soft_keyboard();
            }
        }
        modal
    }

    pub fn set_message(&mut self, message: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.message = message.into();
        self.progress_percent = None;
        cx.notify();
    }

    pub fn set_progress(
        &mut self,
        message: impl Into<SharedString>,
        percent: Option<u8>,
        cx: &mut Context<Self>,
    ) {
        self.message = message.into();
        self.progress_percent = percent;
        cx.notify();
    }

    pub fn set_cancel_flag(&mut self, cancel_flag: Arc<AtomicBool>, cx: &mut Context<Self>) {
        self.cancel_flag = Some(cancel_flag);
        cx.notify();
    }

    pub fn complete(&mut self, cx: &mut Context<Self>) {
        self.completed = true;
        cx.emit(DismissEvent);
    }
}

impl EventEmitter<DismissEvent> for BackgroundOperationModal {}

impl Focusable for BackgroundOperationModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl ModalView for BackgroundOperationModal {
    fn show_close_button(&self) -> bool {
        false
    }

    fn on_before_dismiss(&mut self, _: &mut Window, _: &mut Context<Self>) -> DismissDecision {
        DismissDecision::Dismiss(self.completed)
    }
}

impl Render for BackgroundOperationModal {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("zdroid-background-operation")
            .w_full()
            .min_w_0()
            .p_5()
            .gap_4()
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .child(
                        Icon::new(IconName::LoadCircle)
                            .size(IconSize::Medium)
                            .color(Color::Accent)
                            .with_rotate_animation(2),
                    )
                    .child(
                        Label::new(self.title.clone())
                            .size(LabelSize::Large)
                            .line_clamp(2),
                    ),
            )
            .child(
                div().w_full().min_w_0().child(
                    Label::new(self.message.clone())
                        .size(LabelSize::Small)
                        .color(Color::Muted)
                        .line_clamp(6),
                ),
            )
            .when_some(self.progress_percent, |this, percent| {
                this.child(ProgressBar::new(
                    "zdroid-background-operation-progress",
                    percent as f32,
                    100.0,
                    cx,
                ))
            })
            .when_some(self.cancel_flag.clone(), |this, cancel_flag| {
                this.child(h_flex().w_full().mt_auto().pt_2().justify_end().child(
                    Button::new("cancel-zdroid-background-operation", "Cancel").on_click(
                        move |_, _, _| {
                            cancel_flag.store(true, Ordering::Relaxed);
                        },
                    ),
                ))
            })
    }
}
