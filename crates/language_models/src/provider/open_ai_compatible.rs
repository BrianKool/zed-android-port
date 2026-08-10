use anyhow::{Context as _, Result, bail};
use convert_case::{Case, Casing};
use credentials_provider::CredentialsProvider;
use futures::{AsyncReadExt as _, FutureExt, StreamExt, future::BoxFuture};
use fuzzy::StringMatch;
use gpui::{
    AnyElement, App, AsyncApp, Context, DismissEvent, Entity, PromptLevel, SharedString,
    Task, TaskExt, Window, px,
};
use http_client::{AsyncBody, CustomHeaders, HttpClient, Method, Request as HttpRequest};
use language_model::{
    ApiKeyState, AuthenticateError, EnvVar, IconOrSvg, InferencePhase, InferenceProgress,
    LanguageModel, LanguageModelCompletionError, LanguageModelCompletionEvent, LanguageModelId,
    LanguageModelName, LanguageModelProvider, LanguageModelProviderId, LanguageModelProviderName,
    LanguageModelProviderState, LanguageModelRequest, LanguageModelToolChoice,
    LanguageModelToolSchemaFormat, ProviderSettingsView, RateLimiter, SubPageProviderSettings,
};
use menu;
use open_ai::{
    ResponseStreamEvent,
    responses::{Request as ResponseRequest, StreamEvent as ResponsesStreamEvent, stream_response},
    stream_completion,
};
use picker::{Picker, PickerDelegate};
use serde::{Deserialize, Serialize};
use settings::{Settings, SettingsStore};
use sha2::{Digest, Sha256};
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    process::Stdio,
    sync::Arc,
    time::{Duration, Instant},
};
use ui::{
    ElevationIndex, ListItem, ListItemSpacing, PopoverMenu, PopoverMenuHandle, ProgressBar,
    Tooltip, prelude::*,
};
use ui_input::InputField;
use util::ResultExt;

use crate::provider::open_ai::{
    ChatCompletionMaxTokensParameter, OpenAiEventMapper, OpenAiResponseEventMapper, into_open_ai,
    into_open_ai_response,
};
pub use settings::OpenAiCompatibleAvailableModel as AvailableModel;
pub use settings::OpenAiCompatibleModelCapabilities as ModelCapabilities;

#[derive(Default, Clone, Debug, PartialEq)]
pub struct OpenAiCompatibleSettings {
    pub api_url: String,
    pub available_models: Vec<AvailableModel>,
    pub custom_headers: CustomHeaders,
}

pub struct OpenAiCompatibleLanguageModelProvider {
    id: LanguageModelProviderId,
    name: LanguageModelProviderName,
    http_client: Arc<dyn HttpClient>,
    state: Entity<State>,
}

pub struct State {
    settings_id: Option<Arc<str>>,
    api_key_state: ApiKeyState,
    settings: OpenAiCompatibleSettings,
    credentials_provider: Arc<dyn CredentialsProvider>,
    requires_api_key: bool,
    local_api_key: Option<Arc<str>>,
    local_models: Option<LocalModelManager>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct LocalModelSpec {
    id: String,
    display_name: String,
    file_name: String,
    download_url: Option<String>,
    import_path: Option<PathBuf>,
    download_size: u64,
    sha256: Option<String>,
    context_tokens: u64,
    output_tokens: u64,
    source_label: String,
    built_in: bool,
}

fn catalog_model(
    id: &str,
    display_name: &str,
    repo: &str,
    file_name: &str,
    download_size: u64,
    sha256: &str,
    source_label: &str,
) -> LocalModelSpec {
    LocalModelSpec {
        id: id.into(),
        display_name: display_name.into(),
        file_name: file_name.into(),
        download_url: Some(format!(
            "https://huggingface.co/{repo}/resolve/main/{file_name}"
        )),
        import_path: None,
        download_size,
        sha256: Some(sha256.into()),
        context_tokens: 4_096,
        output_tokens: 2_048,
        source_label: source_label.into(),
        built_in: true,
    }
}

fn sanitize_model_id(name: &str) -> String {
    let mut id = name.to_case(Case::Kebab);
    id.retain(|character| character.is_ascii_alphanumeric() || character == '-');
    if id.is_empty() { "model".into() } else { id }
}

fn local_model_catalog() -> Vec<LocalModelSpec> {
    vec![
        catalog_model(
            "qwen2.5-coder-1.5b-q4-k-m",
            "Qwen 2.5 Coder 1.5B Q4_K_M",
            "Qwen/Qwen2.5-Coder-1.5B-Instruct-GGUF",
            "qwen2.5-coder-1.5b-instruct-q4_k_m.gguf",
            1_117_320_768,
            "cc324af070c2ecbfd324a30884d2f951a7ff756aba85cb811a6ec436933bb046",
            "Official Qwen GGUF",
        ),
        catalog_model(
            "qwen2.5-coder-3b-q4-k-m",
            "Qwen 2.5 Coder 3B Q4_K_M",
            "Qwen/Qwen2.5-Coder-3B-Instruct-GGUF",
            "qwen2.5-coder-3b-instruct-q4_k_m.gguf",
            2_104_932_800,
            "724fb256bec1ff062b2f65e4569e871ad2e95ab2a3989723d1769c54294730b7",
            "Official Qwen GGUF",
        ),
        catalog_model(
            "qwen2.5-coder-7b-instruct-q5_k_m",
            "Qwen 2.5 Coder 7B Q5_K_M",
            "Qwen/Qwen2.5-Coder-7B-Instruct-GGUF",
            "qwen2.5-coder-7b-instruct-q5_k_m.gguf",
            5_444_831_232,
            "586844eac4d6d6321689f0192c8aa8e69cd8625974a5cc2d925b1a03366e4d16",
            "Official Qwen GGUF",
        ),
        catalog_model(
            "deepseek-r1-distill-1.5b-q4-k-m",
            "DeepSeek R1 Distill 1.5B Q4_K_M",
            "unsloth/DeepSeek-R1-Distill-Qwen-1.5B-GGUF",
            "DeepSeek-R1-Distill-Qwen-1.5B-Q4_K_M.gguf",
            1_117_321_312,
            "f3bdf9cf31dee4b57ae4e455a1cb0d01b5c2c1b50d72d3112141c195506c2840",
            "Unsloth community GGUF",
        ),
        catalog_model(
            "deepseek-r1-distill-7b-q4-k-m",
            "DeepSeek R1 Distill 7B Q4_K_M",
            "unsloth/DeepSeek-R1-Distill-Qwen-7B-GGUF",
            "DeepSeek-R1-Distill-Qwen-7B-Q4_K_M.gguf",
            4_683_073_248,
            "78272d8d32084548bd450394a560eb2d70de8232ab96a725769b1f9171235c1c",
            "Unsloth community GGUF",
        ),
        catalog_model(
            "gemma-3-1b-it-q4-k-m",
            "Gemma 3 1B IT Q4_K_M",
            "ggml-org/gemma-3-1b-it-GGUF",
            "gemma-3-1b-it-Q4_K_M.gguf",
            806_058_240,
            "8ccc5cd1f1b3602548715ae25a66ed73fd5dc68a210412eea643eb20eb75a135",
            "llama.cpp community GGUF",
        ),
        catalog_model(
            "gemma-3-4b-it-q4-k-m",
            "Gemma 3 4B IT Q4_K_M",
            "ggml-org/gemma-3-4b-it-GGUF",
            "gemma-3-4b-it-Q4_K_M.gguf",
            2_489_757_856,
            "882e8d2db44dc554fb0ea5077cb7e4bc49e7342a1f0da57901c0802ea21a0863",
            "llama.cpp community GGUF",
        ),
        catalog_model(
            "llama-3.2-1b-instruct-q4-k-m",
            "Llama 3.2 1B Instruct Q4_K_M",
            "bartowski/Llama-3.2-1B-Instruct-GGUF",
            "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            807_694_464,
            "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83",
            "Bartowski community GGUF",
        ),
        catalog_model(
            "llama-3.2-3b-instruct-q4-k-m",
            "Llama 3.2 3B Instruct Q4_K_M",
            "bartowski/Llama-3.2-3B-Instruct-GGUF",
            "Llama-3.2-3B-Instruct-Q4_K_M.gguf",
            2_019_377_696,
            "6c1a2b41161032677be168d354123594c0e6e67d2b9227c84f296ad037c728ff",
            "Bartowski community GGUF",
        ),
    ]
}

fn mobile_model_guidance(spec: &LocalModelSpec) -> &'static str {
    match spec.download_size {
        0..=1_300_000_000 => "Recommended for mobile Agent use",
        1_300_000_001..=2_300_000_000 => "Balanced; expect a slower first response",
        _ => "Experimental on phones; high heat and long first response",
    }
}

pub fn zdroid_local_settings() -> OpenAiCompatibleSettings {
    OpenAiCompatibleSettings {
        api_url: "http://127.0.0.1:8080/v1".into(),
        custom_headers: CustomHeaders::default(),
        available_models: local_model_catalog()
            .into_iter()
            .map(|model| AvailableModel {
                name: model.id.clone(),
                display_name: Some(format!("{} (Local)", model.display_name)),
                max_tokens: model.context_tokens,
                max_output_tokens: Some(model.output_tokens),
                max_completion_tokens: None,
                reasoning_effort: None,
                capabilities: ModelCapabilities {
                    tools: true,
                    images: false,
                    parallel_tool_calls: false,
                    prompt_cache_key: false,
                    chat_completions: true,
                    interleaved_reasoning: false,
                    max_tokens_parameter: false,
                },
            })
            .collect(),
    }
}

#[derive(Clone, Debug)]
enum LocalModelStatus {
    NotDownloaded,
    Downloaded,
    Downloading { downloaded: u64, total: u64 },
    Installing,
    Ready { server_running: bool },
    Error(String),
}

struct LocalModelState {
    spec: LocalModelSpec,
    status: LocalModelStatus,
    model_path: PathBuf,
    server: Option<smol::process::Child>,
}

#[derive(Clone)]
struct LocalModelCardData {
    spec: LocalModelSpec,
    status: LocalModelStatus,
    can_delete: bool,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default)]
struct LocalRuntimeConfig {
    threads: u16,
    context_tokens: u32,
    output_tokens: u32,
    batch_size: u32,
    vulkan: bool,
}

const MIN_AGENT_CONTEXT_TOKENS: u32 = 4_096;

impl LocalRuntimeConfig {
    fn normalized(mut self) -> Self {
        self.context_tokens = self.context_tokens.max(MIN_AGENT_CONTEXT_TOKENS);
        self.output_tokens = self.output_tokens.min(self.context_tokens);
        self
    }
}

