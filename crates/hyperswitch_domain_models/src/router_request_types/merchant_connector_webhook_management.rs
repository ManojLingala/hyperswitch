#[derive(Debug, Clone)]
pub struct ConnectorWebhookRegisterRequest {
    /// The scope of this webhook registration.
    pub scope: api_models::merchant_connector_webhook_management::ScopeIdentifier,
    /// The base webhook URL to register.
    pub webhook_url: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ConnectorWebhookData {
    pub event_type: common_enums::ConnectorWebhookEventType,
}
