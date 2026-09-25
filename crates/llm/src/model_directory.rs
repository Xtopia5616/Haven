//! Model client and primary-route directory for `LlmRouter`.
//!
//! The directory owns adapters and the model identities selected by request
//! policies. `LlmRouter` remains the sole owner of its `RouterConfig` snapshot
//! and all request execution state; metadata lookups borrow that snapshot
//! instead of keeping a second configuration copy.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex as StdMutex};

use haven_common::config::{
    Capability, ModelEndpoint, RequestKind, RoutedModel, RouterConfig, endpoint_credentials_ready,
};
use haven_common::media::CapabilityProfile;

use crate::adapters::adapter_for;
use crate::client::LlmClient;
use crate::request_descriptor::RequestDescriptor;
use crate::types::LlmError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RouteMode {
    Production,
    InjectedClients,
}

#[derive(Clone)]
struct PrimaryRoute {
    required_capability: Capability,
    model_id: String,
}

/// The clients and configured primary identities used by one router.
pub(crate) struct ModelDirectory {
    clients: HashMap<String, Arc<dyn LlmClient>>,
    primary_routes: StdMutex<HashMap<RequestKind, PrimaryRoute>>,
}

impl ModelDirectory {
    /// Build one provider client per materialized model and keep only
    /// credential-ready, capability-compatible primary routes.
    pub(crate) fn from_config(config: &RouterConfig) -> Self {
        let clients = config
            .models
            .iter()
            .map(|model| {
                (
                    model.id.clone(),
                    Arc::from(adapter_for(&model.endpoint)) as Arc<dyn LlmClient>,
                )
            })
            .collect();
        Self::from_clients(config, clients, RouteMode::Production)
    }

    /// Build a directory with explicit clients. Injected clients are used by
    /// test constructors that intentionally have no provider credentials;
    /// their routes still require the configured model's declared capability.
    pub(crate) fn with_injected_clients(
        config: &RouterConfig,
        clients: impl IntoIterator<Item = (String, Arc<dyn LlmClient>)>,
    ) -> Self {
        Self::from_clients(
            config,
            clients.into_iter().collect(),
            RouteMode::InjectedClients,
        )
    }

    fn from_clients(
        config: &RouterConfig,
        clients: HashMap<String, Arc<dyn LlmClient>>,
        route_mode: RouteMode,
    ) -> Self {
        Self {
            clients,
            primary_routes: StdMutex::new(Self::build_primary_routes(config, route_mode)),
        }
    }

    fn build_primary_routes(
        config: &RouterConfig,
        route_mode: RouteMode,
    ) -> HashMap<RequestKind, PrimaryRoute> {
        config
            .request_policies
            .iter()
            .filter_map(|policy| {
                let descriptor = RequestDescriptor::from(policy.request);
                let model_id = policy.primary.trim();
                let model = config.model(model_id)?;
                let supports_request = model.capabilities.contains(&descriptor.required_capability);
                let credentials_ready = match route_mode {
                    RouteMode::Production => endpoint_credentials_ready(&model.endpoint),
                    RouteMode::InjectedClients => true,
                };
                (supports_request && credentials_ready).then_some((
                    descriptor.purpose,
                    PrimaryRoute {
                        required_capability: descriptor.required_capability,
                        model_id: model_id.to_string(),
                    },
                ))
            })
            .collect()
    }

    /// Rebuild request-to-primary identities after a test-only config mutation.
    pub(crate) fn rebuild_primary_routes(&self, config: &RouterConfig, mode: RouteMode) {
        *self.primary_routes.lock().unwrap() = Self::build_primary_routes(config, mode);
    }

    /// Select the configured client, preserving the historical default
    /// adapter fallback used by the non-executing selection helper.
    pub(crate) fn select_client(&self, request: RequestKind) -> Arc<dyn LlmClient> {
        self.client_for_route(request)
            .unwrap_or_else(|| Arc::from(adapter_for(&ModelEndpoint::default())))
    }

    /// Resolve the single configured primary for an executing request.
    pub(crate) fn resolve_client(
        &self,
        descriptor: RequestDescriptor,
    ) -> Result<(String, Arc<dyn LlmClient>), LlmError> {
        let route = self
            .primary_routes
            .lock()
            .unwrap()
            .get(&descriptor.purpose)
            .cloned()
            .filter(|route| route.required_capability == descriptor.required_capability)
            .ok_or_else(|| {
                LlmError::Configuration(format!(
                    "no configured model for {}",
                    descriptor.purpose.as_str()
                ))
            })?;
        let model_id = route.model_id;
        let client = self.clients.get(&model_id).cloned().ok_or_else(|| {
            LlmError::Configuration(format!("model client is unavailable: {model_id}"))
        })?;
        Ok((model_id, client))
    }