impl Default for LocalRuntimeConfig {
    fn default() -> Self {
        Self {
            threads: 0,
            context_tokens: 4_096,
            output_tokens: 2_048,
            batch_size: 256,
            vulkan: true,
        }
    }
}

struct LocalModelManager {
    models: Vec<LocalModelState>,
    active_model_id: Option<String>,
    runtime: LocalRuntimeConfig,
}

impl State {
    fn is_authenticated(&self) -> bool {
        if self.local_models.is_some() {
            self.local_model_ready()
        } else {
            !self.requires_api_key || self.api_key_state.has_key()
        }
    }

    fn set_api_key(&mut self, api_key: Option<String>, cx: &mut Context<Self>) -> Task<Result<()>> {
        let credentials_provider = self.credentials_provider.clone();
        let api_url = SharedString::new(self.settings.api_url.as_str());
        self.api_key_state.store(
            api_url,
            api_key,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        )
    }

    fn authenticate(&mut self, cx: &mut Context<Self>) -> Task<Result<(), AuthenticateError>> {
        if !self.requires_api_key {
            return Task::ready(Ok(()));
        }
        let credentials_provider = self.credentials_provider.clone();
        let api_url = SharedString::new(self.settings.api_url.clone());
        self.api_key_state.load_if_needed(
            api_url,
            |this| &mut this.api_key_state,
            credentials_provider,
            cx,
        )
    }

    fn local_model_ready(&self) -> bool {
        self.local_models.as_ref().is_some_and(|manager| {
            manager.models.iter().any(|model| {
                matches!(
                    model.status,
                    LocalModelStatus::Ready {
                        server_running: true
                    }
                )
            })
        })
    }

    fn local_model_installed(&self) -> bool {
        self.local_models.as_ref().is_some_and(|manager| {
            manager
                .models
                .iter()
                .any(|model| model.model_path.is_file())
        })
    }

    fn install_local_model(
        &mut self,
        model_id: String,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(manager) = self.local_models.as_mut() else {
            return Task::ready(Ok(()));
        };
        let Some(model_index) = manager
            .models
            .iter()
            .position(|model| model.spec.id == model_id)
        else {
            return Task::ready(Err(anyhow::anyhow!("local model {model_id} was not found")));
        };
        for (index, model) in manager.models.iter_mut().enumerate() {
            if index != model_index {
                if let Some(mut server) = model.server.take() {
                    server.kill().ok();
                }
                if matches!(model.status, LocalModelStatus::Ready { .. }) {
                    model.status = LocalModelStatus::Ready {
                        server_running: false,
                    };
                }
            }
        }
        manager.active_model_id = Some(model_id.clone());
        let runtime = manager.runtime.clone();
        let Some(api_key) = self.local_api_key.clone() else {
            return Task::ready(Err(anyhow::anyhow!(
                "local model authentication token is unavailable"
            )));
        };
        let local_model = &mut manager.models[model_index];
        if matches!(
            local_model.status,
            LocalModelStatus::Downloading { .. } | LocalModelStatus::Installing
        ) {
            return Task::ready(Ok(()));
        }

        local_model.status = if local_model.model_path.is_file() {
            LocalModelStatus::Installing
        } else {
            LocalModelStatus::Downloading {
                downloaded: local_model
                    .model_path
                    .with_extension("gguf.part")
                    .metadata()
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
                total: local_model.spec.download_size,
            }
        };
        let spec = local_model.spec.clone();
        let model_path = local_model.model_path.clone();
        let task_id = format!("zdroid-local-{}", spec.id);
        let description = if model_path.is_file() {
            format!("Local LLM: {} is starting", spec.display_name)
        } else if spec.import_path.is_some() {
            format!("Local LLM: {} is importing", spec.display_name)
        } else {
            format!("Local LLM: {} is downloading", spec.display_name)
        };
        cx.start_background_task(&task_id, &description);
        cx.notify();

        cx.spawn(async move |this, cx| {
            let result =
                install_local_model(&this, spec.clone(), &model_path, runtime, api_key, cx).await;
            let successful = result.is_ok();
            let error_message = result.as_ref().err().map(|error| format!("{error:#}"));
            this.update(cx, |this, cx| {
                if let Some(local_model) = this.local_models.as_mut().and_then(|manager| {
                    manager
                        .models
                        .iter_mut()
                        .find(|model| model.spec.id == spec.id)
                }) {
                    local_model.status = match error_message {
                        Some(error) => LocalModelStatus::Error(error),
                        None => LocalModelStatus::Ready {
                            server_running: local_model.server.is_some(),
                        },
                    };
                }
                let finished_description = if successful {
                    format!("Local LLM: {} is ready", spec.display_name)
                } else {
                    format!("Local LLM: {} setup failed", spec.display_name)
                };
                cx.finish_background_task(&task_id, &finished_description, successful);
                cx.notify();
            })?;
            result
        })
    }

    fn add_custom_model(&mut self, spec: LocalModelSpec, cx: &mut Context<Self>) -> Result<String> {
        validate_local_model_spec(&spec)?;
        let manager = self
            .local_models
            .as_mut()
            .context("Local LLM manager is unavailable")?;
        if manager.models.iter().any(|model| model.spec.id == spec.id) {
            bail!("A local model with this name already exists");
        }
        let id = spec.id.clone();
        manager.models.push(LocalModelState {
            model_path: local_model_path(&spec),
            spec,
            status: LocalModelStatus::NotDownloaded,
            server: None,
        });
        save_custom_models(&manager.models)?;
        cx.notify();
        Ok(id)
    }

    fn apply_local_runtime(
        &mut self,
        runtime: LocalRuntimeConfig,
        cx: &mut Context<Self>,
    ) -> Task<Result<()>> {
        let Some(manager) = self.local_models.as_mut() else {
            return Task::ready(Ok(()));
        };

        let restart_model_id = manager.active_model_id.clone().filter(|active_id| {
            manager
                .models
                .iter()
                .any(|model| model.spec.id == *active_id && model.server.is_some())
        });
        manager.runtime = runtime.normalized();
        for model in &mut manager.models {
            if let Some(mut server) = model.server.take() {
                server.kill().ok();
            }
            if matches!(model.status, LocalModelStatus::Ready { .. }) {
                model.status = LocalModelStatus::Ready {
                    server_running: false,
                };
            }
        }
        save_runtime_config(&manager.runtime).log_err();
        cx.notify();

        if let Some(model_id) = restart_model_id {
            self.install_local_model(model_id, cx)
        } else {
            Task::ready(Ok(()))
        }
    }

    fn delete_local_model(&mut self, model_id: &str, cx: &mut Context<Self>) -> Result<()> {
        let manager = self
            .local_models
            .as_mut()
            .context("Local LLM manager is unavailable")?;
        let index = manager
            .models
            .iter()
            .position(|model| model.spec.id == model_id)
            .context("Local model was not found")?;
        let model = &mut manager.models[index];
        if let Some(mut server) = model.server.take() {
            server.kill().ok();
        }
        for path in [
            model.model_path.clone(),
            model.model_path.with_extension("gguf.part"),
            model.model_path.with_extension("llama-server.log"),
        ] {
            if path.is_file() {
                std::fs::remove_file(&path)
                    .with_context(|| format!("delete {}", path.display()))?;
            }
        }
        if model.spec.built_in {
            model.status = LocalModelStatus::NotDownloaded;
        } else {
            manager.models.remove(index);
        }
        if manager.active_model_id.as_deref() == Some(model_id) {
            manager.active_model_id = None;
        }
        save_custom_models(&manager.models)?;
        cx.notify();
        Ok(())
    }
}

fn local_model_path(spec: &LocalModelSpec) -> PathBuf {
    paths::home_dir()
        .join(".local/share/zdroid/models")
        .join(&spec.file_name)
}

fn validate_local_model_spec(spec: &LocalModelSpec) -> Result<()> {
    let path = Path::new(&spec.file_name);
    if path.file_name().and_then(|name| name.to_str()) != Some(spec.file_name.as_str())
        || !spec.file_name.to_ascii_lowercase().ends_with(".gguf")
    {
        bail!("Local model filename must be a single .gguf filename");
    }
    if spec
        .download_url
        .as_ref()
        .is_some_and(|url| !url.starts_with("https://"))
    {
        bail!("Local model URL must use HTTPS");
    }
    if spec.sha256.as_ref().is_some_and(|sha256| {
        sha256.len() != 64 || !sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        bail!("Local model SHA-256 is invalid");
    }
    Ok(())
}

fn local_models_dir() -> PathBuf {
    paths::home_dir().join(".local/share/zdroid/models")
}

fn dpkg_package_is_installed(prefix: &Path, package: &str) -> bool {
    let Ok(status) = std::fs::read_to_string(prefix.join("var/lib/dpkg/status")) else {
        return false;
    };
    status.split("\n\n").any(|paragraph| {
        paragraph
            .lines()
            .any(|line| line == format!("Package: {package}"))
            && paragraph
                .lines()
                .any(|line| line == "Status: install ok installed")
    })
}

fn runtime_config_path() -> PathBuf {
    local_models_dir().join("runtime.json")
}

fn custom_models_path() -> PathBuf {
    local_models_dir().join("custom-models.json")
}

fn load_runtime_config() -> Result<LocalRuntimeConfig> {
    let path = runtime_config_path();
    if !path.is_file() {
        return Ok(LocalRuntimeConfig::default());
    }
    let stored: LocalRuntimeConfig = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("read {}", path.display()))?;
    let normalized = stored.clone().normalized();
    if normalized != stored {
        save_runtime_config(&normalized)?;
    }
    Ok(normalized)
}

fn save_runtime_config(config: &LocalRuntimeConfig) -> Result<()> {
    std::fs::create_dir_all(local_models_dir())?;
    std::fs::write(runtime_config_path(), serde_json::to_vec_pretty(config)?)?;
    Ok(())
}

fn load_custom_models() -> Result<Vec<LocalModelSpec>> {
    let path = custom_models_path();
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let models: Vec<LocalModelSpec> = serde_json::from_slice(&std::fs::read(&path)?)
        .with_context(|| format!("read {}", path.display()))?;
    models
        .into_iter()
        .map(|model| {
            validate_local_model_spec(&model)?;
            Ok(model)
        })
        .collect()
}

