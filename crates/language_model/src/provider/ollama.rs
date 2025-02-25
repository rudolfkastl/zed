use anyhow::{anyhow, Result};
use futures::{future::BoxFuture, stream::BoxStream, FutureExt, StreamExt};
use gpui::{AnyView, AppContext, AsyncAppContext, FocusHandle, ModelContext, Subscription, Task};
use http_client::HttpClient;
use ollama::{
    get_models, preload_model, stream_chat_completion, ChatMessage, ChatOptions, ChatRequest,
};
use settings::{Settings, SettingsStore};
use std::{future, sync::Arc, time::Duration};
use ui::{prelude::*, ButtonLike, ElevationIndex, Indicator};

use crate::{
    settings::AllLanguageModelSettings, LanguageModel, LanguageModelId, LanguageModelName,
    LanguageModelProvider, LanguageModelProviderId, LanguageModelProviderName,
    LanguageModelProviderState, LanguageModelRequest, RateLimiter, Role,
};

const OLLAMA_DOWNLOAD_URL: &str = "https://ollama.com/download";
const OLLAMA_LIBRARY_URL: &str = "https://ollama.com/library";

const PROVIDER_ID: &str = "ollama";
const PROVIDER_NAME: &str = "Ollama";

#[derive(Default, Debug, Clone, PartialEq)]
pub struct OllamaSettings {
    pub api_url: String,
    pub low_speed_timeout: Option<Duration>,
}

pub struct OllamaLanguageModelProvider {
    http_client: Arc<dyn HttpClient>,
    state: gpui::Model<State>,
}

pub struct State {
    http_client: Arc<dyn HttpClient>,
    available_models: Vec<ollama::Model>,
    _subscription: Subscription,
}

impl State {
    fn is_authenticated(&self) -> bool {
        !self.available_models.is_empty()
    }

    fn fetch_models(&mut self, cx: &mut ModelContext<Self>) -> Task<Result<()>> {
        let settings = &AllLanguageModelSettings::get_global(cx).ollama;
        let http_client = self.http_client.clone();
        let api_url = settings.api_url.clone();

        // As a proxy for the server being "authenticated", we'll check if its up by fetching the models
        cx.spawn(|this, mut cx| async move {
            let models = get_models(http_client.as_ref(), &api_url, None).await?;

            let mut models: Vec<ollama::Model> = models
                .into_iter()
                // Since there is no metadata from the Ollama API
                // indicating which models are embedding models,
                // simply filter out models with "-embed" in their name
                .filter(|model| !model.name.contains("-embed"))
                .map(|model| ollama::Model::new(&model.name))
                .collect();

            models.sort_by(|a, b| a.name.cmp(&b.name));

            this.update(&mut cx, |this, cx| {
                this.available_models = models;
                cx.notify();
            })
        })
    }
}

impl OllamaLanguageModelProvider {
    pub fn new(http_client: Arc<dyn HttpClient>, cx: &mut AppContext) -> Self {
        let this = Self {
            http_client: http_client.clone(),
            state: cx.new_model(|cx| State {
                http_client,
                available_models: Default::default(),
                _subscription: cx.observe_global::<SettingsStore>(|this: &mut State, cx| {
                    this.fetch_models(cx).detach();
                    cx.notify();
                }),
            }),
        };
        this.state
            .update(cx, |state, cx| state.fetch_models(cx).detach());
        this
    }
}

impl LanguageModelProviderState for OllamaLanguageModelProvider {
    type ObservableEntity = State;

    fn observable_entity(&self) -> Option<gpui::Model<Self::ObservableEntity>> {
        Some(self.state.clone())
    }
}

impl LanguageModelProvider for OllamaLanguageModelProvider {
    fn id(&self) -> LanguageModelProviderId {
        LanguageModelProviderId(PROVIDER_ID.into())
    }

    fn name(&self) -> LanguageModelProviderName {
        LanguageModelProviderName(PROVIDER_NAME.into())
    }