    /// Look up an adapter by the model identity already resolved for a request.
    pub(crate) fn client_for_model_id(
        &self,
        model_id: &str,
    ) -> Result<Arc<dyn LlmClient>, LlmError> {
        self.clients.get(model_id).cloned().ok_or_else(|| {
            LlmError::Configuration(format!("model client is unavailable: {model_id}"))
        })
    }

    fn client_for_route(&self, request: RequestKind) -> Option<Arc<dyn LlmClient>> {
        let model_id = self.primary_model_id(request)?;
        self.clients.get(&model_id).cloned()
    }

    pub(crate) fn primary_model_id(&self, request: RequestKind) -> Option<String> {
        self.primary_routes
            .lock()
            .unwrap()
            .get(&request)
            .map(|route| route.model_id.clone())
    }

    /// Read the configured model for metadata paths. This deliberately uses
    /// `RouterConfig::route`, retaining its exact credential and capability
    /// checks independently of the normalized dispatch route table.
    pub(crate) fn configured_model<'a>(
        &self,
        config: &'a RouterConfig,
        request: RequestKind,
    ) -> Option<&'a RoutedModel> {
        config.route(request)
    }

    pub(crate) fn endpoint_for_request<'a>(
        &self,
        config: &'a RouterConfig,
        request: RequestKind,
    ) -> Option<&'a ModelEndpoint> {
        self.configured_model(config, request)
            .map(|model| &model.endpoint)
    }

    pub(crate) fn context_window_for_request(
        &self,
        config: &RouterConfig,
        request: RequestKind,
    ) -> Option<u32> {
        self.endpoint_for_request(config, request)
            .and_then(crate::registry::context_window_for)
    }

    pub(crate) fn is_request_configured(
        &self,
        config: &RouterConfig,
        request: RequestKind,
    ) -> bool {
        self.configured_model(config, request).is_some()
    }

    /// Return each configured model once, preserving request-policy order.
    /// This is the route selection used by Router pre-warming.
    pub(crate) fn configured_requests(&self, config: &RouterConfig) -> Vec<RequestKind> {
        let mut seen_models = HashSet::new();
        config
            .request_policies
            .iter()
            .filter_map(|policy| {
                let model = self.configured_model(config, policy.request)?;
                seen_models
                    .insert(model.id.clone())
                    .then_some(policy.request)
            })
            .collect()
    }

    pub(crate) fn capability_profile_for_request(&self, request: RequestKind) -> CapabilityProfile {
        self.select_client(request).capability_profile()
    }

    pub(crate) fn model_ids(&self) -> impl Iterator<Item = &str> {
        self.clients.keys().map(String::as_str)
    }

    /// Clamp materialized output caps to each model's context window before
    /// adapter construction. The config snapshot remains owned by the router.
    pub(crate) fn clamp_max_tokens_to_context_windows(
        config: &mut RouterConfig,
        fallback_context_window: u32,
    ) {
        for model in &mut config.models {
            let window = crate::registry::context_window_for(&model.endpoint)
                .unwrap_or(fallback_context_window);
            if window > 0 {
                model.endpoint.max_tokens = model.endpoint.max_tokens.min(window);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use haven_common::config::Capability;
    use haven_common::config::RequestPolicy;

    fn model(id: &str, capabilities: Vec<Capability>, api_key: &str) -> RoutedModel {
        RoutedModel {
            id: id.into(),
            endpoint: ModelEndpoint {
                api_key: api_key.into(),
                context_window: Some(8_192),
                max_tokens: 4_096,
                ..Default::default()
            },
            capabilities,
        }
    }

    fn policy(request: RequestKind, primary: &str) -> RequestPolicy {
        RequestPolicy {
            request,
            primary: primary.into(),
        }
    }

    fn config(models: Vec<RoutedModel>, request_policies: Vec<RequestPolicy>) -> RouterConfig {
        RouterConfig {
            models,
            request_policies,
            ..Default::default()
        }
    }

    fn injected_client() -> Arc<dyn LlmClient> {
        Arc::from(adapter_for(&ModelEndpoint::default()))
    }

    #[test]
    fn production_routes_require_ready_credentials_and_required_capability() {
        let config = config(
            vec![
                model("ready-chat", vec![Capability::Chat], "sk-ready"),
                model("keyless-vision", vec![Capability::Vision], ""),
                model("wrong-capability", vec![Capability::FastChat], "sk-ready"),
            ],
            vec![
                policy(RequestKind::Chat, "ready-chat"),
                policy(RequestKind::Vision, "keyless-vision"),
                policy(RequestKind::Transcription, "wrong-capability"),
            ],
        );
        let directory = ModelDirectory::from_config(&config);

        assert_eq!(
            directory.primary_model_id(RequestKind::Chat).as_deref(),
            Some("ready-chat")
        );
        assert_eq!(directory.primary_model_id(RequestKind::Vision), None);
        assert_eq!(directory.primary_model_id(RequestKind::Transcription), None);
    }

    #[test]
    fn injected_routes_skip_credentials_but_still_require_capability() {
        let config = config(
            vec![
                model("keyless-chat", vec![Capability::Chat], ""),
                model("wrong-capability", vec![Capability::Chat], ""),
            ],
            vec![
                policy(RequestKind::Chat, "keyless-chat"),
                policy(RequestKind::Vision, "wrong-capability"),
            ],
        );
        let directory = ModelDirectory::with_injected_clients(
            &config,
            [
                ("keyless-chat".into(), injected_client()),
                ("wrong-capability".into(), injected_client()),
            ],
        );

        assert_eq!(
            directory.primary_model_id(RequestKind::Chat).as_deref(),
            Some("keyless-chat")
        );
        assert_eq!(directory.primary_model_id(RequestKind::Vision), None);
        assert!(matches!(
            directory.resolve_client(RequestDescriptor::from(RequestKind::Vision)),
            Err(LlmError::Configuration(message)) if message == "no configured model for vision"
        ));
    }

    #[test]
    fn execution_route_rejects_a_descriptor_with_the_wrong_capability() {
        let config = config(
            vec![model("chat", vec![Capability::Chat], "")],
            vec![policy(RequestKind::Chat, "chat")],
        );
        let directory =
            ModelDirectory::with_injected_clients(&config, [("chat".into(), injected_client())]);
        let mismatched_descriptor = RequestDescriptor {
            purpose: RequestKind::Chat,
            required_capability: Capability::FastChat,
        };

        assert!(matches!(
            directory.resolve_client(mismatched_descriptor),
            Err(LlmError::Configuration(message)) if message == "no configured model for chat"
        ));
    }

    #[test]
    fn injected_audio_chat_and_transcription_routes_require_their_own_capabilities() {
        let config = config(
            vec![
                model("audio-input", vec![Capability::AudioInput], ""),
                model("transcription", vec![Capability::Transcription], ""),
            ],
            vec![
                policy(RequestKind::AudioChat, "audio-input"),
                policy(RequestKind::Transcription, "audio-input"),
            ],
        );
        let directory = ModelDirectory::with_injected_clients(
            &config,
            [
                ("audio-input".into(), injected_client()),
                ("transcription".into(), injected_client()),
            ],
        );

        assert_eq!(
            directory
                .primary_model_id(RequestKind::AudioChat)
                .as_deref(),
            Some("audio-input")
        );
        assert_eq!(directory.primary_model_id(RequestKind::Transcription), None);
    }

    #[test]
    fn missing_primary_reports_no_route_and_keeps_unknown_request_unrouted() {
        let config = config(
            vec![model("chat", vec![Capability::Chat], "")],
            vec![policy(RequestKind::Chat, "chat")],
        );
        let directory =
            ModelDirectory::with_injected_clients(&config, [("chat".into(), injected_client())]);

        assert_eq!(directory.primary_model_id(RequestKind::Vision), None);
        assert!(matches!(
            directory.resolve_client(RequestDescriptor::from(RequestKind::Vision)),
            Err(LlmError::Configuration(message)) if message == "no configured model for vision"
        ));
    }

    #[test]
    fn request_kinds_sharing_a_primary_resolve_the_same_model_identity_and_client() {
        let config = config(
            vec![model(
                "shared",
                vec![Capability::Chat, Capability::Vision],
                "",
            )],
            vec![
                policy(RequestKind::Chat, "shared"),
                policy(RequestKind::Vision, "shared"),
            ],
        );
        let shared_client = injected_client();
        let directory = ModelDirectory::with_injected_clients(
            &config,
            [("shared".into(), shared_client.clone())],
        );
        let (chat_id, chat_client) = directory
            .resolve_client(RequestDescriptor::from(RequestKind::Chat))
            .unwrap();
        let (vision_id, vision_client) = directory
            .resolve_client(RequestDescriptor::from(RequestKind::Vision))
            .unwrap();

        assert_eq!(chat_id, "shared");
        assert_eq!(vision_id, chat_id);
        assert!(Arc::ptr_eq(&chat_client, &vision_client));
    }

    #[test]
    fn endpoint_and_context_window_metadata_follow_the_configured_primary() {
        let config = config(
            vec![model("chat", vec![Capability::Chat], "sk-ready")],
            vec![policy(RequestKind::Chat, "chat")],
        );
        let directory = ModelDirectory::from_config(&config);
        let endpoint = directory
            .endpoint_for_request(&config, RequestKind::Chat)
            .expect("configured endpoint metadata");

        assert_eq!(endpoint.context_window, Some(8_192));
        assert_eq!(
            directory.context_window_for_request(&config, RequestKind::Chat),
            Some(8_192)
        );
        assert_eq!(
            directory.configured_requests(&config),
            vec![RequestKind::Chat]
        );
        assert_eq!(
            directory.context_window_for_request(&config, RequestKind::Vision),
            None
        );
    }
}