fn save_custom_models(models: &[LocalModelState]) -> Result<()> {
    std::fs::create_dir_all(local_models_dir())?;
    let specs = models
        .iter()
        .filter(|model| !model.spec.built_in)
        .map(|model| model.spec.clone())
        .collect::<Vec<_>>();
    std::fs::write(custom_models_path(), serde_json::to_vec_pretty(&specs)?)?;
    Ok(())
}

fn local_model_available(spec: &LocalModelSpec, runtime: &LocalRuntimeConfig) -> AvailableModel {
    AvailableModel {
        name: spec.id.clone(),
        display_name: Some(format!("{} (Local)", spec.display_name)),
        max_tokens: runtime.context_tokens as u64,
        max_output_tokens: Some(runtime.output_tokens as u64),
        max_completion_tokens: None,
        reasoning_effort: None,
        capabilities: ModelCapabilities {
            tools: true,
            images: false,
            parallel_tool_calls: false,
            prompt_cache_key: false,
            chat_completions: true,
            interleaved_reasoning: false,
            max_tokens_parameter: false,
        },
    }
}

fn bootstrap_prefix() -> Result<PathBuf> {
    std::env::var_os("PREFIX")
        .map(PathBuf::from)
        .context("Zdroid Bootstrap is not active; install it from the runtime picker first")
}

fn update_local_download_progress(
    state: &gpui::WeakEntity<State>,
    model_id: &str,
    downloaded: u64,
    total: u64,
    cx: &mut AsyncApp,
) {
    state
        .update(cx, |state, cx| {
            if let Some(model) = state.local_models.as_mut().and_then(|manager| {
                manager
                    .models
                    .iter_mut()
                    .find(|model| model.spec.id == model_id)
            }) {
                model.status = LocalModelStatus::Downloading { downloaded, total };
            }
            cx.notify();
        })
        .ok();
}

async fn install_local_model(
    state: &gpui::WeakEntity<State>,
    spec: LocalModelSpec,
    model_path: &Path,
    runtime: LocalRuntimeConfig,
    api_key: Arc<str>,
    cx: &mut AsyncApp,
) -> Result<()> {
    let prefix = bootstrap_prefix()?;
    let part_path = model_path.with_extension("gguf.part");
    if let Some(parent) = model_path.parent() {
        smol::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("create local model directory {}", parent.display()))?;
    }

    if !model_path.is_file() {
        if spec.download_size > 0
            && part_path
                .metadata()
                .is_ok_and(|metadata| metadata.len() > spec.download_size)
        {
            smol::fs::remove_file(&part_path).await.ok();
        }
        if let Some(import_path) = &spec.import_path {
            let mut source = smol::fs::File::open(import_path)
                .await
                .with_context(|| format!("open imported GGUF {}", import_path.display()))?;
            let total = source.metadata().await?.len();
            let mut destination = smol::fs::File::create(&part_path).await?;
            let mut buffer = vec![0_u8; 1024 * 1024];
            let mut copied = 0_u64;
            loop {
                let read = source.read(&mut buffer).await?;
                if read == 0 {
                    break;
                }
                futures::AsyncWriteExt::write_all(&mut destination, &buffer[..read]).await?;
                copied += read as u64;
                update_local_download_progress(state, &spec.id, copied, total, cx);
            }
            futures::AsyncWriteExt::flush(&mut destination).await?;
        } else if !part_path
            .metadata()
            .is_ok_and(|metadata| spec.download_size > 0 && metadata.len() == spec.download_size)
        {
            let download_url = spec
                .download_url
                .as_deref()
                .context("local model has no download URL")?;
            if !download_url.starts_with("https://") {
                bail!("GGUF downloads must use HTTPS");
            }
            let curl = prefix.join("bin/curl");
            let mut child = smol::process::Command::new(&curl)
                .args([
                    "--fail",
                    "--location",
                    "--proto",
                    "=https",
                    "--proto-redir",
                    "=https",
                    "--max-filesize",
                    "8589934592",
                    "--continue-at",
                    "-",
                ])
                .arg("--output")
                .arg(&part_path)
                .arg(download_url)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .with_context(|| format!("start model download with {}", curl.display()))?;

            loop {
                if let Some(status) = child.try_status().context("check model download status")? {
                    if !status.success() {
                        bail!("model download exited with {status}");
                    }
                    break;
                }
                let downloaded = smol::fs::metadata(&part_path)
                    .await
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
                update_local_download_progress(state, &spec.id, downloaded, spec.download_size, cx);
                smol::Timer::after(Duration::from_millis(500)).await;
            }
        }

        state.update(cx, |state, cx| {
            if let Some(local_model) = state.local_models.as_mut().and_then(|manager| {
                manager
                    .models
                    .iter_mut()
                    .find(|model| model.spec.id == spec.id)
            }) {
                local_model.status = LocalModelStatus::Installing;
            }
            cx.start_background_task(
                &format!("zdroid-local-{}", spec.id),
                &format!("Local LLM: {} is installing", spec.display_name),
            );
            cx.notify();
        })?;

        let metadata = smol::fs::metadata(&part_path)
            .await
            .context("read downloaded model metadata")?;
        if spec.download_size > 0 && metadata.len() != spec.download_size {
            bail!(
                "downloaded model size is {} bytes; expected {}",
                metadata.len(),
                spec.download_size,
            );
        }
        let mut file = smol::fs::File::open(&part_path)
            .await
            .context("open downloaded model for verification")?;
        let mut magic = [0_u8; 4];
        file.read_exact(&mut magic)
            .await
            .context("read GGUF header")?;
        if &magic != b"GGUF" {
            smol::fs::remove_file(&part_path).await.ok();
            bail!("the selected file is not a GGUF model; its header is invalid");
        }
        let mut hasher = Sha256::new();
        hasher.update(magic);
        let mut buffer = vec![0_u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        let digest = format!("{:x}", hasher.finalize());
        if spec
            .sha256
            .as_ref()
            .is_some_and(|expected| digest != *expected)
        {
            smol::fs::remove_file(&part_path).await.ok();
            bail!(
                "downloaded model checksum did not match the verified {} file; the invalid download was removed",
                spec.display_name
            );
        }
        if !spec.built_in {
            state.update(cx, |state, _| {
                if let Some(manager) = state.local_models.as_mut() {
                    if let Some(model) = manager
                        .models
                        .iter_mut()
                        .find(|model| model.spec.id == spec.id)
                    {
                        model.spec.sha256 = Some(digest.clone());
                        model.spec.download_size = metadata.len();
                        model.spec.import_path = None;
                    }
                    save_custom_models(&manager.models).log_err();
                }
            })?;
        }
        smol::fs::rename(&part_path, model_path)
            .await
            .context("install verified local model")?;
    } else {
        state.update(cx, |state, cx| {
            if let Some(local_model) = state.local_models.as_mut().and_then(|manager| {
                manager
                    .models
                    .iter_mut()
                    .find(|model| model.spec.id == spec.id)
            }) {
                local_model.status = LocalModelStatus::Installing;
            }
            cx.start_background_task(
                &format!("zdroid-local-{}", spec.id),
                &format!("Local LLM: {} is starting", spec.display_name),
            );
            cx.notify();
        })?;
    }

    let server_path = prefix.join("bin/llama-server");
    let vulkan_backend_path = prefix.join("lib/libggml-vulkan.so");
    let android_vulkan_loader_installed =
        dpkg_package_is_installed(&prefix, "vulkan-loader-android");
    if !server_path.is_file() || !vulkan_backend_path.is_file() || !android_vulkan_loader_installed
    {
        let package_manager = prefix.join(".zed/bin/pkg");
        let status = smol::process::Command::new(&package_manager)
            .args([
                "install",
                "-y",
                "llama-cpp",
                "llama-cpp-backend-vulkan",
                "vulkan-loader-android",
                "vulkan-loader-generic-",
                "-o",
                "Dpkg::Options::=--force-confdef",
                "-o",
                "Dpkg::Options::=--force-confold",
            ])
            .env("PREFIX", &prefix)
            .env("LD_LIBRARY_PATH", prefix.join("lib"))
            .env("TERMUX_APP__PACKAGE_NAME", "com.zdroid")
            .env("DEBIAN_FRONTEND", "noninteractive")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await
            .with_context(|| format!("install llama.cpp with {}", package_manager.display()))?;
        if !status.success() {
            bail!("llama.cpp installation exited with {status}");
        }
    }

    let server_log_path = model_path.with_extension("llama-server.log");
    let spawn_server = |use_vulkan: bool| -> Result<smol::process::Child> {
        let stdout = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&server_log_path)
            .with_context(|| format!("open local model log {}", server_log_path.display()))?;
        let stderr = stdout.try_clone().context("clone local model log handle")?;
        let mut command = smol::process::Command::new(&server_path);
        command
            .arg("-m")
            .arg(model_path)
            .args([
                "--alias",
                &spec.id,
                "--host",
                "127.0.0.1",
                "--port",
                "8080",
                "--api-key",
                api_key.as_ref(),
                "--no-webui",
                "--slots",
                "--cache-prompt",
                "--cache-reuse",
                "256",
                "--cache-ram",
                "128",
                "--cache-type-k",
                "q8_0",
                "--cache-type-v",
                "q8_0",
                "--flash-attn",
                "auto",
                "-c",
                &runtime.context_tokens.to_string(),
                "-np",
                "1",
                "-n",
                &runtime.output_tokens.to_string(),
                "-b",
                &runtime.batch_size.to_string(),
            ])
            .env("PREFIX", &prefix)
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        if runtime.threads > 0 {
            command.args([
                "--threads",
                &runtime.threads.to_string(),
                "--threads-batch",
                &runtime.threads.to_string(),
            ]);
        }
        if use_vulkan && runtime.vulkan {
            command.env("GGML_BACKEND_PATH", &vulkan_backend_path);
            command.args(["--gpu-layers", "99"]);
        } else {
            command.args(["--device", "none"]);
        }
        command
            .spawn()
            .with_context(|| format!("start local model server {}", server_path.display()))
    };

    let mut using_vulkan = runtime.vulkan && vulkan_backend_path.is_file();
    let mut child = spawn_server(using_vulkan)?;
    let health_url = "http://127.0.0.1:8080/health";
    let mut healthy = false;
    for _ in 0..480 {
        if let Some(status) = child.try_status().context("check local model server")? {
            if using_vulkan {
                log::warn!(
                    "local model Vulkan server exited before becoming ready ({status}); retrying on CPU; log: {}",
                    server_log_path.display()
                );
                using_vulkan = false;
                child = spawn_server(false)?;
                continue;
            }
            bail!(
                "local model server exited before becoming ready ({status}); see {}",
                server_log_path.display()
            );
        }
        let health = smol::process::Command::new(prefix.join("bin/curl"))
            .args(["--fail", "--silent", "--output", "/dev/null", health_url])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;
        if health.is_ok_and(|status| status.success()) {
            healthy = true;
            break;
        }
        smol::Timer::after(Duration::from_millis(500)).await;
    }
    if !healthy {
        child.kill().ok();
        bail!(
            "local model server did not become ready within 4 minutes; see {}",
            server_log_path.display()
        );
    }
    state.update(cx, |state, _| {
        if let Some(local_model) = state.local_models.as_mut().and_then(|manager| {
            manager
                .models
                .iter_mut()
                .find(|model| model.spec.id == spec.id)
        }) {
            local_model.server = Some(child);
        }
    })?;
    Ok(())
}

