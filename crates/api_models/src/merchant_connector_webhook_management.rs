use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The scope of webhook registration.
/// Determines which entities the connector should register webhooks for.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(tag = "type", content = "values", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Scope {
    /// Connector does not scope webhooks to any specific entity.
    /// Single registration call.
    NotSpecific,

    /// Scoped by payment method types (e.g., Pix, Boleto).
    PaymentMethodTypes(Vec<common_enums::PaymentMethodType>),

    /// Scoped by event types (e.g., Payments, Refunds, Disputes).
    EventTypes(Vec<common_enums::EventType>),
}

/// Discriminator for the scope type in the response.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ScopeType {
    NotSpecific,
    PaymentMethodType,
    EventType,
}

/// Identifies a single scope entry.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(untagged)]
pub enum ScopeIdentifier {
    NotSpecific,
    PaymentMethodType(common_enums::PaymentMethodType),
    EventType(common_enums::EventType),
}

/// Result of registering a webhook for a single scope identifier.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WebhookRegistrationResult {
    /// The scope identifier this result corresponds to.
    pub identifier: ScopeIdentifier,

    /// Whether the registration succeeded or failed.
    pub status: common_enums::WebhookRegistrationStatus,

    /// The connector-generated webhook ID, if successful.
    pub connector_webhook_id: Option<String>,

    /// Error details, if the registration failed.
    pub error: Option<WebhookRegistrationError>,
}

/// Error details for a failed webhook registration.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct WebhookRegistrationError {
    pub code: String,
    pub message: String,
}

/// Register a webhook at the connector
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectorWebhookRegisterRequest {
    #[schema(value_type = Option<Scope>)]
    pub scope: Scope,
    #[schema(value_type = Option<ConnectorWebhookEventType>)]
    pub event_type: common_enums::ConnectorWebhookEventType,
}

/// Response for registering connector webhooks.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct RegisterConnectorWebhookResponse {
    /// The type of scope used for this registration.
    pub scope_type: ScopeType,

    /// List of identifiers that were requested to be registered.
    pub requested: Vec<ScopeIdentifier>,

    /// Per-identifier registration results.
    pub results: Vec<WebhookRegistrationResult>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectorWebhookListResponse {
    pub connector: String,
    pub webhooks: Vec<ConnectorWebhookResponse>,
}

#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectorWebhookResponse {
    #[schema(value_type = Option<ConnectorWebhookEventType>)]
    pub event_type: common_enums::ConnectorWebhookEventType,
    pub connector_webhook_id: String,
}
