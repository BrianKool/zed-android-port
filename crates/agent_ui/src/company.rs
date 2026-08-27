use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use agent_client_protocol::schema::v1 as acp;
use chrono::{Local, TimeZone as _, Utc};
use db::kvp::KeyValueStore;
use editor::{Editor, HighlightKey, NavigationOverlayKey};
use gpui::{
    App, Context, DismissEvent, Entity, EventEmitter, FocusHandle, Focusable, Global,
    HighlightStyle, MouseButton, MouseDownEvent, Render, ScrollAnchor, ScrollHandle, SharedString,
    Subscription, TaskExt, WeakEntity, Window, prelude::*, px,
};
use multi_buffer::MultiBufferOffset;
use serde::{Deserialize, Serialize};
use ui::{
    Button, ButtonStyle, Checkbox, Color, Headline, HeadlineSize, Icon, IconButton, IconName,
    Label, LabelSize, TintColor, ToggleState, Tooltip, WithScrollbar, prelude::*, vh, vw,
};
use uuid::Uuid;
use workspace::ModalView;

use crate::{
    AgentPanel,
    thread_metadata_store::{ThreadId, ThreadMetadataStore},
};

const COMPANY_STORE_KEY: &str = "zdroid_agent_companies_v1";

#[derive(Clone, Default)]
struct GlobalCompanyStore(CompanyStoreData);

impl Global for GlobalCompanyStore {}