impl OpenAiCompatibleLanguageModelProvider {
    pub fn new(
        id: Arc<str>,
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        Self::new_inner(
            id.clone(),
            id,
            None,
            true,
            http_client,
            credentials_provider,
            cx,
        )
    }

    pub fn new_local(
        id: Arc<str>,
        name: Arc<str>,
        settings: OpenAiCompatibleSettings,
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        Self::new_inner(
            id,
            name,
            Some(settings),
            false,
            http_client,
            credentials_provider,
            cx,
        )
    }

    fn new_inner(
        id: Arc<str>,
        name: Arc<str>,
        static_settings: Option<OpenAiCompatibleSettings>,
        requires_api_key: bool,
        http_client: Arc<dyn HttpClient>,
        credentials_provider: Arc<dyn CredentialsProvider>,
        cx: &mut App,
    ) -> Self {
        fn resolve_settings<'a>(id: &'a str, cx: &'a App) -> Option<&'a OpenAiCompatibleSettings> {
            crate::AllLanguageModelSettings::get_global(cx)
                .openai_compatible
                .get(id)
        }

        let api_key_env_var_name = format!("{}_API_KEY", id).to_case(Case::UpperSnake).into();
        let state_id = id.clone();
        let state = cx.new(move |cx| {
            cx.observe_global::<SettingsStore>(|this: &mut State, cx| {
                let Some(settings_id) = this.settings_id.as_deref() else {
                    return;
                };
                let Some(settings) = resolve_settings(settings_id, cx).cloned() else {
                    return;
                };
                if &this.settings != &settings {
                    let credentials_provider = this.credentials_provider.clone();
                    let api_url = SharedString::new(settings.api_url.as_str());
                    this.api_key_state.handle_url_change(
                        api_url,
                        |this| &mut this.api_key_state,
                        credentials_provider,
                        cx,
                    );
                    this.settings = settings;
                    cx.notify();
                }
            })
            .detach();
            let settings = static_settings
                .clone()
                .or_else(|| resolve_settings(&state_id, cx).cloned())
                .unwrap_or_default();
            let local_models = (!requires_api_key).then(|| {
                let runtime_ready = bootstrap_prefix()
                    .map(|prefix| prefix.join("bin/llama-server").is_file())
                    .unwrap_or(false);
                let mut specs = local_model_catalog();
                specs.extend(load_custom_models().unwrap_or_default());
                let models = specs
                    .into_iter()
                    .map(|spec| {
                        let model_path = local_model_path(&spec);
                        LocalModelState {
                            spec,
                            status: if model_path.is_file() && runtime_ready {
                                LocalModelStatus::Ready {
                                    server_running: false,
                                }
                            } else if model_path.is_file() {
                                LocalModelStatus::Downloaded
                            } else {
                                LocalModelStatus::NotDownloaded
                            },
                            model_path,
                            server: None,
                        }
                    })
                    .collect::<Vec<_>>();
                LocalModelManager {
                    active_model_id: models
                        .iter()
                        .find(|model| model.model_path.is_file())
                        .map(|model| model.spec.id.clone()),
                    models,
                    runtime: load_runtime_config().unwrap_or_default(),
                }
            });
            let local_api_key = (!requires_api_key).then(|| {
                Arc::<str>::from(format!(
                    "{:032x}{:032x}",
                    rand::random::<u128>(),
                    rand::random::<u128>()
                ))
            });
            State {
                settings_id: static_settings.is_none().then(|| state_id.clone()),
                api_key_state: ApiKeyState::new(
                    SharedString::new(settings.api_url.as_str()),
                    EnvVar::new(api_key_env_var_name),
                ),
                settings,
                credentials_provider,
                requires_api_key,
                local_api_key,
                local_models,
            }
        });

        if !requires_api_key && state.read(cx).local_model_installed() {
            let model_id = state
                .read(cx)
                .local_models
                .as_ref()
                .and_then(|manager| manager.active_model_id.clone());
            if let Some(model_id) = model_id {
                state
                    .update(cx, |state, cx| state.install_local_model(model_id, cx))
                    .detach_and_log_err(cx);
            }
        }

        Self {
            id: id.clone().into(),
            name: name.into(),
            http_client,
            state,
        }
    }

    fn create_language_model(&self, model: AvailableModel) -> Arc<dyn LanguageModel> {
        Arc::new(OpenAiCompatibleLanguageModel {
            id: LanguageModelId::from(model.name.clone()),
            provider_id: self.id.clone(),
            provider_name: self.name.clone(),
            model,
            state: self.state.clone(),
            http_client: self.http_client.clone(),
            request_limiter: RateLimiter::new(4),
        })
    }
}

impl LanguageModelProviderState for OpenAiCompatibleLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<Entity<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OpenAiCompatibleLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelProviderName {
        self.name.clone()
    }

    fn icon(&self) -> IconOrSvg {
        IconOrSvg::Icon(IconName::AiOpenAiCompat)
    }

    fn default_model(&self, cx: &App) -> Option<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        if let Some(manager) = &state.local_models {
            let model = manager.models.iter().find(|model| {
                matches!(
                    model.status,
                    LocalModelStatus::Ready {
                        server_running: true
                    }
                )
            })?;
            return Some(
                self.create_language_model(local_model_available(&model.spec, &manager.runtime)),
            );
        }
        state
            .settings
            .available_models
            .first()
            .map(|model| self.create_language_model(model.clone()))
    }

    fn default_fast_model(&self, _cx: &App) -> Option<Arc<dyn LanguageModel>> {
        None
    }

    fn provided_models(&self, cx: &App) -> Vec<Arc<dyn LanguageModel>> {
        let state = self.state.read(cx);
        if let Some(manager) = &state.local_models {
            return manager
                .models
                .iter()
                .filter(|model| {
                    matches!(
                        model.status,
                        LocalModelStatus::Ready {
                            server_running: true
                        }
                    )
                })
                .map(|model| {
                    self.create_language_model(local_model_available(&model.spec, &manager.runtime))
                })
                .collect();
        }
        state
            .settings
            .available_models
            .iter()
            .map(|model| self.create_language_model(model.clone()))
            .collect()
    }

    fn is_authenticated(&self, cx: &App) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut App) -> Task<Result<(), AuthenticateError>> {
        self.state.update(cx, |state, cx| state.authenticate(cx))
    }

    fn settings_view(&self, _cx: &mut App) -> Option<ProviderSettingsView> {
        let state = self.state.clone();
        Some(ProviderSettingsView::SubPage(SubPageProviderSettings::new(
            move |window, cx| {
                cx.new(|cx| ConfigurationView::new(state.clone(), window, cx))
                    .into()
            },
        )))
    }

    fn set_api_key(&self, api_key: Option<String>, cx: &mut App) -> Task<Result<()>> {
        self.state
            .update(cx, |state, cx| state.set_api_key(api_key, cx))
    }
}