    fn provided_models(&self, cx: &AppContext) -> Vec<Arc<dyn LanguageModel>> {
        self.state
            .read(cx)
            .available_models
            .iter()
            .map(|model| {
                Arc::new(OllamaLanguageModel {
                    id: LanguageModelId::from(model.name.clone()),
                    model: model.clone(),
                    http_client: self.http_client.clone(),
                    request_limiter: RateLimiter::new(4),
                }) as Arc<dyn LanguageModel>
            })
            .collect()
    }

    fn load_model(&self, model: Arc<dyn LanguageModel>, cx: &AppContext) {
        let settings = &AllLanguageModelSettings::get_global(cx).ollama;
        let http_client = self.http_client.clone();
        let api_url = settings.api_url.clone();
        let id = model.id().0.to_string();
        cx.spawn(|_| async move { preload_model(http_client, &api_url, &id).await })
            .detach_and_log_err(cx);
    }

    fn is_authenticated(&self, cx: &AppContext) -> bool {
        self.state.read(cx).is_authenticated()
    }

    fn authenticate(&self, cx: &mut AppContext) -> Task<Result<()>> {
        if self.is_authenticated(cx) {
            Task::ready(Ok(()))
        } else {
            self.state.update(cx, |state, cx| state.fetch_models(cx))
        }
    }

    fn configuration_view(&self, cx: &mut WindowContext) -> (AnyView, Option<FocusHandle>) {
        let state = self.state.clone();
        (
            cx.new_view(|cx| ConfigurationView::new(state, cx)).into(),
            None,
        )
    }

    fn reset_credentials(&self, cx: &mut AppContext) -> Task<Result<()>> {
        self.state.update(cx, |state, cx| state.fetch_models(cx))
    }
}

pub struct OllamaLanguageModel {
    id: LanguageModelId,
    model: ollama::Model,
    http_client: Arc<dyn HttpClient>,
    request_limiter: RateLimiter,
}

impl OllamaLanguageModel {
    fn to_ollama_request(&self, request: LanguageModelRequest) -> ChatRequest {
        ChatRequest {
            model: self.model.name.clone(),
            messages: request
                .messages
                .into_iter()
                .map(|msg| match msg.role {
                    Role::User => ChatMessage::User {
                        content: msg.content,
                    },
                    Role::Assistant => ChatMessage::Assistant {
                        content: msg.content,
                    },
                    Role::System => ChatMessage::System {
                        content: msg.content,
                    },
                })
                .collect(),
            keep_alive: self.model.keep_alive.clone().unwrap_or_default(),
            stream: true,
            options: Some(ChatOptions {
                num_ctx: Some(self.model.max_tokens),
                stop: Some(request.stop),
                temperature: Some(request.temperature),
                ..Default::default()
            }),
        }
    }
}

impl LanguageModel for OllamaLanguageModel {
    fn id(&self) -> LanguageModelId {
        self.id.clone()
    }

    fn name(&self) -> LanguageModelName {
        LanguageModelName::from(self.model.display_name().to_string())
    }

    fn provider_id(&self) -> LanguageModelProviderId {
        LanguageModelProviderId(PROVIDER_ID.into())
    }

    fn provider_name(&self) -> LanguageModelProviderName {
        LanguageModelProviderName(PROVIDER_NAME.into())
    }

    fn telemetry_id(&self) -> String {
        format!("ollama/{}", self.model.id())
    }

    fn max_token_count(&self) -> usize {
        self.model.max_token_count()
    }

    fn count_tokens(
        &self,
        request: LanguageModelRequest,
        _cx: &AppContext,
    ) -> BoxFuture<'static, Result<usize>> {
        // There is no endpoint for this _yet_ in Ollama
        // see: https://github.com/ollama/ollama/issues/1716 and https://github.com/ollama/ollama/issues/3582
        let token_count = request
            .messages
            .iter()
            .map(|msg| msg.content.chars().count())
            .sum::<usize>()
            / 4;