enum CompanyMentionHighlight {}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct CompanyStoreData {
    #[serde(default)]
    pub companies: Vec<Company>,
    #[serde(default)]
    pub personnel: Vec<CompanyMember>,
    #[serde(default)]
    pub company_thread_ids: HashSet<ThreadId>,
    #[serde(default)]
    pub personnel_thread_ids: HashMap<ThreadId, Uuid>,
    #[serde(default)]
    pub sessions: Vec<CompanySessionRecord>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Company {
    pub id: Uuid,
    pub name: String,
    #[serde(default)]
    pub rules: String,
    #[serde(default)]
    pub skills: String,
    #[serde(default)]
    pub members: Vec<CompanyMember>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompanyMember {
    pub id: Uuid,
    pub name: String,
    pub agent_id: String,
    pub model: Option<String>,
    pub role: String,
    pub persona: String,
    pub skills: String,
    pub rules: String,
    pub boundaries: String,
    pub workflow: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompanySessionKind {
    #[default]
    Meeting,
    Task,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompanySessionStatus {
    #[default]
    Running,
    WaitingForUser,
    Completed,
    Failed,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompanyTimelineEntryKind {
    User,
    Personnel,
    System,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompanyTimelineEntry {
    pub id: Uuid,
    pub kind: CompanyTimelineEntryKind,
    pub personnel_id: Option<Uuid>,
    pub speaker: String,
    pub content: String,
    pub worker_thread_id: Option<ThreadId>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub enum CompanyWorkItemStatus {
    #[default]
    Backlog,
    Discussing,
    Ready,
    InProgress,
    Review,
    Done,
    Blocked,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompanyWorkItem {
    pub id: Uuid,
    pub title: String,
    pub description: String,
    pub status: CompanyWorkItemStatus,
    #[serde(default)]
    pub assignee_ids: Vec<Uuid>,
    #[serde(default)]
    pub worker_thread_ids: Vec<ThreadId>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CompanySessionRecord {
    pub id: Uuid,
    pub company_id: Uuid,
    pub company_name: String,
    pub kind: CompanySessionKind,
    pub title: String,
    pub description: String,
    #[serde(default)]
    pub started_at_unix_ms: i64,
    #[serde(default)]
    pub archived: bool,
    #[serde(default)]
    pub personnel_ids: Vec<Uuid>,
    #[serde(default)]
    pub attachments: Vec<PathBuf>,
    pub status: CompanySessionStatus,
    #[serde(default)]
    pub timeline: Vec<CompanyTimelineEntry>,
    #[serde(default)]
    pub worker_thread_ids: Vec<ThreadId>,
    #[serde(default)]
    pub primary_thread_id: Option<ThreadId>,
    #[serde(default)]
    pub work_items: Vec<CompanyWorkItem>,
    #[serde(default)]
    pub pending_user_context: Vec<String>,
    #[serde(default)]
    pub context_blocks: Vec<acp::ContentBlock>,
    #[serde(default)]
    pub stop_requested: bool,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub runner_active: bool,
    #[serde(default)]
    pub runner_generation: u64,
    #[serde(default)]
    pub active_personnel_ids: Vec<Uuid>,
    #[serde(default)]
    pub waiting_personnel_ids: Vec<Uuid>,
    #[serde(default)]
    pub completed_personnel_ids: Vec<Uuid>,
    #[serde(default)]
    pub failed_personnel_ids: Vec<Uuid>,
    #[serde(default)]
    pub completion_condition: String,
    #[serde(default)]
    pub meeting_turn_limit: Option<usize>,
    #[serde(default = "default_true")]
    pub allow_clarifying_questions: bool,
}

fn default_true() -> bool {
    true
}

#[derive(Clone, Debug)]
pub struct CompanySessionRequest {
    pub company: Company,
    pub kind: CompanySessionKind,
    pub title: String,
    pub description: String,
    pub member_ids: Vec<Uuid>,
    pub attachments: Vec<PathBuf>,
    pub completion_condition: String,
    pub meeting_turn_limit: Option<usize>,
    pub allow_clarifying_questions: bool,
}

#[derive(Clone, Debug)]
pub struct CompanyAgentOption {
    pub id: String,
    pub name: SharedString,
    pub models: Vec<(String, SharedString)>,
}

pub fn load_companies(cx: &App) -> CompanyStoreData {
    if let Some(store) = cx.try_global::<GlobalCompanyStore>() {
        return store.0.clone();
    }

    let mut data: CompanyStoreData = KeyValueStore::global(cx)
        .read_kvp(COMPANY_STORE_KEY)
        .ok()
        .flatten()
        .and_then(|json| serde_json::from_str(&json).ok())
        .unwrap_or_default();

    // Migrate the original company-owned member records into the global
    // personnel directory. Companies retain ID-matched snapshots for storage
    // compatibility, while the global record is authoritative.
    for member in data
        .companies
        .iter()
        .flat_map(|company| company.members.iter())
        .cloned()
        .collect::<Vec<_>>()
    {
        if !data.personnel.iter().any(|person| person.id == member.id) {
            data.personnel.push(member);
        }
    }
    for company in &mut data.companies {
        for member in &mut company.members {
            if let Some(person) = data.personnel.iter().find(|person| person.id == member.id) {
                *member = person.clone();
            }
        }
    }
    data
}

fn save_companies(data: &CompanyStoreData, cx: &mut App) {
    if cx.has_global::<GlobalCompanyStore>() {
        cx.global_mut::<GlobalCompanyStore>().0 = data.clone();
    } else {
        cx.set_global(GlobalCompanyStore(data.clone()));
    }

    let Ok(json) = serde_json::to_string(data) else {
        log::error!("failed to serialize Zdroid company settings");
        return;
    };
    let kvp = KeyValueStore::global(cx);
    db::write_and_log(cx, move || async move {
        kvp.write_kvp(COMPANY_STORE_KEY.to_string(), json).await
    });
}

pub fn refresh_personnel_mention_highlights(
    editor: &Entity<Editor>,
    personnel: &[CompanyMember],
    cx: &mut App,
) {
    let text = editor.read(cx).text(cx);
    let byte_ranges = personnel
        .iter()
        .flat_map(|person| {
            let mention = format!("@{}", person.name);
            text.match_indices(&mention)
                .map(move |(start, value)| start..start + value.len())
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();

    editor.update(cx, |editor, cx| {
        let snapshot = editor.buffer().read(cx).snapshot(cx);
        let ranges = byte_ranges
            .into_iter()
            .map(|range| {
                snapshot.anchor_before(MultiBufferOffset(range.start))
                    ..snapshot.anchor_after(MultiBufferOffset(range.end))
            })
            .collect();
        editor.highlight_text(
            HighlightKey::NavigationOverlay(
                NavigationOverlayKey::unique::<CompanyMentionHighlight>(),
            ),
            ranges,
            HighlightStyle {
                color: Some(cx.theme().colors().text_accent),
                ..Default::default()
            },
            cx,
        );
    });
}

pub fn update_company_session(
    session_id: Uuid,
    cx: &mut App,
    update: impl FnOnce(&mut CompanySessionRecord),
) {
    let mut data = load_companies(cx);
    let Some(session) = data
        .sessions
        .iter_mut()
        .find(|session| session.id == session_id)
    else {
        return;
    };
    update(session);
    save_companies(&data, cx);
}

pub fn create_company_session(session: CompanySessionRecord, cx: &mut App) {
    let mut data = load_companies(cx);
    data.sessions.push(session);
    save_companies(&data, cx);
}

pub fn delete_company_session(session_id: Uuid, cx: &mut App) -> Vec<ThreadId> {
    let mut data = load_companies(cx);
    let Some(index) = data
        .sessions
        .iter()
        .position(|session| session.id == session_id)
    else {
        return Vec::new();
    };
    let session = data.sessions.remove(index);
    let mut thread_ids = session.worker_thread_ids;
    if let Some(primary_thread_id) = session.primary_thread_id
        && !thread_ids.contains(&primary_thread_id)
    {
        thread_ids.push(primary_thread_id);
    }
    for thread_id in &thread_ids {
        data.company_thread_ids.remove(thread_id);
    }
    save_companies(&data, cx);
    thread_ids
}

pub fn mark_company_threads(thread_ids: impl IntoIterator<Item = ThreadId>, cx: &mut App) {
    let mut data = load_companies(cx);
    data.company_thread_ids.extend(thread_ids);
    save_companies(&data, cx);
}

pub fn mark_personnel_thread(thread_id: ThreadId, personnel_id: Uuid, cx: &mut App) {
    let mut data = load_companies(cx);
    data.company_thread_ids.insert(thread_id);
    data.personnel_thread_ids.insert(thread_id, personnel_id);
    save_companies(&data, cx);
}

enum CompanyModalMode {
    List,
    PersonnelList,
    Sessions {
        company_id: Uuid,
    },
    EditCompany {
        company_id: Option<Uuid>,
    },
    EditMember {
        company_id: Option<Uuid>,
        member_id: Option<Uuid>,
    },
    StartSession {
        company_id: Uuid,
    },
    AddPersonnel {
        company_id: Uuid,
        session_id: Uuid,
    },
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum CompanySessionFilter {
    #[default]
    Active,
    Complete,
    Archive,
}

pub struct CompanyModal {
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    mention_scroll_handle: ScrollHandle,
    panel: WeakEntity<AgentPanel>,
    data: CompanyStoreData,
    mode: CompanyModalMode,
    agent_options: Vec<CompanyAgentOption>,
    company_name: Entity<Editor>,
    company_rules: Entity<Editor>,
    company_skills: Entity<Editor>,
    member_name: Entity<Editor>,
    persona: Entity<Editor>,
    skills: Entity<Editor>,
    rules: Entity<Editor>,
    boundaries: Entity<Editor>,
    workflow: Entity<Editor>,
    field_help_dialog: Option<(&'static str, &'static str)>,
    member_form_error: Option<SharedString>,
    selected_agent_id: String,
    selected_model: Option<String>,
    session_title: Entity<Editor>,
    session_description: Entity<Editor>,
    _session_description_subscription: Subscription,
    completion_condition: Entity<Editor>,
    meeting_turn_limit: Entity<Editor>,
    manual_stop_only: bool,
    allow_clarifying_questions: bool,
    session_kind: CompanySessionKind,
    selected_members: HashSet<Uuid>,
    attachments: Vec<PathBuf>,
    session_filter: CompanySessionFilter,
}

pub struct CompanyAgendaModal {
    focus_handle: FocusHandle,
    title: SharedString,
    content: SharedString,
    scroll_handle: ScrollHandle,
}

impl CompanyAgendaModal {
    pub fn new(title: SharedString, content: SharedString, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            title,
            content,
            scroll_handle: ScrollHandle::new(),
        }
    }
}

impl Render for CompanyAgendaModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("company-agenda-dialog")
            .relative()
            .w(vw(0.90, window))
            .h(vh(0.90, window))
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .p_4()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(Icon::new(IconName::Building2))
                            .child(Headline::new(self.title.clone()).size(HeadlineSize::Small)),
                    )
                    .child(
                        IconButton::new("close-company-agenda", IconName::Close)
                            .tooltip(Tooltip::text("Close"))
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("company-agenda-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .p_4()
                            .child(Label::new(self.content.clone())),
                    )
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
    }
}

impl ModalView for CompanyAgendaModal {}

impl Focusable for CompanyAgendaModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for CompanyAgendaModal {}

impl CompanyModal {
    fn single_line_editor(
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_placeholder_text(placeholder, window, cx);
            editor
        })
    }

    fn multi_line_editor(
        placeholder: &'static str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<Editor> {
        cx.new(|cx| {
            let mut editor = Editor::auto_height(3, 10, window, cx);
            editor.set_placeholder_text(placeholder, window, cx);
            editor
        })
    }

    pub fn new(
        panel: WeakEntity<AgentPanel>,
        agent_options: Vec<CompanyAgentOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let selected_agent_id = agent_options
            .first()
            .map(|agent| agent.id.clone())
            .unwrap_or_else(|| agent::ZED_AGENT_ID.to_string());
        let session_description =
            Self::multi_line_editor("Meeting description and desired outcome", window, cx);
        let _session_description_subscription = cx.subscribe(
            &session_description,
            |this, editor, event: &editor::EditorEvent, cx| {
                if matches!(event, editor::EditorEvent::BufferEdited) {
                    let members = match this.mode {
                        CompanyModalMode::StartSession { company_id } => this
                            .data
                            .companies
                            .iter()
                            .find(|company| company.id == company_id)
                            .map(|company| company.members.clone())
                            .unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    refresh_personnel_mention_highlights(&editor, &members, cx);
                    cx.notify();
                }
            },
        );

        Self {
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
            mention_scroll_handle: ScrollHandle::new(),
            panel,
            data: load_companies(cx),
            mode: CompanyModalMode::List,
            agent_options,
            company_name: Self::single_line_editor("Company or team name", window, cx),
            company_rules: Self::multi_line_editor("Company-wide rules", window, cx),
            company_skills: Self::multi_line_editor(
                "Company-wide installed /skill commands, one per line",
                window,
                cx,
            ),
            member_name: Self::single_line_editor("Personnel name", window, cx),
            persona: Self::multi_line_editor("Persona and pre-configuration", window, cx),
            skills: Self::multi_line_editor("Installed /skill commands, one per line", window, cx),
            rules: Self::multi_line_editor("Rules", window, cx),
            boundaries: Self::multi_line_editor("Boundaries", window, cx),
            workflow: Self::multi_line_editor("Workflow", window, cx),
            field_help_dialog: None,
            member_form_error: None,
            selected_agent_id,
            selected_model: None,
            session_title: Self::single_line_editor("Meeting or task name", window, cx),
            session_description,
            _session_description_subscription,
            completion_condition: Self::multi_line_editor(
                "Completion condition, such as reaching agreement on the API contract",
                window,
                cx,
            ),
            meeting_turn_limit: Self::single_line_editor("Maximum AI messages", window, cx),
            manual_stop_only: false,
            allow_clarifying_questions: true,
            session_kind: CompanySessionKind::Meeting,
            selected_members: HashSet::default(),
            attachments: Vec::new(),
            session_filter: CompanySessionFilter::Active,
        }
    }

    pub fn new_personnel(
        panel: WeakEntity<AgentPanel>,
        agent_options: Vec<CompanyAgentOption>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut modal = Self::new(panel, agent_options, window, cx);
        modal.mode = CompanyModalMode::PersonnelList;
        modal
    }

    pub fn new_add_personnel(
        panel: WeakEntity<AgentPanel>,
        agent_options: Vec<CompanyAgentOption>,
        session_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut modal = Self::new(panel, agent_options, window, cx);
        if let Some(session) = modal
            .data
            .sessions
            .iter()
            .find(|session| session.id == session_id)
        {
            modal.mode = CompanyModalMode::AddPersonnel {
                company_id: session.company_id,
                session_id,
            };
        }
        modal
    }

    fn editor_text(editor: &Entity<Editor>, cx: &App) -> String {
        editor.read(cx).text(cx).trim().to_string()
    }

    fn set_editor_text(
        editor: &Entity<Editor>,
        text: impl AsRef<str>,
        window: &mut Window,
        cx: &mut App,
    ) {
        editor.update(cx, |editor, cx| editor.set_text(text.as_ref(), window, cx));
    }

    fn session_description_mention_query(&self, cx: &App) -> Option<(usize, String)> {
        let text = self.session_description.read(cx).text(cx);
        let token_start = text
            .char_indices()
            .rev()
            .find(|(_, character)| character.is_whitespace())
            .map(|(index, character)| index + character.len_utf8())
            .unwrap_or(0);
        let query = text.get(token_start..)?.strip_prefix('@')?;
        (!query.contains(char::is_whitespace)).then(|| (token_start, query.to_string()))
    }

    fn insert_session_personnel_mention(
        &mut self,
        member_name: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some((token_start, _)) = self.session_description_mention_query(cx) else {
            return;
        };
        let mut text = self.session_description.read(cx).text(cx);
        text.replace_range(token_start.., &format!("@{member_name} "));
        self.session_description.update(cx, |editor, cx| {
            editor.set_text(text, window, cx);
            editor.focus_handle(cx).focus(window, cx);
        });
        if cfg!(target_os = "android") && !window.soft_keyboard_visible() {
            window.toggle_soft_keyboard();
        }
        cx.notify();
    }

    fn show_company_form(
        &mut self,
        company_id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let name = company_id
            .and_then(|id| self.data.companies.iter().find(|company| company.id == id))
            .map(|company| company.name.as_str())
            .unwrap_or_default();
        Self::set_editor_text(&self.company_name, name, window, cx);
        let company =
            company_id.and_then(|id| self.data.companies.iter().find(|company| company.id == id));
        Self::set_editor_text(
            &self.company_rules,
            company
                .map(|company| company.rules.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        Self::set_editor_text(
            &self.company_skills,
            company
                .map(|company| company.skills.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        self.mode = CompanyModalMode::EditCompany { company_id };
        self.company_name
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn save_company(&mut self, company_id: Option<Uuid>, cx: &mut Context<Self>) {
        let name = Self::editor_text(&self.company_name, cx);
        if name.is_empty() {
            return;
        }
        let rules = Self::editor_text(&self.company_rules, cx);
        let skills = Self::editor_text(&self.company_skills, cx);
        if let Some(company) = company_id.and_then(|id| {
            self.data
                .companies
                .iter_mut()
                .find(|company| company.id == id)
        }) {
            company.name = name;
            company.rules = rules;
            company.skills = skills;
        } else {
            self.data.companies.push(Company {
                id: Uuid::new_v4(),
                name,
                rules,
                skills,
                members: Vec::new(),
            });
        }
        save_companies(&self.data, cx);
        self.mode = CompanyModalMode::List;
        cx.notify();
    }

    fn show_member_form(
        &mut self,
        company_id: Option<Uuid>,
        member_id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let member = member_id
            .and_then(|id| self.data.personnel.iter().find(|person| person.id == id))
            .cloned()
            .or_else(|| {
                company_id.and_then(|company_id| {
                    self.data
                        .companies
                        .iter()
                        .find(|company| company.id == company_id)
                        .and_then(|company| {
                            member_id.and_then(|id| {
                                company.members.iter().find(|member| member.id == id)
                            })
                        })
                        .cloned()
                })
            });
        let field = |editor: &Entity<Editor>, value: &str, window: &mut Window, cx: &mut App| {
            Self::set_editor_text(editor, value, window, cx);
        };
        field(
            &self.member_name,
            member.as_ref().map(|m| m.name.as_str()).unwrap_or_default(),
            window,
            cx,
        );
        field(
            &self.persona,
            member
                .as_ref()
                .map(|m| m.persona.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        field(
            &self.skills,
            member
                .as_ref()
                .map(|m| m.skills.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        field(
            &self.rules,
            member
                .as_ref()
                .map(|m| m.rules.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        field(
            &self.boundaries,
            member
                .as_ref()
                .map(|m| m.boundaries.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        field(
            &self.workflow,
            member
                .as_ref()
                .map(|m| m.workflow.as_str())
                .unwrap_or_default(),
            window,
            cx,
        );
        if let Some(member) = member {
            self.selected_agent_id = member.agent_id;
            self.selected_model = member.model;
        } else {
            self.selected_agent_id = self
                .agent_options
                .first()
                .map(|agent| agent.id.clone())
                .unwrap_or_else(|| agent::ZED_AGENT_ID.to_string());
            self.selected_model = None;
        }
        self.mode = CompanyModalMode::EditMember {
            company_id,
            member_id,
        };
        self.member_form_error = None;
        self.member_name.read(cx).focus_handle(cx).focus(window, cx);
        cx.notify();
    }

    fn save_member(
        &mut self,
        company_id: Option<Uuid>,
        member_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) {
        let name = Self::editor_text(&self.member_name, cx);
        if name.is_empty() {
            self.member_form_error = Some("Personnel name is required.".into());
            cx.notify();
            return;
        }
        let member = CompanyMember {
            id: member_id.unwrap_or_else(Uuid::new_v4),
            name,
            agent_id: self.selected_agent_id.clone(),
            model: self.selected_model.clone(),
            // Kept for backwards-compatible persisted data. The role is now part of
            // the member name/persona instead of a separate mobile form field.
            role: String::new(),
            persona: Self::editor_text(&self.persona, cx),
            skills: Self::editor_text(&self.skills, cx),
            rules: Self::editor_text(&self.rules, cx),
            boundaries: Self::editor_text(&self.boundaries, cx),
            workflow: Self::editor_text(&self.workflow, cx),
        };
        if let Some(existing) = self
            .data
            .personnel
            .iter_mut()
            .find(|person| person.id == member.id)
        {
            *existing = member.clone();
        } else {
            self.data.personnel.push(member.clone());
        }
        for company in &mut self.data.companies {
            if let Some(existing) = company
                .members
                .iter_mut()
                .find(|existing| existing.id == member.id)
            {
                *existing = member.clone();
            }
        }
        if let Some(company_id) = company_id {
            let Some(company) = self
                .data
                .companies
                .iter_mut()
                .find(|company| company.id == company_id)
            else {
                self.member_form_error =
                    Some("The company could not be found. Go back and retry.".into());
                cx.notify();
                return;
            };
            if !company
                .members
                .iter()
                .any(|existing| existing.id == member.id)
            {
                company.members.push(member);
            }
        }
        save_companies(&self.data, cx);
        self.member_form_error = None;
        self.mode = if let Some(company_id) = company_id {
            CompanyModalMode::EditCompany {
                company_id: Some(company_id),
            }
        } else {
            CompanyModalMode::PersonnelList
        };
        cx.notify();
    }

    fn show_session_form(&mut self, company_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.session_kind = CompanySessionKind::Meeting;
        self.selected_members = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .map(|company| company.members.iter().map(|member| member.id).collect())
            .unwrap_or_default();
        self.attachments.clear();
        Self::set_editor_text(&self.session_title, "", window, cx);
        Self::set_editor_text(&self.session_description, "", window, cx);
        Self::set_editor_text(&self.completion_condition, "", window, cx);
        Self::set_editor_text(&self.meeting_turn_limit, "8", window, cx);
        self.manual_stop_only = false;
        self.allow_clarifying_questions = true;
        self.mode = CompanyModalMode::StartSession { company_id };
        self.session_title
            .read(cx)
            .focus_handle(cx)
            .focus(window, cx);
        cx.notify();
    }

    fn start_session(&mut self, company_id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        let Some(company) = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .cloned()
        else {
            return;
        };
        let title = Self::editor_text(&self.session_title, cx);
        let description = Self::editor_text(&self.session_description, cx);
        let completion_condition = Self::editor_text(&self.completion_condition, cx);
        let meeting_turn_limit =
            if self.session_kind == CompanySessionKind::Meeting && !self.manual_stop_only {
                Self::editor_text(&self.meeting_turn_limit, cx)
                    .parse::<usize>()
                    .ok()
                    .map(|limit| limit.clamp(1, 100))
            } else {
                None
            };
        if title.is_empty() || description.is_empty() || self.selected_members.is_empty() {
            return;
        }
        let request = CompanySessionRequest {
            company,
            kind: self.session_kind,
            title,
            description,
            member_ids: self.selected_members.iter().copied().collect(),
            attachments: self.attachments.clone(),
            completion_condition,
            meeting_turn_limit,
            allow_clarifying_questions: self.allow_clarifying_questions,
        };
        if let Some(panel) = self.panel.upgrade() {
            panel.update(cx, |panel, cx| {
                panel.launch_company_session(request, window, cx)
            });
        }
        cx.emit(DismissEvent);
    }

    fn field(&self, label: &'static str, editor: Entity<Editor>) -> AnyElement {
        let focus_editor = editor.clone();
        let scroll_anchor = ScrollAnchor::for_handle(self.scroll_handle.clone());
        let focus_scroll_anchor = scroll_anchor.clone();
        v_flex()
            .w_full()
            .gap_1()
            .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
            .child(
                div()
                    .id(SharedString::from(format!("company-field-{label}")))
                    .anchor_scroll(Some(scroll_anchor))
                    .w_full()
                    .min_h_8()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .when(cfg!(target_os = "android"), |this| {
                        this.capture_any_mouse_down(move |event: &MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left {
                                focus_editor.read(cx).focus_handle(cx).focus(window, cx);
                                if !window.soft_keyboard_visible() {
                                    window.toggle_soft_keyboard();
                                }
                                focus_scroll_anchor.scroll_to_center_after_frames(48, window, cx);
                            }
                        })
                    })
                    .child(editor),
            )
            .into_any_element()
    }

    fn info_field(
        &self,
        id: &'static str,
        label: &'static str,
        help: &'static str,
        editor: Entity<Editor>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let focus_editor = editor.clone();
        let scroll_anchor = ScrollAnchor::for_handle(self.scroll_handle.clone());
        let focus_scroll_anchor = scroll_anchor.clone();
        v_flex()
            .w_full()
            .gap_1()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new(label).size(LabelSize::Small).color(Color::Muted))
                    .child(
                        IconButton::new(
                            SharedString::from(format!("company-field-help-{id}")),
                            IconName::Info,
                        )
                        .icon_size(ui::IconSize::Small)
                        .tooltip(Tooltip::text("About this field"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.field_help_dialog = Some((label, help));
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .id(SharedString::from(format!("company-info-field-{id}")))
                    .anchor_scroll(Some(scroll_anchor))
                    .w_full()
                    .min_h_8()
                    .px_2()
                    .py_1p5()
                    .rounded_md()
                    .border_1()
                    .when(cfg!(target_os = "android"), |this| {
                        this.capture_any_mouse_down(move |event: &MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left {
                                focus_editor.read(cx).focus_handle(cx).focus(window, cx);
                                if !window.soft_keyboard_visible() {
                                    window.toggle_soft_keyboard();
                                }
                                focus_scroll_anchor.scroll_to_center_after_frames(48, window, cx);
                            }
                        })
                    })
                    .child(editor),
            )
            .into_any_element()
    }

    fn confirm_delete_company(
        &mut self,
        company_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(company) = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
        else {
            return;
        };
        let session_count = self
            .data
            .sessions
            .iter()
            .filter(|session| session.company_id == company_id)
            .count();
        let detail = format!(
            "Deleting {} also removes its {session_count} Meeting/Task session records, main threads and Personnel sub-threads. Export keeps a readable transcript in ~/company-exports first.",
            company.name
        );
        let prompt = window.prompt(
            gpui::PromptLevel::Critical,
            "Delete this company and all of its sessions?",
            Some(&detail),
            &["Export Markdown & Delete", "Delete", "Cancel"],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            let choice = prompt.await;
            if choice == Ok(0) || choice == Ok(1) {
                this.update(cx, |this, cx| {
                    this.delete_company(company_id, choice == Ok(0), cx);
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn delete_company(&mut self, company_id: Uuid, export_markdown: bool, cx: &mut Context<Self>) {
        let Some(company) = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .cloned()
        else {
            return;
        };
        let sessions = self
            .data
            .sessions
            .iter()
            .filter(|session| session.company_id == company_id)
            .cloned()
            .collect::<Vec<_>>();

        if export_markdown {
            let mut markdown = format!("# {} Sessions\n\n", company.name);
            for session in &sessions {
                let kind = match session.kind {
                    CompanySessionKind::Meeting => "Meeting",
                    CompanySessionKind::Task => "Task",
                };
                markdown.push_str(&format!("## {kind}: {}\n\n", session.title));
                markdown.push_str(&format!(
                    "- Started: {}\n- Status: {}\n\n{}\n\n",
                    Self::session_started_label(session),
                    Self::session_status_label(session),
                    session.description
                ));
                for entry in &session.timeline {
                    markdown.push_str(&format!("### {}\n\n{}\n\n", entry.speaker, entry.content));
                }
            }
            let safe_name = company
                .name
                .chars()
                .map(|character| {
                    if character.is_alphanumeric() || matches!(character, '-' | '_') {
                        character
                    } else {
                        '-'
                    }
                })
                .collect::<String>();
            let export_dir = paths::home_dir().join("company-exports");
            if let Err(error) = std::fs::create_dir_all(&export_dir).and_then(|()| {
                std::fs::write(
                    export_dir.join(format!(
                        "{}-sessions-{}.md",
                        safe_name,
                        Utc::now().format("%Y%m%d-%H%M%S")
                    )),
                    markdown,
                )
            }) {
                log::error!("failed to export company sessions: {error:#}");
                return;
            }
        }

        let mut thread_ids = Vec::new();
        for session in &sessions {
            thread_ids.extend(session.worker_thread_ids.iter().copied());
            if let Some(primary_thread_id) = session.primary_thread_id {
                thread_ids.push(primary_thread_id);
            }
        }
        let thread_ids = thread_ids.into_iter().collect::<HashSet<_>>();
        self.data
            .sessions
            .retain(|session| session.company_id != company_id);
        self.data
            .companies
            .retain(|company| company.id != company_id);
        for thread_id in &thread_ids {
            self.data.company_thread_ids.remove(thread_id);
            self.data.personnel_thread_ids.remove(thread_id);
            ThreadMetadataStore::global(cx).update(cx, |store, cx| store.delete(*thread_id, cx));
        }
        save_companies(&self.data, cx);
        self.mode = CompanyModalMode::List;
        cx.notify();
    }

    fn session_started_label(session: &CompanySessionRecord) -> String {
        if session.started_at_unix_ms <= 0 {
            return "Start time unavailable (older session)".to_string();
        }
        Local
            .timestamp_millis_opt(session.started_at_unix_ms)
            .single()
            .map(|time| time.format("%Y-%m-%d %H:%M").to_string())
            .unwrap_or_else(|| "Start time unavailable".to_string())
    }

    fn session_status_label(session: &CompanySessionRecord) -> &'static str {
        if session.archived {
            return "Archived";
        }
        match (session.status, session.paused) {
            (CompanySessionStatus::Running, true) => "On hold",
            (CompanySessionStatus::Running, false) => "Running",
            (CompanySessionStatus::WaitingForUser, _) => "Waiting for you",
            (CompanySessionStatus::Completed, _) => "Completed",
            (CompanySessionStatus::Failed, _) => "Failed",
        }
    }

    fn render_company_sessions(
        &mut self,
        company_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let company_name = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .map(|company| company.name.clone())
            .unwrap_or_else(|| "Company".to_string());
        let filter_label = match self.session_filter {
            CompanySessionFilter::Active => "Active",
            CompanySessionFilter::Complete => "Complete",
            CompanySessionFilter::Archive => "Archive",
        };
        let modal = cx.weak_entity();
        let filter_menu = ui::ContextMenu::build(window, cx, move |mut menu, _, _| {
            for (label, filter) in [
                ("Active", CompanySessionFilter::Active),
                ("Complete", CompanySessionFilter::Complete),
                ("Archive", CompanySessionFilter::Archive),
            ] {
                menu = menu.item(ui::ContextMenuEntry::new(label).handler({
                    let modal = modal.clone();
                    move |_, cx| {
                        modal
                            .update(cx, |modal, cx| {
                                modal.session_filter = filter;
                                cx.notify();
                            })
                            .ok();
                    }
                }));
            }
            menu
        });

        let mut sessions = v_flex().w_full().gap_2();
        let mut visible_count = 0;
        for session in self
            .data
            .sessions
            .iter()
            .filter(|session| session.company_id == company_id)
            .filter(|session| match self.session_filter {
                CompanySessionFilter::Active => {
                    !session.archived
                        && matches!(
                            session.status,
                            CompanySessionStatus::Running | CompanySessionStatus::WaitingForUser
                        )
                }
                CompanySessionFilter::Complete => {
                    !session.archived
                        && matches!(
                            session.status,
                            CompanySessionStatus::Completed | CompanySessionStatus::Failed
                        )
                }
                CompanySessionFilter::Archive => session.archived,
            })
            .rev()
            .cloned()
        {
            visible_count += 1;
            let session_id = session.id;
            let open_session = session.clone();
            let kind = match session.kind {
                CompanySessionKind::Meeting => "Meeting",
                CompanySessionKind::Task => "Task",
            };
            sessions = sessions.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(
                                Label::new(format!("{kind}: {}", session.title))
                                    .size(LabelSize::Large),
                            )
                            .child(
                                Label::new(format!(
                                    "{} · Started {}",
                                    Self::session_status_label(&session),
                                    Self::session_started_label(&session)
                                ))
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .justify_end()
                            .gap_2()
                            .child(
                                Button::new(
                                    SharedString::from(format!(
                                        "open-company-session-{session_id}"
                                    )),
                                    "Open",
                                )
                                .disabled(session.primary_thread_id.is_none())
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(panel) = this.panel.upgrade() {
                                            panel.update(cx, |panel, cx| {
                                                panel.open_company_session(
                                                    &open_session,
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                        cx.emit(DismissEvent);
                                    },
                                )),
                            )
                            .when(!session.archived, |this| {
                                this.child(
                                    Button::new(
                                        SharedString::from(format!(
                                            "archive-company-session-{session_id}"
                                        )),
                                        "Close (Archive)",
                                    )
                                    .style(ButtonStyle::Subtle)
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if let Some(session) = this
                                                .data
                                                .sessions
                                                .iter_mut()
                                                .find(|session| session.id == session_id)
                                            {
                                                session.archived = true;
                                                session.stop_requested = true;
                                                session.runner_active = false;
                                                session.paused = true;
                                                session.runner_generation =
                                                    session.runner_generation.saturating_add(1);
                                                save_companies(&this.data, cx);
                                                cx.notify();
                                            }
                                        },
                                    )),
                                )
                            }),
                    ),
            );
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .child(Label::new(format!("{company_name} Sessions")).size(LabelSize::Large))
                    .child(ui::DropdownMenu::new(
                        "company-session-filter",
                        filter_label,
                        filter_menu,
                    )),
            )
            .when(visible_count == 0, |this| {
                this.child(Label::new(format!("No {filter_label} sessions.")).color(Color::Muted))
            })
            .child(sessions)
            .child(
                Button::new("back-from-company-sessions", "Back").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.mode = CompanyModalMode::List;
                        cx.notify();
                    },
                )),
            )
            .into_any_element()
    }

    fn render_list(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let mut list = v_flex().w_full().gap_2();
        for company in self.data.companies.clone() {
            let company_id = company.id;
            list = list.child(
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .w_full()
                            .justify_between()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(Icon::new(IconName::Building2))
                                    .child(Label::new(company.name).size(LabelSize::Large)),
                            )
                            .child(
                                Label::new(format!("{} personnel", company.members.len()))
                                    .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new(
                                    SharedString::from(format!("open-company-{company_id}")),
                                    "Open",
                                )
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.show_session_form(company_id, window, cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("configure-company-{company_id}")),
                                    "Configure",
                                )
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.show_company_form(Some(company_id), window, cx);
                                    },
                                )),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("sessions-company-{company_id}")),
                                    "Sessions",
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.session_filter = CompanySessionFilter::Active;
                                        this.mode = CompanyModalMode::Sessions { company_id };
                                        cx.notify();
                                    },
                                )),
                            )
                            .child(
                                Button::new(
                                    SharedString::from(format!("delete-company-{company_id}")),
                                    "Delete",
                                )
                                .style(ButtonStyle::Subtle)
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.confirm_delete_company(company_id, window, cx);
                                    },
                                )),
                            ),
                    ),
            );
        }
        v_flex()
            .w_full()
            .gap_3()
            .child(
                Button::new("create-company", "Create Company or Team")
                    .style(ButtonStyle::Tinted(TintColor::Accent))
                    .start_icon(Icon::new(IconName::Plus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_company_form(None, window, cx);
                    })),
            )
            .child(list)
            .into_any_element()
    }

    fn render_personnel_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = v_flex().w_full().gap_2();
        for person in self.data.personnel.clone() {
            let person_id = person.id;
            let chat_person = person.clone();
            list = list.child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .gap_2()
                    .p_3()
                    .rounded_md()
                    .border_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        v_flex()
                            .min_w_0()
                            .child(Label::new(person.name).size(LabelSize::Large))
                            .child(
                                Label::new(
                                    self.agent_options
                                        .iter()
                                        .find(|agent| agent.id == person.agent_id)
                                        .map(|agent| agent.name.clone())
                                        .unwrap_or_else(|| person.agent_id.clone().into()),
                                )
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                Button::new(
                                    SharedString::from(format!("chat-personnel-{person_id}")),
                                    "Chat",
                                )
                                .style(ButtonStyle::Tinted(TintColor::Accent))
                                .start_icon(Icon::new(IconName::Chat))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        if let Some(panel) = this.panel.upgrade() {
                                            panel.update(cx, |panel, cx| {
                                                panel.launch_personnel_chat(
                                                    chat_person.clone(),
                                                    window,
                                                    cx,
                                                );
                                            });
                                        }
                                        cx.emit(DismissEvent);
                                    },
                                )),
                            )
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("configure-personnel-{person_id}")),
                                    IconName::Pencil,
                                )
                                .tooltip(Tooltip::text("Configure Personnel"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.show_member_form(None, Some(person_id), window, cx);
                                    },
                                )),
                            ),
                    ),
            );
        }
        v_flex()
            .w_full()
            .gap_3()
            .child(
                Button::new("create-personnel", "Add Personnel")
                    .style(ButtonStyle::Tinted(TintColor::Accent))
                    .start_icon(Icon::new(IconName::Plus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.show_member_form(None, None, window, cx);
                    })),
            )
            .child(list)
            .into_any_element()
    }

    fn render_company_form(
        &mut self,
        company_id: Option<Uuid>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let members = company_id
            .and_then(|id| self.data.companies.iter().find(|company| company.id == id))
            .map(|company| company.members.clone())
            .unwrap_or_default();
        let assigned_ids = members
            .iter()
            .map(|member| member.id)
            .collect::<HashSet<_>>();
        let available_personnel = self
            .data
            .personnel
            .iter()
            .filter(|person| !assigned_ids.contains(&person.id))
            .cloned()
            .collect::<Vec<_>>();
        let mut member_list = v_flex().w_full().gap_1();
        for member in &members {
            let member_id = member.id;
            let company_id = company_id.expect("members only exist for saved companies");
            member_list = member_list.child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().colors().element_background)
                    .child(v_flex().child(member.name.clone()))
                    .child(
                        h_flex()
                            .gap_1()
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("edit-member-{member_id}")),
                                    IconName::Pencil,
                                )
                                .tooltip(Tooltip::text("Configure Personnel"))
                                .on_click(cx.listener(
                                    move |this, _, window, cx| {
                                        this.show_member_form(
                                            Some(company_id),
                                            Some(member_id),
                                            window,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                IconButton::new(
                                    SharedString::from(format!("remove-personnel-{member_id}")),
                                    IconName::Close,
                                )
                                .tooltip(Tooltip::text("Remove from Company"))
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(company) = this
                                            .data
                                            .companies
                                            .iter_mut()
                                            .find(|company| company.id == company_id)
                                        {
                                            company.members.retain(|member| member.id != member_id);
                                            save_companies(&this.data, cx);
                                            cx.notify();
                                        }
                                    },
                                )),
                            ),
                    ),
            );
        }
        let has_available_personnel = !available_personnel.is_empty();
        let mut available_list = v_flex().w_full().gap_1();
        for person in available_personnel {
            let person_id = person.id;
            available_list = available_list.child(
                h_flex()
                    .w_full()
                    .justify_between()
                    .p_2()
                    .rounded_md()
                    .bg(cx.theme().colors().element_background)
                    .child(person.name)
                    .child(
                        Button::new(
                            SharedString::from(format!("assign-personnel-{person_id}")),
                            "Assign",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let person = this
                                .data
                                .personnel
                                .iter()
                                .find(|person| person.id == person_id)
                                .cloned();
                            let company = this
                                .data
                                .companies
                                .iter_mut()
                                .find(|company| company.id == company_id.unwrap());
                            if let (Some(person), Some(company)) = (person, company)
                                && !company.members.iter().any(|member| member.id == person_id)
                            {
                                company.members.push(person);
                                save_companies(&this.data, cx);
                                cx.notify();
                            }
                        })),
                    ),
            );
        }
        v_flex()
            .w_full()
            .gap_3()
            .child(self.field(
                "Company / Team Name",
                self.company_name.clone(),
            ))
            .child(self.info_field(
                "company-skills",
                "Company Skills (/ commands)",
                "Installed slash-command skills listed here apply to every Personnel chat, Meeting and Task activity created by this company. This does not install missing skills.",
                self.company_skills.clone(),
                cx,
            ))
            .child(self.info_field(
                "company-rules",
                "Company Rules",
                "High-level AGENTS.md-style instructions inherited by every person and activity in this company. Personnel rules still apply in addition to these rules.",
                self.company_rules.clone(),
                cx,
            ))
            .when(company_id.is_some(), |this| {
                let company_id = company_id.unwrap();
                this.child(Label::new("Assigned Personnel").size(LabelSize::Large))
                    .child(member_list)
                    .child(
                        Button::new("add-company-member", "Create New Personnel")
                            .start_icon(Icon::new(IconName::Plus))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.show_member_form(Some(company_id), None, window, cx);
                            })),
                    )
                    .when(has_available_personnel, |this| {
                        this.child(Label::new("Available Personnel").size(LabelSize::Large))
                            .child(available_list)
                    })
            })
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("cancel-company", "Back").on_click(cx.listener(
                        |this, _, _, cx| {
                            this.mode = CompanyModalMode::List;
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("save-company", "Save")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .on_click(
                                cx.listener(move |this, _, _, cx| {
                                    this.save_company(company_id, cx)
                                }),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn agent_name(&self) -> SharedString {
        self.agent_options
            .iter()
            .find(|agent| agent.id == self.selected_agent_id)
            .map(|agent| agent.name.clone())
            .unwrap_or_else(|| self.selected_agent_id.clone().into())
    }

    fn render_member_form(
        &mut self,
        _company_id: Option<Uuid>,
        _member_id: Option<Uuid>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let weak_self = cx.weak_entity();
        let agent_menu = ui::ContextMenu::build(window, cx, |mut menu, _, _| {
            for option in self.agent_options.clone() {
                let id = option.id.clone();
                menu = menu.item(ui::ContextMenuEntry::new(option.name).handler({
                    let this = weak_self.clone();
                    move |_, cx| {
                        this.update(cx, |this, cx| {
                            this.selected_agent_id = id.clone();
                            this.selected_model = None;
                            cx.notify();
                        })
                        .ok();
                    }
                }));
            }
            menu
        });
        let models = self
            .agent_options
            .iter()
            .find(|agent| agent.id == self.selected_agent_id)
            .map(|agent| agent.models.clone())
            .unwrap_or_default();
        let model_label: SharedString = self
            .selected_model
            .as_ref()
            .and_then(|selected| models.iter().find(|(id, _)| id == selected))
            .map(|(_, name)| name.clone())
            .unwrap_or_else(|| "Agent default".into());
        let weak_self = cx.weak_entity();
        let model_menu = ui::ContextMenu::build(window, cx, |mut menu, _, _| {
            let this = weak_self.clone();
            menu = menu.item(
                ui::ContextMenuEntry::new("Agent default").handler(move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.selected_model = None;
                        cx.notify();
                    })
                    .ok();
                }),
            );
            for (model_id, model_name) in models {
                let this = weak_self.clone();
                menu = menu.item(ui::ContextMenuEntry::new(model_name).handler(move |_, cx| {
                    this.update(cx, |this, cx| {
                        this.selected_model = Some(model_id.clone());
                        cx.notify();
                    })
                    .ok();
                }));
            }
            menu
        });

        v_flex()
            .w_full()
            .gap_3()
            .child(self.info_field(
                "name",
                "Name",
                "The person's display name. You can include the job title here, for example: Alex - UI/UX Designer.",
                self.member_name.clone(),
                cx,
            ))
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Label::new("Agent")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        ui::DropdownMenu::new("company-agent", self.agent_name(), agent_menu)
                            .full_width(true),
                    ),
            )
            .child(
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Label::new("Model")
                            .size(LabelSize::Small)
                            .color(Color::Muted),
                    )
                    .child(
                        ui::DropdownMenu::new("company-model", model_label, model_menu)
                            .full_width(true),
                    ),
            )
            .child(self.info_field(
                "persona",
                "Pre-config / Persona",
                "Defines who this person is, their expertise, perspective and preferred working style.",
                self.persona.clone(),
                cx,
            ))
            .child(self.info_field(
                "skills",
                "Skills (/ commands)",
                "Lists installed slash-command skills this person may invoke, for example /frontend-design. This does not install missing skills.",
                self.skills.clone(),
                cx,
            ))
            .child(self.info_field(
                "rules",
                "Rules",
                "Instructions the person must follow while working, such as testing requirements or coding conventions.",
                self.rules.clone(),
                cx,
            ))
            .child(self.info_field(
                "boundaries",
                "Boundaries",
                "Defines what the person must not do and which files, systems or responsibilities are outside their ownership.",
                self.boundaries.clone(),
                cx,
            ))
            .child(self.info_field(
                "workflow",
                "Workflow",
                "Defines the order of work, such as analyse, propose, implement, test and hand off.",
                self.workflow.clone(),
                cx,
            ))
            .into_any_element()
    }

    fn render_member_footer(
        &mut self,
        company_id: Option<Uuid>,
        member_id: Option<Uuid>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let error = self.member_form_error.clone();
        v_flex()
            .w_full()
            .gap_2()
            .p_4()
            .border_t_1()
            .border_color(cx.theme().colors().border)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("cancel-member", "Back").on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.member_form_error = None;
                            this.mode = if let Some(company_id) = company_id {
                                CompanyModalMode::EditCompany {
                                    company_id: Some(company_id),
                                }
                            } else {
                                CompanyModalMode::PersonnelList
                            };
                            cx.notify();
                        },
                    )))
                    .child(
                        Button::new("save-member", "Save Personnel")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                // Android IMEs may leave the visible name in an active
                                // composition until focus changes. Commit that composition
                                // before reading the editor on the next UI frame.
                                this.focus_handle.focus(window, cx);
                                if window.soft_keyboard_visible() {
                                    window.toggle_soft_keyboard();
                                }
                                cx.defer_in(window, move |this, _, cx| {
                                    this.save_member(company_id, member_id, cx);
                                });
                            })),
                    ),
            )
            .when_some(error, |this, error| {
                this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))
            })
            .into_any_element()
    }

    fn render_session_form(
        &mut self,
        company_id: Uuid,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let members = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .map(|company| company.members.clone())
            .unwrap_or_default();
        let mut member_list = v_flex().w_full().gap_1();
        for member in &members {
            let member_id = member.id;
            let selected = self.selected_members.contains(&member_id);
            member_list = member_list.child(
                ui::ListItem::new(SharedString::from(format!("session-member-{member_id}")))
                    .toggle_state(selected)
                    .start_slot(Icon::new(if selected {
                        IconName::Check
                    } else {
                        IconName::Person
                    }))
                    .child(Label::new(member.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.selected_members.remove(&member_id) {
                            this.selected_members.insert(member_id);
                        }
                        cx.notify();
                    })),
            );
        }
        let attachments = self
            .attachments
            .iter()
            .map(|path| {
                Label::new(path.display().to_string())
                    .size(LabelSize::Small)
                    .truncate()
            })
            .collect::<Vec<_>>();
        let description_mentions =
            self.session_description_mention_query(cx)
                .and_then(|(_, query)| {
                    let normalized_query = query.to_lowercase();
                    let matching_members = members
                        .iter()
                        .filter(|member| {
                            normalized_query.is_empty()
                                || member.name.to_lowercase().contains(&normalized_query)
                        })
                        .take(8)
                        .cloned()
                        .collect::<Vec<_>>();
                    if matching_members.is_empty() {
                        return None;
                    }
                    let mut menu = v_flex()
                        .id("task-description-personnel-menu")
                        .w_full()
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .track_scroll(&self.mention_scroll_handle)
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .bg(cx.theme().colors().elevated_surface_background);
                    if members.len() > 5 {
                        menu = menu.child(
                            Label::new("Type after @ to search and assign Personnel")
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        );
                    }
                    for member in matching_members {
                        let member_id = member.id;
                        let member_name = member.name.clone();
                        let member_name_for_click = member_name.clone();
                        menu = menu.child(
                            ui::ListItem::new(SharedString::from(format!(
                                "task-description-mention-{member_id}"
                            )))
                            .start_slot(Icon::new(IconName::Person))
                            .child(Label::new(member_name))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.insert_session_personnel_mention(
                                        &member_name_for_click,
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        );
                    }
                    Some(
                        div()
                            .relative()
                            .w_full()
                            .child(menu)
                            .vertical_scrollbar_for(&self.mention_scroll_handle, window, cx)
                            .into_any_element(),
                    )
                });
        let description_label = match self.session_kind {
            CompanySessionKind::Meeting => "Meeting Description",
            CompanySessionKind::Task => "Task Description",
        };
        v_flex()
            .w_full()
            .gap_3()
            .child(
                h_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Button::new("company-session-meeting", "Meeting")
                            .toggle_state(self.session_kind == CompanySessionKind::Meeting)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.session_kind = CompanySessionKind::Meeting;
                                this.session_description.update(cx, |editor, cx| {
                                    editor.set_placeholder_text(
                                        "Meeting description and desired outcome",
                                        window,
                                        cx,
                                    );
                                });
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("company-session-task", "Task")
                            .toggle_state(self.session_kind == CompanySessionKind::Task)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.session_kind = CompanySessionKind::Task;
                                this.session_description.update(cx, |editor, cx| {
                                    editor.set_placeholder_text(
                                        "Task activity description; use @Personnel to assign work",
                                        window,
                                        cx,
                                    );
                                });
                                cx.notify();
                            })),
                    ),
            )
            .child(self.field("Name", self.session_title.clone()))
            .child(self.field(description_label, self.session_description.clone()))
            .when_some(description_mentions, |this, menu| this.child(menu))
            .child(self.field("Completion Condition", self.completion_condition.clone()))
            .when(self.session_kind == CompanySessionKind::Meeting, |this| {
                this.child(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .child(
                            Button::new("meeting-limited-turns", "Maximum Turns")
                                .toggle_state(!self.manual_stop_only)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.manual_stop_only = false;
                                    cx.notify();
                                })),
                        )
                        .child(
                            Button::new("meeting-manual-stop", "Manual Stop")
                                .toggle_state(self.manual_stop_only)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.manual_stop_only = true;
                                    cx.notify();
                                })),
                        ),
                )
                .when(!self.manual_stop_only, |this| {
                    this.child(self.field(
                        "Maximum AI Turns",
                        self.meeting_turn_limit.clone(),
                    ))
                })
            })
            .child(
                Checkbox::new(
                    "allow-company-clarification",
                    if self.allow_clarifying_questions {
                        ToggleState::Selected
                    } else {
                        ToggleState::Unselected
                    },
                )
                .label("Allow Clarifying Questions")
                .on_click(cx.listener(|this, state: &ToggleState, _, cx| {
                    this.allow_clarifying_questions = *state == ToggleState::Selected;
                    cx.notify();
                })),
            )
            .child(Label::new("People Included").size(LabelSize::Large))
            .child(member_list)
            .child(
                Button::new("company-add-attachments", "Attach External Files")
                    .start_icon(Icon::new(IconName::Paperclip))
                    .on_click(cx.listener(|_, _, window, cx| {
                        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
                            files: true,
                            directories: false,
                            multiple: true,
                            prompt: Some("Attach files to this company session".into()),
                        });
                        cx.spawn_in(window, async move |this, cx| {
                            let Ok(Ok(Some(paths))) = receiver.await else { return Ok(()) };
                            this.update(cx, |this, cx| {
                                this.attachments.extend(paths);
                                cx.notify();
                            })?;
                            anyhow::Ok(())
                        })
                        .detach_and_log_err(cx);
                    })),
            )
            .children(attachments)
            .child(
                Label::new(match self.session_kind {
                    CompanySessionKind::Meeting => "Personnel speak in order. The beta coordinator limits discussion rounds and produces an agenda.",
                    CompanySessionKind::Task => "Each selected person receives a separate assignment and file-ownership boundary. Use isolated worktrees when personnel may touch overlapping files.",
                })
                .color(Color::Muted),
            )
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(Button::new("cancel-session", "Back").on_click(cx.listener(|this, _, _, cx| {
                        this.mode = CompanyModalMode::List;
                        cx.notify();
                    })))
                    .child(
                        Button::new("start-company-session", "Start")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.start_session(company_id, window, cx)
                            })),
                    ),
            )
            .into_any_element()
    }

    fn render_add_personnel(
        &mut self,
        company_id: Uuid,
        session_id: Uuid,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let existing_personnel = self
            .data
            .sessions
            .iter()
            .find(|session| session.id == session_id)
            .map(|session| session.personnel_ids.clone())
            .unwrap_or_default();
        let available_personnel = self
            .data
            .companies
            .iter()
            .find(|company| company.id == company_id)
            .map(|company| {
                company
                    .members
                    .iter()
                    .filter(|member| !existing_personnel.contains(&member.id))
                    .cloned()
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let mut list = v_flex().w_full().gap_1();
        if available_personnel.is_empty() {
            list = list.child(
                Label::new("Every configured Personnel is already in this session.")
                    .color(Color::Muted),
            );
        } else {
            for member in available_personnel {
                let member_id = member.id;
                let selected = self.selected_members.contains(&member_id);
                list = list.child(
                    ui::ListItem::new(SharedString::from(format!(
                        "add-session-personnel-{member_id}"
                    )))
                    .toggle_state(selected)
                    .start_slot(Icon::new(if selected {
                        IconName::Check
                    } else {
                        IconName::Person
                    }))
                    .child(Label::new(member.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.selected_members.remove(&member_id) {
                            this.selected_members.insert(member_id);
                        }
                        cx.notify();
                    })),
                );
            }
        }

        v_flex()
            .w_full()
            .gap_3()
            .child(
                Label::new(
                    "Add Personnel to the active room. They join on the next available turn.",
                )
                .color(Color::Muted),
            )
            .child(list)
            .child(
                h_flex()
                    .w_full()
                    .justify_end()
                    .gap_2()
                    .child(
                        Button::new("cancel-add-session-personnel", "Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DismissEvent))),
                    )
                    .child(
                        Button::new("confirm-add-session-personnel", "Add")
                            .style(ButtonStyle::Tinted(TintColor::Accent))
                            .disabled(self.selected_members.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let selected =
                                    this.selected_members.iter().copied().collect::<Vec<_>>();
                                let names = this
                                    .data
                                    .companies
                                    .iter()
                                    .find(|company| company.id == company_id)
                                    .map(|company| {
                                        company
                                            .members
                                            .iter()
                                            .filter(|member| selected.contains(&member.id))
                                            .map(|member| member.name.clone())
                                            .collect::<Vec<_>>()
                                    })
                                    .unwrap_or_default();
                                update_company_session(session_id, cx, |session| {
                                    for member_id in &selected {
                                        if !session.personnel_ids.contains(member_id) {
                                            session.personnel_ids.push(*member_id);
                                        }
                                    }
                                    if !names.is_empty() {
                                        session.timeline.push(CompanyTimelineEntry {
                                            id: Uuid::new_v4(),
                                            kind: CompanyTimelineEntryKind::System,
                                            personnel_id: None,
                                            speaker: "Meeting".to_string(),
                                            content: format!(
                                                "{} joined the room.",
                                                names.join(", ")
                                            ),
                                            worker_thread_id: None,
                                        });
                                    }
                                });
                                cx.emit(DismissEvent);
                            })),
                    ),
            )
            .into_any_element()
    }
}