pub struct OpenAiCompatibleLanguageModel {
    id: LanguageModelId,
    provider_id: LanguageModelProviderId,
    provider_name: LanguageModelProviderName,
    model: AvailableModel,
    state: Entity<State>,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

#[derive(Debug, Deserialize)]
struct LocalSlotState {
    n_ctx: u64,
    #[serde(default)]
    n_prompt_tokens: u64,
    #[serde(default)]
    n_prompt_tokens_processed: u64,
    #[serde(default)]
    n_prompt_tokens_cache: u64,
    #[serde(default)]
    next_token: LocalSlotNextToken,
    #[serde(default)]
    is_processing: bool,
}

#[derive(Debug, Default, Deserialize)]
struct LocalSlotNextToken {
    #[serde(default)]
    n_decoded: u64,
}

#[derive(Clone, Copy)]
struct TimedLocalSlotState {
    observed_at: Instant,
    prompt_tokens_processed: u64,
    output_tokens: u64,
}

async fn fetch_local_slot_state(
    http_client: &dyn HttpClient,
    api_url: &str,
    api_key: &str,
) -> Option<LocalSlotState> {
    let server_url = api_url.trim_end_matches('/').trim_end_matches("/v1");
    let request = HttpRequest::builder()
        .method(Method::GET)
        .uri(format!("{server_url}/slots"))
        .header("Authorization", format!("Bearer {}", api_key.trim()))
        .body(AsyncBody::default())
        .ok()?;
    let mut response = http_client.send(request).await.ok()?;
    if !response.status().is_success() {
        return None;
    }

    let mut body = String::new();
    response.body_mut().read_to_string(&mut body).await.ok()?;
    serde_json::from_str::<Vec<LocalSlotState>>(&body)
        .ok()?
        .into_iter()
        .find(|slot| slot.is_processing)
}

fn stream_with_local_inference_progress(
    completions: futures::stream::BoxStream<
        'static,
        Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
    >,
    http_client: Arc<dyn HttpClient>,
    api_url: String,
    api_key: Arc<str>,
) -> futures::stream::BoxStream<
    'static,
    Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
> {
    struct ProgressStreamState {
        completions: futures::stream::BoxStream<
            'static,
            Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
        >,
        http_client: Arc<dyn HttpClient>,
        api_url: String,
        api_key: Arc<str>,
        next_poll: Instant,
        previous: Option<TimedLocalSlotState>,
    }

    futures::stream::unfold(
        ProgressStreamState {
            completions,
            http_client,
            api_url,
            api_key,
            next_poll: Instant::now(),
            previous: None,
        },
        |mut state| async move {
            loop {
                let completion = FutureExt::fuse(state.completions.next());
                let timer = FutureExt::fuse(smol::Timer::at(state.next_poll));
                futures::pin_mut!(completion, timer);

                futures::select_biased! {
                    _ = timer => {
                        state.next_poll = Instant::now() + Duration::from_millis(750);
                        let Some(slot) = fetch_local_slot_state(
                            state.http_client.as_ref(),
                            &state.api_url,
                            &state.api_key,
                        ).await else {
                            continue;
                        };

                        let observed_at = Instant::now();
                        let prompt_tokens = slot
                            .n_prompt_tokens_processed
                            .saturating_add(slot.n_prompt_tokens_cache);
                        let output_tokens = slot.next_token.n_decoded;
                        let phase = if output_tokens > 0 {
                            InferencePhase::Generating
                        } else {
                            InferencePhase::Prefilling
                        };
                        let tokens_per_second = state.previous.and_then(|previous| {
                            let elapsed = observed_at.duration_since(previous.observed_at).as_secs_f64();
                            if elapsed <= 0.0 {
                                return None;
                            }
                            let tokens = match phase {
                                InferencePhase::Generating => output_tokens
                                    .saturating_sub(previous.output_tokens),
                                InferencePhase::Prefilling => slot
                                    .n_prompt_tokens_processed
                                    .saturating_sub(previous.prompt_tokens_processed),
                                InferencePhase::Complete => 0,
                            };
                            (tokens > 0).then_some(tokens as f64 / elapsed)
                        });
                        state.previous = Some(TimedLocalSlotState {
                            observed_at,
                            prompt_tokens_processed: slot.n_prompt_tokens_processed,
                            output_tokens,
                        });

                        let progress = InferenceProgress {
                            phase,
                            context_tokens: slot
                                .n_prompt_tokens
                                .max(prompt_tokens.saturating_add(output_tokens)),
                            context_limit: slot.n_ctx,
                            prompt_tokens,
                            output_tokens,
                            cached_tokens: slot.n_prompt_tokens_cache,
                            tokens_per_second,
                        };
                        return Some((
                            Ok(LanguageModelCompletionEvent::InferenceProgress(progress)),
                            state,
                        ));
                    }
                    event = completion => return event.map(|event| (event, state)),
                }
            }
        },
    )
    .boxed()
}

impl OpenAiCompatibleLanguageModel {
    fn stream_completion(
        &self,
        request: open_ai::Request,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<'static, Result<ResponseStreamEvent>>,
            LanguageModelCompletionError,
        >,
    > {
        let http_client = self.http_client.clone();

        let (api_key, local_api_key, api_url, requires_api_key, custom_headers) =
            self.state.read_with(cx, |state, _cx| {
                let api_url = &state.settings.api_url;
                (
                    state.api_key_state.key(api_url),
                    state.local_api_key.clone(),
                    state.settings.api_url.clone(),
                    state.requires_api_key,
                    state.settings.custom_headers.clone(),
                )
            });

        let provider = self.provider_name.clone();
        let future = self.request_limiter.stream(async move {
            let api_key = match (api_key, local_api_key, requires_api_key) {
                (Some(api_key), _, _) => api_key,
                (None, Some(api_key), false) => api_key,
                (None, None, false) => {
                    return Err(LanguageModelCompletionError::NoApiKey { provider });
                }
                (None, _, true) => {
                    return Err(LanguageModelCompletionError::NoApiKey { provider });
                }
            };
            let request = stream_completion(
                http_client.as_ref(),
                provider.0.as_str(),
                &api_url,
                &api_key,
                request,
                &custom_headers,
            );
            let response = request.await?;
            Ok(response)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }

    fn stream_response(
        &self,
        request: ResponseRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<'static, Result<futures::stream::BoxStream<'static, Result<ResponsesStreamEvent>>>>
    {
        let http_client = self.http_client.clone();

        let (api_key, local_api_key, api_url, requires_api_key, custom_headers) =
            self.state.read_with(cx, |state, _cx| {
                let api_url = &state.settings.api_url;
                (
                    state.api_key_state.key(api_url),
                    state.local_api_key.clone(),
                    state.settings.api_url.clone(),
                    state.requires_api_key,
                    state.settings.custom_headers.clone(),
                )
            });

        let provider = self.provider_name.clone();
        let future = self.request_limiter.stream(async move {
            let api_key = match (api_key, local_api_key, requires_api_key) {
                (Some(api_key), _, _) => api_key,
                (None, Some(api_key), false) => api_key,
                (None, None, false) => {
                    return Err(LanguageModelCompletionError::NoApiKey { provider });
                }
                (None, _, true) => {
                    return Err(LanguageModelCompletionError::NoApiKey { provider });
                }
            };
            let request = stream_response(
                http_client.as_ref(),
                provider.0.as_str(),
                &api_url,
                &api_key,
                request,
                &custom_headers,
            );
            let response = request.await?;
            Ok(response)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }
}

fn default_thinking_reasoning_effort(model: &AvailableModel) -> Option<open_ai::ReasoningEffort> {
    model
        .reasoning_effort
        .filter(|effort| *effort != open_ai::ReasoningEffort::None)
}

fn chat_completion_max_tokens_parameter(
    model: &AvailableModel,
) -> ChatCompletionMaxTokensParameter {
    if model.capabilities.max_tokens_parameter {
        ChatCompletionMaxTokensParameter::MaxTokens
    } else {
        ChatCompletionMaxTokensParameter::MaxCompletionTokens
    }
}

fn selected_thinking_reasoning_effort(
    request: &LanguageModelRequest,
) -> Option<open_ai::ReasoningEffort> {
    request
        .thinking_effort
        .as_deref()
        .and_then(|effort| effort.parse::<open_ai::ReasoningEffort>().ok())
        .filter(|effort| *effort != open_ai::ReasoningEffort::None)
}

fn chat_completion_reasoning_effort(
    request: &LanguageModelRequest,
    model: &AvailableModel,
) -> Option<open_ai::ReasoningEffort> {
    if model.reasoning_effort == Some(open_ai::ReasoningEffort::None) {
        return Some(open_ai::ReasoningEffort::None);
    }
    if request.thinking_allowed {
        selected_thinking_reasoning_effort(request)
            .or_else(|| default_thinking_reasoning_effort(model))
    } else if model.reasoning_effort.is_some() {
        Some(open_ai::ReasoningEffort::None)
    } else {
        None
    }
}

fn disable_response_thinking_for_none_effort(
    request: &mut LanguageModelRequest,
    model: &AvailableModel,
) {
    if model.reasoning_effort == Some(open_ai::ReasoningEffort::None) {
        request.thinking_allowed = false;
        request.thinking_effort = None;
    }
}

impl LanguageModel for OpenAiCompatibleLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(
            self.model
                .display_name
                .clone()
                .unwrap_or_else(|| self.model.name.clone()),
        )
    }

    fn provider_id(&self) -> LanguageModelProviderId {
        self.provider_id.clone()
    }

    fn provider_name(&self) -> LanguageModelProviderName {
        self.provider_name.clone()
    }

    fn supports_tools(&self) -> bool {
        self.model.capabilities.tools
    }

    fn tool_input_format(&self) -> LanguageModelToolSchemaFormat {
        LanguageModelToolSchemaFormat::JsonSchemaSubset
    }

    fn supports_images(&self) -> bool {
        self.model.capabilities.images
    }

    fn supports_tool_choice(&self, choice: LanguageModelToolChoice) -> bool {
        match choice {
            LanguageModelToolChoice::Auto => self.model.capabilities.tools,
            LanguageModelToolChoice::Any => self.model.capabilities.tools,
            LanguageModelToolChoice::None => true,
        }
    }

    fn supports_streaming_tools(&self) -> bool {
        true
    }

    fn supports_split_token_display(&self) -> bool {
        true
    }

    fn telemetry_id(&self) -> String {
        format!("openai/{}", self.model.name)
    }

    fn max_token_count(&self) -> u64 {
        self.model.max_tokens
    }

    fn max_output_tokens(&self) -> Option<u64> {
        self.model.max_output_tokens
    }

    fn stream_completion(
        &self,
        mut request: LanguageModelRequest,
        cx: &AsyncApp,
    ) -> BoxFuture<
        'static,
        Result<
            futures::stream::BoxStream<
                'static,
                Result<LanguageModelCompletionEvent, LanguageModelCompletionError>,
            >,
            LanguageModelCompletionError,
        >,
    > {
        if !self.supports_fast_mode() {
            request.speed = None;
        }

        if self.model.capabilities.chat_completions {
            let local_metrics = (self.provider_id.0.as_ref() == "zdroid-local").then(|| {
                self.state.read_with(cx, |state, _cx| {
                    (
                        self.http_client.clone(),
                        state.settings.api_url.clone(),
                        state.local_api_key.clone(),
                    )
                })
            });
            let reasoning_effort = chat_completion_reasoning_effort(&request, &self.model);
            let request = match into_open_ai(
                request,
                &self.model.name,
                self.model.capabilities.parallel_tool_calls,
                self.model.capabilities.prompt_cache_key,
                self.max_output_tokens(),
                chat_completion_max_tokens_parameter(&self.model),
                reasoning_effort,
                self.model.capabilities.interleaved_reasoning,
            ) {
                Ok(request) => request,
                Err(error) => return async move { Err(error.into()) }.boxed(),
            };
            let completions = self.stream_completion(request, cx);
            async move {
                let mapper = OpenAiEventMapper::new();
                let completions = mapper.map_stream(completions.await?).boxed();
                if let Some((http_client, api_url, Some(api_key))) = local_metrics {
                    Ok(stream_with_local_inference_progress(
                        completions,
                        http_client,
                        api_url,
                        api_key,
                    ))
                } else {
                    Ok(completions)
                }
            }
            .boxed()
        } else {
            disable_response_thinking_for_none_effort(&mut request, &self.model);
            let request = match into_open_ai_response(
                request,
                &self.model.name,
                self.model.capabilities.parallel_tool_calls,
                self.model.capabilities.prompt_cache_key,
                self.max_output_tokens(),
                default_thinking_reasoning_effort(&self.model),
                self.model.reasoning_effort.is_some(),
                &self.provider_id,
            ) {
                Ok(request) => request,
                Err(error) => return async move { Err(error.into()) }.boxed(),
            };
            let completions = self.stream_response(request, cx);
            let compaction_state_owner = self.provider_id.clone();
            async move {
                let mapper = OpenAiResponseEventMapper::new(compaction_state_owner);
                Ok(mapper.map_stream(completions.await?).boxed())
            }
            .boxed()
        }
    }
}

type LocalModelPicker = Picker<LocalModelPickerDelegate>;

#[derive(Clone)]
struct LocalModelPickerEntry {
    id: String,
    name: SharedString,
    details: SharedString,
}

struct LocalModelPickerDelegate {
    models: Vec<LocalModelPickerEntry>,
    filtered_models: Vec<StringMatch>,
    selected_index: usize,
    on_selected: Arc<dyn Fn(String, &mut Window, &mut App) + 'static>,
}

impl LocalModelPickerDelegate {
    fn new(
        models: Vec<LocalModelPickerEntry>,
        selected_id: Option<&str>,
        on_selected: impl Fn(String, &mut Window, &mut App) + 'static,
    ) -> Self {
        let selected_index = selected_id
            .and_then(|id| models.iter().position(|model| model.id == id))
            .unwrap_or(0);
        let filtered_models = models
            .iter()
            .enumerate()
            .map(|(index, model)| StringMatch {
                candidate_id: index,
                string: model.name.to_string(),
                positions: Vec::new(),
                score: 0.0,
            })
            .collect();
        Self {
            models,
            filtered_models,
            selected_index,
            on_selected: Arc::new(on_selected),
        }
    }
}

impl PickerDelegate for LocalModelPickerDelegate {
    type ListItem = AnyElement;

    fn name() -> &'static str {
        "LocalModelPicker"
    }

    fn match_count(&self) -> usize {
        self.filtered_models.len()
    }

    fn selected_index(&self) -> usize {
        self.selected_index
    }

    fn set_selected_index(
        &mut self,
        index: usize,
        _: &mut Window,
        cx: &mut Context<LocalModelPicker>,
    ) {
        self.selected_index = index.min(self.filtered_models.len().saturating_sub(1));
        cx.notify();
    }

    fn placeholder_text(&self, _: &mut Window, _: &mut App) -> Arc<str> {
        "Search local models...".into()
    }

    fn update_matches(
        &mut self,
        query: String,
        _: &mut Window,
        cx: &mut Context<LocalModelPicker>,
    ) -> Task<()> {
        let query = query.to_lowercase();
        self.filtered_models = self
            .models
            .iter()
            .enumerate()
            .filter(|(_, model)| {
                query.is_empty()
                    || model.name.to_lowercase().contains(&query)
                    || model.details.to_lowercase().contains(&query)
            })
            .map(|(index, model)| StringMatch {
                candidate_id: index,
                string: model.name.to_string(),
                positions: Vec::new(),
                score: 0.0,
            })
            .collect();
        self.selected_index = 0;
        cx.notify();
        Task::ready(())
    }

    fn confirm(&mut self, _: bool, window: &mut Window, cx: &mut Context<LocalModelPicker>) {
        let Some(model_match) = self.filtered_models.get(self.selected_index) else {
            return;
        };
        let Some(model) = self.models.get(model_match.candidate_id) else {
            return;
        };
        (self.on_selected)(model.id.clone(), window, cx);
        cx.emit(DismissEvent);
    }

    fn dismissed(&mut self, window: &mut Window, cx: &mut Context<LocalModelPicker>) {
        cx.defer_in(window, |picker, window, cx| {
            picker.set_query("", window, cx);
        });
        cx.emit(DismissEvent);
    }

    fn render_match(
        &self,
        index: usize,
        selected: bool,
        _: &mut Window,
        _: &mut Context<LocalModelPicker>,
    ) -> Option<Self::ListItem> {
        let model_match = self.filtered_models.get(index)?;
        let model = self.models.get(model_match.candidate_id)?;
        Some(
            ListItem::new(index)
                .inset(true)
                .spacing(ListItemSpacing::Sparse)
                .toggle_state(selected)
                .child(
                    v_flex()
                        .min_w_0()
                        .gap_1()
                        .child(Label::new(model.name.clone()))
                        .child(
                            Label::new(model.details.clone())
                                .size(LabelSize::Small)
                                .color(Color::Muted),
                        ),
                )
                .into_any_element(),
        )
    }
}

struct ConfigurationView {
    api_key_editor: Entity<InputField>,
    custom_name_editor: Entity<InputField>,
    custom_url_editor: Entity<InputField>,
    custom_sha_editor: Entity<InputField>,
    pending_custom_model: Option<LocalModelSpec>,
    selected_local_model_id: Option<String>,
    custom_error: Option<String>,
    pending_runtime: Option<LocalRuntimeConfig>,
    state: Entity<State>,
    load_credentials_task: Option<Task<()>>,
}

impl ConfigurationView {
    fn new(state: Entity<State>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let api_key_editor = cx.new(|cx| {
            InputField::new(
                window,
                cx,
                "000000000000000000000000000000000000000000000000000",
            )
        });
        let custom_name_editor =
            cx.new(|cx| InputField::new(window, cx, "My local model").label("Model name"));
        let custom_url_editor = cx.new(|cx| {
            InputField::new(window, cx, "https://.../model.gguf").label("GGUF HTTPS URL")
        });
        let custom_sha_editor = cx.new(|cx| {
            InputField::new(window, cx, "Optional 64-character SHA-256").label("Expected SHA-256")
        });

        cx.observe(&state, |_, _, cx| {
            cx.notify();
        })
        .detach();

        let load_credentials_task = state.read(cx).requires_api_key.then(|| {
            cx.spawn_in(window, {
                let state = state.clone();
                async move |this, cx| {
                    if let Some(task) = Some(state.update(cx, |state, cx| state.authenticate(cx))) {
                        // We don't log an error, because "not signed in" is also an error.
                        let _ = task.await;
                    }
                    this.update(cx, |this, cx| {
                        this.load_credentials_task = None;
                        cx.notify();
                    })
                    .log_err();
                }
            })
        });

        Self {
            api_key_editor,
            custom_name_editor,
            custom_url_editor,
            custom_sha_editor,
            pending_custom_model: None,
            selected_local_model_id: None,
            custom_error: None,
            pending_runtime: state
                .read(cx)
                .local_models
                .as_ref()
                .map(|manager| manager.runtime.clone()),
            state,
            load_credentials_task,
        }
    }

    fn save_api_key(&mut self, _: &menu::Confirm, window: &mut Window, cx: &mut Context<Self>) {
        let api_key = self.api_key_editor.read(cx).text(cx).trim().to_string();
        if api_key.is_empty() {
            return;
        }

        // url changes can cause the editor to be displayed again
        self.api_key_editor
            .update(cx, |input, cx| input.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(Some(api_key), cx))
                .await
        })
        .detach_and_log_err(cx);
    }

    fn reset_api_key(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.api_key_editor
            .update(cx, |input, cx| input.set_text("", window, cx));

        let state = self.state.clone();
        cx.spawn_in(window, async move |_, cx| {
            state
                .update(cx, |state, cx| state.set_api_key(None, cx))
                .await
        })
        .detach_and_log_err(cx);
    }

    fn install_local_model_by_id(
        &mut self,
        model_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let task = self
            .state
            .update(cx, |state, cx| state.install_local_model(model_id, cx));
        cx.spawn_in(window, async move |_, _| task.await)
            .detach_and_log_err(cx);
    }

    fn update_pending_runtime(
        &mut self,
        update: impl FnOnce(&mut LocalRuntimeConfig),
        cx: &mut Context<Self>,
    ) {
        let runtime = self.pending_runtime.get_or_insert_with(|| {
            self.state
                .read(cx)
                .local_models
                .as_ref()
                .map(|manager| manager.runtime.clone())
                .unwrap_or_default()
        });
        update(runtime);
        cx.notify();
    }

    fn apply_pending_runtime(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(runtime) = self.pending_runtime.clone() else {
            return;
        };
        let task = self
            .state
            .update(cx, |state, cx| state.apply_local_runtime(runtime, cx));
        cx.spawn_in(window, async move |_, _| task.await)
            .detach_and_log_err(cx);
    }

    fn review_custom_url(&mut self, _: &gpui::ClickEvent, _: &mut Window, cx: &mut Context<Self>) {
        let name = self.custom_name_editor.read(cx).text(cx).trim().to_string();
        let url = self.custom_url_editor.read(cx).text(cx).trim().to_string();
        let sha = self
            .custom_sha_editor
            .read(cx)
            .text(cx)
            .trim()
            .to_ascii_lowercase();
        self.custom_error = None;
        let result = (|| -> Result<LocalModelSpec> {
            if name.is_empty() {
                bail!("Enter a model name");
            }
            if !url.starts_with("https://") {
                bail!("The model URL must use HTTPS");
            }
            let file_name = url
                .split('?')
                .next()
                .and_then(|url| url.rsplit('/').next())
                .context("The URL has no filename")?;
            if !file_name.to_ascii_lowercase().ends_with(".gguf") {
                bail!("The URL must point to a .gguf file");
            }
            if !sha.is_empty()
                && (sha.len() != 64 || !sha.bytes().all(|byte| byte.is_ascii_hexdigit()))
            {
                bail!("SHA-256 must contain exactly 64 hexadecimal characters");
            }
            let id = format!("custom-{}", sanitize_model_id(&name));
            Ok(LocalModelSpec {
                id,
                display_name: name,
                file_name: format!("custom-{}", file_name),
                download_url: Some(url),
                import_path: None,
                download_size: 0,
                sha256: (!sha.is_empty()).then_some(sha),
                context_tokens: 4_096,
                output_tokens: 2_048,
                source_label: "Custom HTTPS GGUF".into(),
                built_in: false,
            })
        })();
        match result {
            Ok(spec) => self.pending_custom_model = Some(spec),
            Err(error) => self.custom_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn choose_custom_file(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let receiver = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Import GGUF model".into()),
        });
        let this = cx.entity().downgrade();
        window
            .spawn(cx, async move |cx| {
                let paths = match receiver.await {
                    Ok(Ok(Some(paths))) => paths,
                    _ => return Ok::<(), anyhow::Error>(()),
                };
                let Some(path) = paths.into_iter().next() else {
                    return Ok::<(), anyhow::Error>(());
                };
                this.update(cx, |this, cx| {
                    let file_name = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .unwrap_or("imported.gguf")
                        .to_string();
                    if !file_name.to_ascii_lowercase().ends_with(".gguf") {
                        this.custom_error = Some("Select a .gguf file".into());
                    } else {
                        let display_name = path
                            .file_stem()
                            .and_then(|name| name.to_str())
                            .unwrap_or("Imported model")
                            .to_string();
                        this.pending_custom_model = Some(LocalModelSpec {
                            id: format!("custom-{}", sanitize_model_id(&display_name)),
                            display_name,
                            file_name: format!("custom-{file_name}"),
                            download_url: None,
                            import_path: Some(path.clone()),
                            download_size: path
                                .metadata()
                                .map(|metadata| metadata.len())
                                .unwrap_or(0),
                            sha256: None,
                            context_tokens: 4_096,
                            output_tokens: 2_048,
                            source_label: "Imported GGUF file".into(),
                            built_in: false,
                        });
                        this.custom_error = None;
                    }
                    cx.notify();
                })?;
                Ok::<(), anyhow::Error>(())
            })
            .detach_and_log_err(cx);
    }

    fn confirm_custom_model(
        &mut self,
        _: &gpui::ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(spec) = self.pending_custom_model.take() else {
            return;
        };
        match self
            .state
            .update(cx, |state, cx| state.add_custom_model(spec, cx))
        {
            Ok(model_id) => self.install_local_model_by_id(model_id, window, cx),
            Err(error) => self.custom_error = Some(format!("{error:#}")),
        }
        cx.notify();
    }

    fn request_delete_model(
        &mut self,
        model_id: String,
        display_name: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = window.prompt(
            PromptLevel::Critical,
            &format!("Delete {display_name}?"),
            Some(
                "Warning: this permanently removes the downloaded GGUF model from this device. This cannot be undone.",
            ),
            &["Delete model", "Cancel"],
            cx,
        );
        let state = self.state.clone();
        cx.spawn_in(window, async move |this, cx| {
            if prompt.await? != 0 {
                return anyhow::Ok(());
            }
            if let Err(error) =
                state.update(cx, |state, cx| state.delete_local_model(&model_id, cx))
            {
                this.update(cx, |this, cx| {
                    this.custom_error = Some(format!("Could not delete model: {error:#}"));
                    cx.notify();
                })?;
            }
            anyhow::Ok(())
        })
        .detach_and_log_err(cx);
    }

    fn render_local_model_card(
        &self,
        model: &LocalModelCardData,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let spec = model.spec.clone();
        let status = model.status.clone();
        let is_busy = matches!(
            status,
            LocalModelStatus::Downloading { .. } | LocalModelStatus::Installing
        );
        let action_label = match status {
            LocalModelStatus::Ready {
                server_running: true,
            } => "Running",
            LocalModelStatus::Ready {
                server_running: false,
            } => "Start",
            LocalModelStatus::Error(_) => "Retry",
            LocalModelStatus::Downloaded => "Install",
            _ => "Download",
        };
        let status_element = match &status {
            LocalModelStatus::NotDownloaded => Label::new("Not downloaded")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            LocalModelStatus::Downloaded => Label::new("Downloaded - runtime not installed")
                .size(LabelSize::Small)
                .color(Color::Muted)
                .into_any_element(),
            LocalModelStatus::Downloading { downloaded, total } => {
                let percent = if *total == 0 {
                    0.0
                } else {
                    (*downloaded as f32 / *total as f32 * 100.0).clamp(0.0, 100.0)
                };
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(
                        Label::new(if *total == 0 {
                            format!(
                                "Downloading {:.2} GB (remote size unknown)",
                                *downloaded as f64 / 1e9
                            )
                        } else {
                            format!(
                                "Downloading {:.1}% ({:.2} / {:.2} GB)",
                                percent,
                                *downloaded as f64 / 1e9,
                                *total as f64 / 1e9
                            )
                        })
                        .size(LabelSize::Small),
                    )
                    .when(*total > 0, |this| {
                        this.child(ProgressBar::new(
                            format!("progress-{}", spec.id),
                            percent,
                            100.0,
                            cx,
                        ))
                    })
                    .into_any_element()
            }
            LocalModelStatus::Installing => Label::new("Installing and verifying...")
                .size(LabelSize::Small)
                .color(Color::Accent)
                .into_any_element(),
            LocalModelStatus::Ready { server_running } => Label::new(if *server_running {
                "Ready - Vulkan/CPU server running"
            } else {
                "Downloaded - ready to start"
            })
            .size(LabelSize::Small)
            .color(Color::Success)
            .into_any_element(),
            LocalModelStatus::Error(error) => Label::new(format!("Setup failed: {error}"))
                .size(LabelSize::Small)
                .color(Color::Error)
                .into_any_element(),
        };
        let model_id = spec.id.clone();
        let delete_model_id = spec.id.clone();
        let delete_display_name = spec.display_name.clone();
        let can_delete = model.can_delete;

        v_flex()
            .w_full()
            .gap_2()
            .p_3()
            .rounded_sm()
            .border_1()
            .border_color(cx.theme().colors().border_variant)
            .child(Label::new(spec.display_name.clone()))
            .child(
                Label::new(format!(
                    "{:.2} GB | {} | {} | {}",
                    spec.download_size as f64 / 1e9,
                    spec.source_label,
                    if spec.built_in {
                        "max 7B mobile catalog"
                    } else {
                        "custom model"
                    },
                    mobile_model_guidance(&spec),
                ))
                .size(LabelSize::Small)
                .color(Color::Muted),
            )
            .child(status_element)
            .child(
                h_flex()
                    .w_full()
                    .flex_wrap()
                    .gap_2()
                    .child(
                        Button::new(format!("local-model-{}", spec.id), action_label)
                            .style(ButtonStyle::Outlined)
                            .disabled(
                                is_busy
                                    || matches!(
                                        status,
                                        LocalModelStatus::Ready {
                                            server_running: true
                                        }
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.install_local_model_by_id(model_id.clone(), window, cx)
                            })),
                    )
                    .when(can_delete && !is_busy, |this| {
                        this.child(
                            Button::new(format!("delete-local-model-{}", spec.id), "Delete")
                                .style(ButtonStyle::Tinted(ui::TintColor::Error))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.request_delete_model(
                                        delete_model_id.clone(),
                                        delete_display_name.clone(),
                                        window,
                                        cx,
                                    );
                                })),
                        )
                    }),
            )
            .into_any_element()
    }

    fn should_render_editor(&self, cx: &Context<Self>) -> bool {
        !self.state.read(cx).is_authenticated()
    }
}