        async move { Ok(token_count) }.boxed()
    }

    fn stream_completion(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncAppContext,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<String>>>> {
        let request = self.to_ollama_request(request);

        let http_client = self.http_client.clone();
        let Ok((api_url, low_speed_timeout)) = cx.update(|cx| {
            let settings = &AllLanguageModelSettings::get_global(cx).ollama;
            (settings.api_url.clone(), settings.low_speed_timeout)
        }) else {
            return futures::future::ready(Err(anyhow!("App state dropped"))).boxed();
        };

        let future = self.request_limiter.stream(async move {
            let response =
                stream_chat_completion(http_client.as_ref(), &api_url, request, low_speed_timeout)
                    .await?;
            let stream = response
                .filter_map(|response| async move {
                    match response {
                        Ok(delta) => {
                            let content = match delta.message {
                                ChatMessage::User { content } => content,
                                ChatMessage::Assistant { content } => content,
                                ChatMessage::System { content } => content,
                            };
                            Some(Ok(content))
                        }
                        Err(error) => Some(Err(error)),
                    }
                })
                .boxed();
            Ok(stream)
        });

        async move { Ok(future.await?.boxed()) }.boxed()
    }

    fn use_any_tool(
        &self,
        _request: LanguageModelRequest,
        _name: String,
        _description: String,
        _schema: serde_json::Value,
        _cx: &AsyncAppContext,
    ) -> BoxFuture<'static, Result<serde_json::Value>> {
        future::ready(Err(anyhow!("not implemented"))).boxed()
    }
}

struct ConfigurationView {
    state: gpui::Model<State>,
}

impl ConfigurationView {
    pub fn new(state: gpui::Model<State>, _cx: &mut ViewContext<Self>) -> Self {
        Self { state }
    }

    fn retry_connection(&self, cx: &mut WindowContext) {
        self.state
            .update(cx, |state, cx| state.fetch_models(cx))
            .detach_and_log_err(cx);
    }

    fn render_download_button(&self, _cx: &mut ViewContext<Self>) -> impl IntoElement {
        ButtonLike::new("download_ollama_button")
            .style(ButtonStyle::Filled)
            .size(ButtonSize::Large)
            .layer(ElevationIndex::ModalSurface)
            .child(Label::new("Get Ollama"))
            .on_click(move |_, cx| cx.open_url(OLLAMA_DOWNLOAD_URL))
    }

    fn render_retry_button(&self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        ButtonLike::new("retry_ollama_models")
            .style(ButtonStyle::Filled)
            .size(ButtonSize::Large)
            .layer(ElevationIndex::ModalSurface)
            .child(Label::new("Retry"))
            .on_click(cx.listener(move |this, _, cx| this.retry_connection(cx)))
    }

    fn render_next_steps(&self, _cx: &mut ViewContext<Self>) -> impl IntoElement {
        v_flex()
            .p_4()
            .size_full()
            .gap_2()
            .child(
                Label::new("Once Ollama is on your machine, make sure to download a model or two.")
                    .size(LabelSize::Large),
            )
            .child(
                h_flex().w_full().p_4().justify_center().gap_2().child(
                    ButtonLike::new("view-models")
                        .style(ButtonStyle::Filled)
                        .size(ButtonSize::Large)
                        .layer(ElevationIndex::ModalSurface)
                        .child(Label::new("View Available Models"))
                        .on_click(move |_, cx| cx.open_url(OLLAMA_LIBRARY_URL)),
                ),
            )
    }
}

impl Render for ConfigurationView {
    fn render(&mut self, cx: &mut ViewContext<Self>) -> impl IntoElement {
        let is_authenticated = self.state.read(cx).is_authenticated();

        if is_authenticated {
            v_flex()
                .size_full()
                .child(
                    h_flex()
                        .gap_2()
                        .child(Indicator::dot().color(Color::Success))
                        .child(Label::new("Ollama configured").size(LabelSize::Small)),
                )
                .into_any()
        } else {
            v_flex()
            .size_full()
            .gap_2()
            .child(Label::new("To use Ollama models via the assistant, Ollama must be running on your machine with at least one model downloaded.").size(LabelSize::Large))
            .child(
                h_flex()
                    .w_full()
                    .p_4()
                    .justify_center()
                    .gap_2()
                    .child(
                        self.render_download_button(cx)
                    )
                    .child(
                        self.render_retry_button(cx)
                    )
            )
            .child(self.render_next_steps(cx))
            .into_any()
        }
    }
}