impl Render for CompanyModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let title = match self.mode {
            CompanyModalMode::List => "Company / Team",
            CompanyModalMode::PersonnelList => "Personnel",
            CompanyModalMode::Sessions { .. } => "Company Sessions",
            CompanyModalMode::EditCompany { .. } => "Configure Company",
            CompanyModalMode::EditMember { .. } => "Configure Agent",
            CompanyModalMode::StartSession { .. } => "Start Company Session",
            CompanyModalMode::AddPersonnel { .. } => "Add Personnel",
        };
        let content = match self.mode {
            CompanyModalMode::List => self.render_list(window, cx),
            CompanyModalMode::PersonnelList => self.render_personnel_list(window, cx),
            CompanyModalMode::Sessions { company_id } => {
                self.render_company_sessions(company_id, window, cx)
            }
            CompanyModalMode::EditCompany { company_id } => {
                self.render_company_form(company_id, window, cx)
            }
            CompanyModalMode::EditMember {
                company_id,
                member_id,
            } => self.render_member_form(company_id, member_id, window, cx),
            CompanyModalMode::StartSession { company_id } => {
                self.render_session_form(company_id, window, cx)
            }
            CompanyModalMode::AddPersonnel {
                company_id,
                session_id,
            } => self.render_add_personnel(company_id, session_id, cx),
        };
        let member_return_target = match self.mode {
            CompanyModalMode::EditMember { company_id, .. } => Some(company_id),
            _ => None,
        };
        let member_footer = match self.mode {
            CompanyModalMode::EditMember {
                company_id,
                member_id,
            } => Some(self.render_member_footer(company_id, member_id, cx)),
            _ => None,
        };
        let field_help_dialog = self.field_help_dialog.map(|(label, help)| {
            div()
                .id("company-field-help-overlay")
                .absolute()
                .inset_0()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .p_4()
                .bg(cx.theme().colors().panel_overlay_background)
                .occlude()
                .child(
                    v_flex()
                        .id("company-field-help-dialog")
                        .w_full()
                        .max_w(px(560.))
                        .min_h(vh(0.50, window))
                        .max_h(vh(0.90, window))
                        .rounded_md()
                        .border_1()
                        .border_color(cx.theme().colors().border)
                        .bg(cx.theme().colors().elevated_surface_background)
                        .child(
                            h_flex()
                                .w_full()
                                .justify_between()
                                .p_4()
                                .border_b_1()
                                .border_color(cx.theme().colors().border)
                                .child(Headline::new(label).size(HeadlineSize::Small))
                                .child(
                                    IconButton::new("close-company-field-help", IconName::Close)
                                        .tooltip(Tooltip::text("Close"))
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.field_help_dialog = None;
                                            cx.notify();
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .id("company-field-help-content")
                                .w_full()
                                .flex_1()
                                .min_h_0()
                                .overflow_y_scroll()
                                .p_4()
                                .child(Label::new(help)),
                        )
                        .child(
                            h_flex()
                                .w_full()
                                .justify_end()
                                .p_4()
                                .border_t_1()
                                .border_color(cx.theme().colors().border)
                                .child(
                                    Button::new("dismiss-company-field-help", "Close").on_click(
                                        cx.listener(|this, _, _, cx| {
                                            this.field_help_dialog = None;
                                            cx.notify();
                                        }),
                                    ),
                                ),
                        ),
                )
                .into_any_element()
        });

        v_flex()
            .id("company-team-dialog")
            .relative()
            .w(vw(0.90, window))
            .h(vh(0.90, window))
            .min_h(vh(0.50, window))
            .max_h(vh(0.90, window))
            .max_w(px(920.))
            .min_w_0()
            .rounded_md()
            .border_1()
            .border_color(cx.theme().colors().border)
            .bg(cx.theme().colors().elevated_surface_background)
            .track_focus(&self.focus_handle)
            .child(
                h_flex()
                    .w_full()
                    .p_4()
                    .justify_between()
                    .border_b_1()
                    .border_color(cx.theme().colors().border)
                    .child(
                        h_flex()
                            .gap_2()
                            .when_some(member_return_target, |this, company_id| {
                                this.child(
                                    IconButton::new(
                                        "back-from-company-member",
                                        IconName::ArrowLeft,
                                    )
                                    .tooltip(Tooltip::text("Back to Company"))
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.mode = if let Some(company_id) = company_id {
                                                CompanyModalMode::EditCompany {
                                                    company_id: Some(company_id),
                                                }
                                            } else {
                                                CompanyModalMode::PersonnelList
                                            };
                                            cx.notify();
                                        },
                                    )),
                                )
                            })
                            .child(Icon::new(IconName::Building2))
                            .child(Headline::new(title).size(HeadlineSize::Small)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(
                        div()
                            .id("company-team-dialog-scroll")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll_handle)
                            .child(v_flex().w_full().p_4().child(content)),
                    )
                    .vertical_scrollbar_for(&self.scroll_handle, window, cx),
            )
            .children(member_footer)
            .children(field_help_dialog)
    }
}

impl ModalView for CompanyModal {}

impl Focusable for CompanyModal {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl EventEmitter<DismissEvent> for CompanyModal {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_company_store_data_remains_compatible() {
        let data: CompanyStoreData = serde_json::from_str(r#"{"companies":[]}"#).unwrap();
        assert!(data.companies.is_empty());
        assert!(data.company_thread_ids.is_empty());
    }

    #[test]
    fn company_store_round_trips_thread_markers() {
        let thread_id = ThreadId::new();
        let data = CompanyStoreData {
            companies: Vec::new(),
            personnel: Vec::new(),
            company_thread_ids: HashSet::from([thread_id]),
            personnel_thread_ids: HashMap::new(),
            sessions: Vec::new(),
        };
        let encoded = serde_json::to_string(&data).unwrap();
        let decoded: CompanyStoreData = serde_json::from_str(&encoded).unwrap();
        assert!(decoded.company_thread_ids.contains(&thread_id));
    }
}