impl Render for ConfigurationView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let requires_api_key = self.state.read(cx).requires_api_key;
        if !requires_api_key {
            let (applied_runtime, models, api_url) = {
                let state = self.state.read(cx);
                let Some(manager) = state.local_models.as_ref() else {
                    return div().into_any();
                };
                let models = manager
                    .models
                    .iter()
                    .map(|model| LocalModelCardData {
                        spec: model.spec.clone(),
                        status: model.status.clone(),
                        can_delete: model.model_path.is_file()
                            || model.model_path.with_extension("gguf.part").is_file()
                            || !model.spec.built_in,
                    })
                    .collect::<Vec<_>>();
                (
                    manager.runtime.clone(),
                    models,
                    state.settings.api_url.clone(),
                )
            };
            let runtime = self
                .pending_runtime
                .clone()
                .unwrap_or_else(|| applied_runtime.clone());
            let runtime_changed = runtime != applied_runtime;
            let downloaded_models = models
                .iter()
                .filter(|model| !matches!(model.status, LocalModelStatus::NotDownloaded))
                .collect::<Vec<_>>();
            let available_models = models
                .iter()
                .filter(|model| matches!(model.status, LocalModelStatus::NotDownloaded))
                .collect::<Vec<_>>();
            let selected_available_model = self
                .selected_local_model_id
                .as_deref()
                .and_then(|selected_id| {
                    available_models
                        .iter()
                        .find(|model| model.spec.id == selected_id)
                        .copied()
                })
                .or_else(|| available_models.first().copied());
            let picker_entries = available_models
                .iter()
                .map(|model| LocalModelPickerEntry {
                    id: model.spec.id.clone(),
                    name: model.spec.display_name.clone().into(),
                    details: format!(
                        "{:.2} GB | {} | {}",
                        model.spec.download_size as f64 / 1e9,
                        model.spec.source_label,
                        mobile_model_guidance(&model.spec),
                    )
                    .into(),
                })
                .collect::<Vec<_>>();
            let selected_picker_label = selected_available_model
                .map(|model| model.spec.display_name.clone())
                .unwrap_or_else(|| "No more catalog models".into());
            let selected_picker_id = selected_available_model.map(|model| model.spec.id.clone());
            let model_picker = (!picker_entries.is_empty()).then(|| {
                let this = cx.entity().downgrade();
                PopoverMenu::new("local-model-picker")
                    .trigger(
                        Button::new("local-model-picker-trigger", selected_picker_label)
                            .style(ButtonStyle::Outlined)
                            .end_icon(
                                Icon::new(IconName::ChevronUpDown)
                                    .size(IconSize::Small)
                                    .color(Color::Muted),
                            ),
                    )
                    .menu(move |window, cx| {
                        let picker_entries = picker_entries.clone();
                        let selected_picker_id = selected_picker_id.clone();
                        let this = this.clone();
                        Some(cx.new(|cx| {
                            let delegate = LocalModelPickerDelegate::new(
                                picker_entries,
                                selected_picker_id.as_deref(),
                                move |model_id, _, cx| {
                                    this.update(cx, |this, cx| {
                                        this.selected_local_model_id = Some(model_id);
                                        cx.notify();
                                    })
                                    .ok();
                                },
                            );
                            Picker::uniform_list(delegate, window, cx)
                                .show_scrollbar(true)
                                .initial_width(rems(22.))
                                .max_height(rems(22.))
                        }))
                    })
                    .anchor(gpui::Anchor::TopLeft)
                    .offset(gpui::Point {
                        x: px(0.0),
                        y: px(2.0),
                    })
                    .with_handle(PopoverMenuHandle::default())
            });
            let downloaded_model_cards = downloaded_models
                .into_iter()
                .map(|model| self.render_local_model_card(model, cx))
                .collect::<Vec<_>>();
            let selected_available_card =
                selected_available_model.map(|model| self.render_local_model_card(model, cx));

            let pending = self.pending_custom_model.clone().map(|spec| {
                v_flex()
                    .w_full()
                    .gap_2()
                    .p_3()
                    .rounded_sm()
                    .border_1()
                    .border_color(cx.theme().colors().border_focused)
                    .child(Label::new("Confirm custom GGUF"))
                    .child(
                        Label::new(format!(
                            "{} | {} | {:.2} GB",
                            spec.display_name,
                            spec.source_label,
                            spec.download_size as f64 / 1e9
                        ))
                        .size(LabelSize::Small),
                    )
                    .child(
                        Label::new(spec.download_url.clone().unwrap_or_else(|| {
                            spec.import_path
                                .as_ref()
                                .map(|path| path.display().to_string())
                                .unwrap_or_default()
                        }))
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        Label::new(if spec.sha256.is_some() {
                            "Expected SHA-256 supplied; it must match."
                        } else {
                            "SHA-256 will be calculated after GGUF validation."
                        })
                        .size(LabelSize::Small)
                        .color(Color::Muted),
                    )
                    .child(
                        h_flex()
                            .flex_wrap()
                            .gap_2()
                            .child(
                                Button::new("confirm-custom-gguf", "Confirm and install")
                                    .style(ButtonStyle::Filled)
                                    .on_click(cx.listener(Self::confirm_custom_model)),
                            )
                            .child(
                                Button::new("cancel-custom-gguf", "Cancel")
                                    .style(ButtonStyle::Outlined)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.pending_custom_model = None;
                                        cx.notify();
                                    })),
                            ),
                    )
            });
            return v_flex().w_full().gap_3()
                .child(Label::new("Local LLM runtime"))
                .child(Label::new("For full Zed Agent tools, 1B-1.5B models are recommended on phones. Larger models may take several minutes before the first response under memory or thermal pressure.").size(LabelSize::Small).color(Color::Muted))
                .child(v_flex().w_full().gap_2().p_3().rounded_sm().border_1().border_color(cx.theme().colors().border_variant)
                    .child(h_flex().flex_wrap().gap_2()
                        .child(Label::new(format!("CPU cores: {}", if runtime.threads == 0 { "Auto".into() } else { runtime.threads.to_string() })))
                        .child(Button::new("cpu-auto", "Auto").style(ButtonStyle::Outlined).on_click(cx.listener(|this, _, _, cx| this.update_pending_runtime(|config| config.threads = 0, cx))))
                        .child(Button::new("cpu-minus", "-").style(ButtonStyle::Outlined).on_click(cx.listener(|this, _, _, cx| this.update_pending_runtime(|config| config.threads = config.threads.max(2) - 1, cx))))
                        .child(Button::new("cpu-plus", "+").style(ButtonStyle::Outlined).on_click(cx.listener(|this, _, _, cx| this.update_pending_runtime(|config| config.threads = (config.threads.max(1) + 1).min(16), cx)))))
                    .child(h_flex().flex_wrap().gap_2().child(Label::new(format!("Context: {}K", runtime.context_tokens / 1024))).children([4096_u32, 8192].into_iter().map(|value| {
                        Button::new(format!("context-{value}"), format!("{}K", value / 1024)).style(ButtonStyle::Outlined).on_click(cx.listener(move |this, _, _, cx| this.update_pending_runtime(|config| config.context_tokens = value, cx)))
                    })))
                    .child(Label::new("4K is the minimum for Zed Agent prompts and tools. Use 8K for larger project context if your device has enough memory.").size(LabelSize::Small).color(Color::Muted))
                    .child(h_flex().flex_wrap().gap_2().child(Label::new(format!("Batch: {}", runtime.batch_size))).children([128_u32, 256, 512].into_iter().map(|value| {
                        Button::new(format!("batch-{value}"), value.to_string()).style(ButtonStyle::Outlined).on_click(cx.listener(move |this, _, _, cx| this.update_pending_runtime(|config| config.batch_size = value, cx)))
                    })))
                    .child(h_flex().flex_wrap().gap_2().child(Label::new(format!("Maximum response: {} tokens", runtime.output_tokens))).children([512_u32, 1024, 2048, 4096].into_iter().map(|value| {
                        Button::new(format!("output-{value}"), value.to_string()).style(ButtonStyle::Outlined).on_click(cx.listener(move |this, _, _, cx| this.update_pending_runtime(|config| config.output_tokens = value, cx)))
                    })))
                    .child(Button::new("toggle-vulkan", if runtime.vulkan { "Vulkan: Auto + CPU fallback" } else { "Vulkan: Off (CPU)" }).style(ButtonStyle::Outlined).on_click(cx.listener(|this, _, _, cx| this.update_pending_runtime(|config| config.vulkan = !config.vulkan, cx))))
                    .child(Button::new("apply-local-runtime", "Apply").style(ButtonStyle::Filled).disabled(!runtime_changed).on_click(cx.listener(Self::apply_pending_runtime)))
                    .child(Label::new(if runtime_changed { "Apply to save these settings. A running model will restart automatically." } else { "Runtime settings are applied." }).size(LabelSize::Small).color(Color::Muted)))
                .when(!downloaded_model_cards.is_empty(), |this| {
                    this.child(Label::new("Downloaded models"))
                        .children(downloaded_model_cards)
                })
                .child(Label::new("Browse models"))
                .child(Label::new("Qwen Coder is recommended for code editing and tool use. Other families are useful for local chat and experimentation, but agent tool-call quality varies by model.").size(LabelSize::Small).color(Color::Muted))
                .when_some(model_picker, |this, picker| this.child(picker))
                .when_some(selected_available_card, |this, card| this.child(card))
                .child(Label::new("Add other local GGUF"))
                .child(v_flex().w_full().gap_2().p_3().rounded_sm().border_1().border_color(cx.theme().colors().border_variant)
                    .child(Label::new("Only import GGUF files from a source you trust. Models are data, but malformed files can still target bugs in the native model parser.").size(LabelSize::Small).color(Color::Muted))
                    .child(self.custom_name_editor.clone()).child(self.custom_url_editor.clone()).child(self.custom_sha_editor.clone())
                    .child(h_flex().flex_wrap().gap_2()
                        .child(Button::new("review-custom-url", "Review URL").style(ButtonStyle::Outlined).on_click(cx.listener(Self::review_custom_url)))
                        .child(Button::new("import-custom-gguf", "Import GGUF file").style(ButtonStyle::Outlined).on_click(cx.listener(Self::choose_custom_file))))
                    .when_some(self.custom_error.clone(), |this, error| this.child(Label::new(error).size(LabelSize::Small).color(Color::Error))))
                .when_some(pending, |this, pending| this.child(pending))
                .child(Label::new(format!("No API key required. Endpoint: {api_url}")).size(LabelSize::Small).color(Color::Muted))
                .into_any();
        }
        let state = self.state.read(cx);
        let env_var_set = state.api_key_state.is_from_env_var();
        let env_var_name = state.api_key_state.env_var_name();

        let api_key_section = if self.should_render_editor(cx) {
            v_flex()
                .on_action(cx.listener(Self::save_api_key))
                .child(Label::new("To use Zed's agent with an OpenAI-compatible provider, you need to add an API key."))
                .child(
                    div()
                        .pt(DynamicSpacing::Base04.rems(cx))
                        .child(self.api_key_editor.clone())
                )
                .child(
                    Label::new(
                        format!("You can also set the {env_var_name} environment variable and restart Zed."),
                    )
                    .size(LabelSize::Small).color(Color::Muted),
                )
                .into_any()
        } else {
            h_flex()
                .mt_1()
                .p_1()
                .justify_between()
                .rounded_md()
                .border_1()
                .border_color(cx.theme().colors().border)
                .bg(cx.theme().colors().background)
                .child(
                    h_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_1()
                        .child(Icon::new(IconName::Check).color(Color::Success))
                        .child(
                            div()
                                .w_full()
                                .overflow_x_hidden()
                                .text_ellipsis()
                                .child(Label::new(
                                    if env_var_set {
                                        format!("API key set in {env_var_name} environment variable")
                                    } else {
                                        format!("API key configured for {}", &state.settings.api_url)
                                    }
                                ))
                        ),
                )
                .child(
                    h_flex()
                        .flex_shrink_0()
                        .child(
                            Button::new("reset-api-key", "Reset API Key")
                                .label_size(LabelSize::Small)
                                .start_icon(Icon::new(IconName::Undo).size(IconSize::Small))
                                .layer(ElevationIndex::ModalSurface)
                                .when(env_var_set, |this| {
                                    this.tooltip(Tooltip::text(format!("To reset your API key, unset the {env_var_name} environment variable.")))
                                })
                                .on_click(cx.listener(|this, _, window, cx| this.reset_api_key(window, cx))),
                        ),
                )
                .into_any()
        };

        if self.load_credentials_task.is_some() {
            div().child(Label::new("Loading credentials…")).into_any()
        } else {
            v_flex().size_full().child(api_key_section).into_any()
        }
    }
}

