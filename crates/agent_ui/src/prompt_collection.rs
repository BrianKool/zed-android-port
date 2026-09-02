use editor::Editor;
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, SharedString, Window,
    prelude::*,
};
use prompt_store::{PromptId, PromptMetadata, PromptStore, UserPromptId};
use ui::{
    Button, ButtonStyle, Icon, IconButton, IconName, IconSize, Label, ListItem, ListSeparator,
    Tooltip, prelude::*,
};
use workspace::{ModalView, Workspace};

use crate::{ManagePromptCollection, message_editor::MessageEditor};

#[derive(Clone, Copy, PartialEq, Eq)]
enum PromptCollectionMode {
    Browse,
    Edit,
}

pub struct PromptCollectionModal {
    target: Option<Entity<MessageEditor>>,
    store: Option<Entity<PromptStore>>,
    prompts: Vec<PromptMetadata>,
    selected: Option<UserPromptId>,
    search: Entity<Editor>,
    name: Entity<Editor>,
    body: Entity<Editor>,
    focus_handle: FocusHandle,
    loading: bool,
    mode: PromptCollectionMode,
}

impl PromptCollectionModal {
    pub fn register(
        workspace: &mut Workspace,
        _window: Option<&mut Window>,
        _cx: &mut Context<Workspace>,
    ) {
        workspace.register_action(|workspace, _: &ManagePromptCollection, window, cx| {
            Self::open(workspace, None, window, cx);
        });
    }

    pub fn open(
        workspace: &mut Workspace,
        target: Option<Entity<MessageEditor>>,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        workspace.toggle_modal(window, cx, move |window, cx| Self::new(target, window, cx));
    }

