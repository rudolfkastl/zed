mod model;
pub mod provider;
mod rate_limiter;
mod registry;
mod request;
mod role;
pub mod settings;

use anyhow::Result;
use client::Client;
use futures::{future::BoxFuture, stream::BoxStream};
use gpui::{AnyView, AppContext, AsyncAppContext, FocusHandle, SharedString, Task, WindowContext};
pub use model::*;
use project::Fs;
pub(crate) use rate_limiter::*;
pub use registry::*;
pub use request::*;
pub use role::*;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use std::{future::Future, sync::Arc};

pub fn init(client: Arc<Client>, fs: Arc<dyn Fs>, cx: &mut AppContext) {
    settings::init(fs, cx);
    registry::init(client, cx);
}

pub trait LanguageModel: Send + Sync {
    fn id(&self) -> LanguageModelId;
    fn name(&self) -> LanguageModelName;
    fn provider_id(&self) -> LanguageModelProviderId;
    fn provider_name(&self) -> LanguageModelProviderName;
    fn telemetry_id(&self) -> String;

    fn max_token_count(&self) -> usize;

    fn count_tokens(
        &self,
        request: LanguageModelRequest,
        cx: &AppContext,
    ) -> BoxFuture<'static, Result<usize>>;

    fn stream_completion(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncAppContext,
    ) -> BoxFuture<'static, Result<BoxStream<'static, Result<String>>>>;

    fn use_any_tool(
        &self,
        request: LanguageModelRequest,
        name: String,
        description: String,
        schema: serde_json::Value,
        cx: &AsyncAppContext,
    ) -> BoxFuture<'static, Result<serde_json::Value>>;
}

impl dyn LanguageModel {
    pub fn use_tool<T: LanguageModelTool>(
        &self,
        request: LanguageModelRequest,
        cx: &AsyncAppContext,
    ) -> impl 'static + Future<Output = Result<T>> {
        let schema = schemars::schema_for!(T);
        let schema_json = serde_json::to_value(&schema).unwrap();
        let request = self.use_any_tool(request, T::name(), T::description(), schema_json, cx);
        async move {
            let response = request.await?;
            Ok(serde_json::from_value(response)?)
        }
    }
}

pub trait LanguageModelTool: 'static + DeserializeOwned + JsonSchema {
    fn name() -> String;
    fn description() -> String;
}

pub trait LanguageModelProvider: 'static {
    fn id(&self) -> LanguageModelProviderId;
    fn name(&self) -> LanguageModelProviderName;
    fn provided_models(&self, cx: &AppContext) -> Vec<Arc<dyn LanguageModel>>;
    fn load_model(&self, _model: Arc<dyn LanguageModel>, _cx: &AppContext) {}
    fn is_authenticated(&self, cx: &AppContext) -> bool;
    fn authenticate(&self, cx: &mut AppContext) -> Task<Result<()>>;
    fn configuration_view(&self, cx: &mut WindowContext) -> (AnyView, Option<FocusHandle>);
    fn reset_credentials(&self, cx: &mut AppContext) -> Task<Result<()>>;
}

pub trait LanguageModelProviderState: 'static {
    type ObservableEntity;

    fn observable_entity(&self) -> Option<gpui::Model<Self::ObservableEntity>>;

    fn subscribe<T: 'static>(
        &self,
        cx: &mut gpui::ModelContext<T>,
        callback: impl Fn(&mut T, &mut gpui::ModelContext<T>) + 'static,
    ) -> Option<gpui::Subscription> {
        let entity = self.observable_entity()?;
        Some(cx.observe(&entity, move |this, _, cx| {
            callback(this, cx);
        }))
    }
}

#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct LanguageModelId(pub SharedString);

#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct LanguageModelName(pub SharedString);

#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct LanguageModelProviderId(pub SharedString);

#[derive(Clone, Eq, PartialEq, Hash, Debug, Ord, PartialOrd)]
pub struct LanguageModelProviderName(pub SharedString);

impl From<String> for LanguageModelId {
    fn from(value: String) -> Self {
        Self(SharedString::from(value))
    }
}

impl From<String> for LanguageModelName {
    fn from(value: String) -> Self {
        Self(SharedString::from(value))
    }
}

impl From<String> for LanguageModelProviderId {
    fn from(value: String) -> Self {
        Self(SharedString::from(value))
    }
}

impl From<String> for LanguageModelProviderName {
    fn from(value: String) -> Self {
        Self(SharedString::from(value))
    }
}