#[cfg(test)]
mod local_model_tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn built_in_catalog_is_safe_and_unique() {
        let catalog = local_model_catalog();
        let mut ids = HashSet::new();
        let mut files = HashSet::new();
        assert_eq!(catalog.len(), 9);
        for model in catalog {
            validate_local_model_spec(&model).unwrap();
            assert!(model.built_in);
            assert!(model.download_size > 0 && model.download_size <= 6_000_000_000);
            assert!(ids.insert(model.id));
            assert!(files.insert(model.file_name));
        }
    }

    #[test]
    fn custom_model_ids_cannot_escape_the_model_directory() {
        assert_eq!(sanitize_model_id("My Model 3B"), "my-model-3-b");
        let mut model = local_model_catalog().remove(0);
        model.file_name = "../../outside.gguf".into();
        assert!(validate_local_model_spec(&model).is_err());
    }

    #[test]
    fn local_runtime_is_normalized_for_agent_use() {
        let runtime = LocalRuntimeConfig {
            context_tokens: 2_048,
            output_tokens: 8_192,
            ..Default::default()
        }
        .normalized();

        assert_eq!(runtime.context_tokens, MIN_AGENT_CONTEXT_TOKENS);
        assert_eq!(runtime.output_tokens, MIN_AGENT_CONTEXT_TOKENS);
    }

    #[test]
    fn mobile_guidance_distinguishes_model_cost() {
        let catalog = local_model_catalog();
        let qwen_1_5b = catalog
            .iter()
            .find(|model| model.id == "qwen2.5-coder-1.5b-q4-k-m")
            .unwrap();
        let gemma_4b = catalog
            .iter()
            .find(|model| model.id == "gemma-3-4b-it-q4-k-m")
            .unwrap();

        assert_eq!(
            mobile_model_guidance(qwen_1_5b),
            "Recommended for mobile Agent use"
        );
        assert_eq!(
            mobile_model_guidance(gemma_4b),
            "Experimental on phones; high heat and long first response"
        );
    }

    #[test]
    fn parses_live_llama_slot_counters() {
        let slots: Vec<LocalSlotState> = serde_json::from_str(
            r#"[{
                "n_ctx": 4096,
                "n_prompt_tokens": 1305,
                "n_prompt_tokens_processed": 1024,
                "n_prompt_tokens_cache": 256,
                "is_processing": true,
                "next_token": { "n_decoded": 25 }
            }]"#,
        )
        .unwrap();
        let slot = &slots[0];
        assert_eq!(slot.n_ctx, 4096);
        assert_eq!(slot.n_prompt_tokens_processed, 1024);
        assert_eq!(slot.n_prompt_tokens_cache, 256);
        assert_eq!(slot.next_token.n_decoded, 25);
    }
}