    fn new(
        target: Option<Entity<MessageEditor>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Search prompts", window, cx);
            editor
        });
        let name = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text("Prompt name", window, cx);
            editor
        });
        let body = cx.new(|cx| {
            let mut editor = Editor::multi_line(window, cx);
            editor.set_placeholder_text("Prompt", window, cx);
            editor.set_auto_show_ime(true);
            editor
        });
        cx.subscribe(&search, |_, _, event: &editor::EditorEvent, cx| {
            if matches!(event, editor::EditorEvent::BufferEdited) {
                cx.notify();
            }
        })
        .detach();

        let mut this = Self {
            target,
            store: None,
            prompts: Vec::new(),
            selected: None,
            search,
            name,
            body,
            focus_handle: cx.focus_handle(),
            loading: true,
            mode: PromptCollectionMode::Browse,
        };
        this.load_store(cx);
        this
    }

    fn load_store(&mut self, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            let store_task = cx.update(|cx| PromptStore::global(cx));
            let store = store_task.await?;
            this.update(cx, |this, cx| {
                this.store = Some(store.clone());
                this.prompts = store.read(cx).all_prompt_metadata();
                this.loading = false;
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn new_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = None;
        self.mode = PromptCollectionMode::Edit;
        self.name
            .update(cx, |editor, cx| editor.set_text("", window, cx));
        self.body
            .update(cx, |editor, cx| editor.set_text("", window, cx));
        self.name.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn select(&mut self, metadata: PromptMetadata, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = metadata.id.as_user() else {
            return;
        };
        let Some(store) = self.store.clone() else {
            return;
        };
        self.selected = Some(id);
        self.loading = true;
        cx.spawn_in(window, async move |this, cx| {
            let body_result = store
                .update(cx, |store, cx| store.load(PromptId::from(id), cx))
                .await;
            match body_result {
                Ok(body) => this.update_in(cx, |this, window, cx| {
                    if let Some(target) = this.target.clone() {
                        target.focus_handle(cx).focus(window, cx);
                        target.update(cx, |editor, cx| editor.insert_text(&body, window, cx));
                        this.loading = false;
                        cx.emit(DismissEvent);
                        return;
                    }
                    this.name.update(cx, |editor, cx| {
                        editor.set_text(metadata.title.as_deref().unwrap_or_default(), window, cx)
                    });
                    this.body
                        .update(cx, |editor, cx| editor.set_text(body.as_str(), window, cx));
                    this.loading = false;
                    this.mode = PromptCollectionMode::Edit;
                    this.name.focus_handle(cx).focus(window, cx);
                    cx.notify();
                })?,
                Err(error) => {
                    log::error!("failed to load prompt {id:?}: {error:#}");
                    this.update(cx, |this, cx| {
                        this.loading = false;
                        this.selected = None;
                        cx.notify();
                    })?;
                }
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn show_collection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = PromptCollectionMode::Browse;
        self.selected = None;
        self.search.focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(store) = self.store.clone() else {
            return;
        };
        let title = self.name.read(cx).text(cx).trim().to_string();
        let body = self.body.read(cx).text(cx).trim().to_string();
        if title.is_empty() || body.is_empty() {
            return;
        }
        let id = self.selected;
        self.loading = true;
        cx.spawn_in(window, async move |this, cx| {
            store
                .update(cx, |store, cx| store.save(id, title.into(), body, cx))
                .await?;
            store.update(cx, |store, _| store.refresh_metadata())?;
            this.update(cx, |this, cx| {
                this.prompts = store.read(cx).all_prompt_metadata();
                this.loading = false;
                this.mode = PromptCollectionMode::Browse;
                this.selected = None;
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(store), Some(id)) = (self.store.clone(), self.selected) else {
            return;
        };
        self.loading = true;
        cx.spawn_in(window, async move |this, cx| {
            store.update(cx, |store, cx| store.delete(id, cx)).await?;
            store.update(cx, |store, _| store.refresh_metadata())?;
            this.update_in(cx, |this, window, cx| {
                this.prompts = store.read(cx).all_prompt_metadata();
                this.loading = false;
                this.mode = PromptCollectionMode::Browse;
                this.selected = None;
                this.name
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
                this.body
                    .update(cx, |editor, cx| editor.set_text("", window, cx));
                this.search.focus_handle(cx).focus(window, cx);
                cx.notify();
            })?;
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn use_prompt(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.body.read(cx).text(cx);
        let Some(target) = self.target.clone() else {
            return;
        };
        target.focus_handle(cx).focus(window, cx);
        target.update(cx, |editor, cx| editor.insert_text(&text, window, cx));
        cx.emit(DismissEvent);
    }
}

impl Render for PromptCollectionModal {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).text(cx).to_lowercase();
        let prompts = self
            .prompts
            .iter()
            .filter(|metadata| {
                !metadata.default
                    && metadata
                        .title
                        .as_ref()
                        .is_some_and(|title| title.to_lowercase().contains(&query))
            })
            .cloned()
            .collect::<Vec<_>>();
        let content = match self.mode {
            PromptCollectionMode::Browse => {
                let selecting_prompt = self.target.is_some();
                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .p_3()
                            .pr_12()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Icon::new(IconName::File).size(IconSize::Small))
                                    .child(Label::new(if selecting_prompt {
                                        "Saved Prompts"
                                    } else {
                                        "Prompt Collection"
                                    })),
                            )
                            .when(!selecting_prompt, |this| {
                                this.child(
                                    Button::new("new-prompt", "New")
                                        .style(ButtonStyle::Filled)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.new_prompt(window, cx)
                                        })),
                                )
                            }),
                    )
                    .child(
                        div().px_3().pb_3().child(
                            h_flex()
                                .h_9()
                                .w_full()
                                .min_w_0()
                                .gap_2()
                                .px_2()
                                .rounded_md()
                                .border_1()
                                .border_color(cx.theme().colors().border)
                                .bg(cx.theme().colors().editor_background)
                                .child(
                                    IconButton::new(
                                        "focus-prompt-collection-search",
                                        IconName::MagnifyingGlass,
                                    )
                                    .icon_size(IconSize::Small)
                                    .tooltip(Tooltip::text("Search prompts"))
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.search.focus_handle(cx).focus(window, cx);
                                        },
                                    )),
                                )
                                .child(div().min_w_0().flex_1().child(self.search.clone())),
                        ),
                    )
                    .child(ListSeparator)
                    .child(
                        v_flex()
                            .id("prompt-collection-list")
                            .min_h_0()
                            .flex_1()
                            .overflow_y_scroll()
                            .p_2()
                            .gap_1()
                            .when(prompts.is_empty() && !self.loading, |this| {
                                this.child(
                                    div()
                                        .p_4()
                                        .child(Label::new("No prompts found").color(Color::Muted)),
                                )
                            })
                            .children(prompts.into_iter().map(|metadata| {
                                let title: SharedString =
                                    metadata.title.clone().unwrap_or_else(|| "Untitled".into());
                                ListItem::new(format!("prompt-{}", metadata.id))
                                    .child(Label::new(title))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.select(metadata.clone(), window, cx)
                                    }))
                            })),
                    )
                    .into_any_element()
            }
            PromptCollectionMode::Edit => {
                let has_target = self.target.is_some();
                let can_delete = self.selected.is_some();
                let title = if can_delete {
                    "Edit Prompt"
                } else {
                    "New Prompt"
                };

                v_flex()
                    .size_full()
                    .child(
                        h_flex()
                            .p_3()
                            .pr_12()
                            .gap_2()
                            .child(
                                IconButton::new("back-to-prompt-collection", IconName::ArrowLeft)
                                    .tooltip(Tooltip::text("Back to Prompt Collection"))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.show_collection(window, cx)
                                    })),
                            )
                            .child(Label::new(title)),
                    )
                    .child(ListSeparator)
                    .child(
                        v_flex()
                            .id("prompt-collection-editor")
                            .min_h_0()
                            .flex_1()
                            .overflow_hidden()
                            .p_3()
                            .gap_3()
                            .child(
                                v_flex()
                                    .w_full()
                                    .flex_none()
                                    .gap_1()
                                    .child(
                                        Label::new("Name")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        h_flex()
                                            .id("prompt-collection-name-field")
                                            .w_full()
                                            .h_9()
                                            .min_h_9()
                                            .min_w_0()
                                            .flex_none()
                                            .px_2()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(cx.theme().colors().border_variant)
                                            .bg(cx.theme().colors().editor_background)
                                            .child(
                                                div().min_w_0().flex_1().child(self.name.clone()),
                                            ),
                                    ),
                            )
                            .child(
                                v_flex()
                                    .min_h_0()
                                    .flex_1()
                                    .gap_1()
                                    .child(
                                        Label::new("Prompt")
                                            .size(LabelSize::Small)
                                            .color(Color::Muted),
                                    )
                                    .child(
                                        div()
                                            .id("prompt-collection-body-field")
                                            .w_full()
                                            .min_h_0()
                                            .flex_1()
                                            .overflow_hidden()
                                            .p_2()
                                            .rounded_md()
                                            .border_1()
                                            .border_color(cx.theme().colors().border_variant)
                                            .bg(cx.theme().colors().editor_background)
                                            .child(
                                                div()
                                                    .size_full()
                                                    .min_h_0()
                                                    .min_w_0()
                                                    .child(self.body.clone()),
                                            ),
                                    ),
                            ),
                    )
                    .child(ListSeparator)
                    .child(
                        h_flex()
                            .p_3()
                            .justify_between()
                            .child(
                                Button::new("delete-prompt", "Delete")
                                    .style(ButtonStyle::Subtle)
                                    .disabled(!can_delete || self.loading)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.delete(window, cx)),
                                    ),
                            )
                            .child(
                                h_flex()
                                    .gap_2()
                                    .when(has_target && can_delete, |this| {
                                        this.child(
                                            Button::new("use-prompt", "Insert")
                                                .style(ButtonStyle::Subtle)
                                                .disabled(self.loading)
                                                .on_click(cx.listener(|this, _, window, cx| {
                                                    this.use_prompt(window, cx)
                                                })),
                                        )
                                    })
                                    .child(
                                        Button::new(
                                            "save-prompt",
                                            if self.loading { "Saving..." } else { "Save" },
                                        )
                                        .style(ButtonStyle::Filled)
                                        .disabled(self.loading)
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                // Commit any active Android composition before
                                                // reading the editor buffers for persistence.
                                                this.focus_handle.focus(window, cx);
                                                cx.defer_in(window, |this, window, cx| {
                                                    this.save(window, cx)
                                                });
                                            }),
                                        ),
                                    ),
                            ),
                    )
                    .into_any_element()
            }
        };

        v_flex()
            .size_full()
            .key_context("PromptCollectionModal")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &menu::Cancel, _, cx| cx.emit(DismissEvent)))
            .on_mouse_down_out(cx.listener(|_, _, _, cx| cx.emit(DismissEvent)))
            .child(content)
    }
}

impl Focusable for PromptCollectionModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}
impl EventEmitter<DismissEvent> for PromptCollectionModal {}
impl ModalView for PromptCollectionModal {
    fn android_full_size(&self) -> bool {
        true
    }
}
