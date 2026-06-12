# Webhook Registration Redesign Plan

## Overview

Redesign the connector webhook registration flow to support:
- **Payment-method-type-specific** registrations (e.g., Santander)
- **Event-type-specific** registrations (e.g., Payload)
- **Non-specific** single-call registrations (e.g., Adyen)

The design must be **extensible** for future scope types without breaking existing code.

---

## 1. Scope Enum Design

### API Models (`crates/api_models/src/merchant_connector_webhook_management.rs`)

Use a **tagged union** with `#[non_exhaustive]` for forward compatibility:

```rust
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
```

### Serialization Examples

**NotSpecific (Adyen-like):**
```json
{
  "type": "not_specific"
}
```

**PaymentMethodTypes (Santander-like):**
```json
{
  "type": "payment_method_types",
  "values": ["pix", "boleto", "pix_automatico_push"]
}
```

**EventTypes (Payload-like):**
```json
{
  "type": "event_types",
  "values": ["payment_succeeded", "refund_completed", "dispute_opened"]
}
```

### Extensibility

Adding a new scope type (e.g., `RefundTypes`) only requires adding a new variant:

```rust
pub enum Scope {
    // ... existing variants ...
    RefundTypes(Vec<common_enums::RefundType>),
}
```

Clients consuming the API will not break because:
1. `#[non_exhaustive]` prevents exhaustive matching in downstream crates
2. Tagged unions ensure unknown variants deserialize gracefully

---

## 2. Unified Response Structure

### Problem with the Original Design

The original proposal had nullable top-level fields (`payment_method_types`, `event_types`), which leads to:
- Unclear schema contracts
- Hard to extend for new scope types
- Consumers must handle nullability for every field

### Proposed Response Structure

A **single flat array** with a `scope_type` discriminator:

```rust
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
```

### Response Examples

#### Santander (PaymentMethodType-scoped)
```json
{
  "scope_type": "payment_method_type",
  "requested": [
    {"payment_method_type": "pix"},
    {"payment_method_type": "boleto"},
    {"payment_method_type": "pix_automatico_push"}
  ],
  "results": [
    {
      "identifier": {"payment_method_type": "pix"},
      "status": "success",
      "connector_webhook_id": "wh_pix_001"
    },
    {
      "identifier": {"payment_method_type": "boleto"},
      "status": "success",
      "connector_webhook_id": "wh_boleto_001"
    },
    {
      "identifier": {"payment_method_type": "pix_automatico_push"},
      "status": "success",
      "connector_webhook_id": "wh_pix_push_001"
    }
  ]
}
```

#### Payload (EventType-scoped)
```json
{
  "scope_type": "event_type",
  "requested": [
    {"event_type": "payment_succeeded"},
    {"event_type": "refund_completed"},
    {"event_type": "dispute_opened"}
  ],
  "results": [
    {
      "identifier": {"event_type": "payment_succeeded"},
      "status": "success",
      "connector_webhook_id": "evt_001"
    },
    {
      "identifier": {"event_type": "refund_completed"},
      "status": "success",
      "connector_webhook_id": "evt_002"
    },
    {
      "identifier": {"event_type": "dispute_opened"},
      "status": "failure",
      "error": {
        "code": "EVT_UNSUPPORTED",
        "message": "Dispute events not supported on this merchant account"
      }
    }
  ]
}
```

#### Adyen (NotSpecific)
```json
{
  "scope_type": "not_specific",
  "requested": ["not_specific"],
  "results": [
    {
      "identifier": "not_specific",
      "status": "success",
      "connector_webhook_id": "adyen_wh_001"
    }
  ]
}
```

### Benefits

| Aspect | Original | Proposed |
|--------|----------|----------|
| Nullability | Multiple nullable fields | Zero nullable fields |
| Extensibility | Requires new top-level fields | Add `ScopeType` variant only |
| Clarity | Consumer must check which field is present | Single `scope_type` discriminator |
| Per-item errors | Nested within category objects | Flat array, easy to iterate |

---

## 3. ConnectorSpecifications Trait Extension

### Trait Definition

Add a new method to the `ConnectorSpecifications` trait:

```rust
// crates/hyperswitch_interfaces/src/api.rs

use api_models::merchant_connector_webhook_management::{Scope, ScopeIdentifier};

pub trait ConnectorSpecifications {
    // ... existing methods ...

    /// Returns the webhook registration plan for this connector.
    ///
    /// Given the requested scope and the payment methods enabled for this
    /// merchant connector account, returns a list of `(identifier, webhook_url)`
    /// tuples. Each tuple corresponds to one connector integration call.
    ///
    /// # Examples
    ///
    /// **Santander** with `Scope::PaymentMethodTypes([Pix, Boleto])`:
    /// ```ignore
    /// vec![
    ///     (ScopeIdentifier::PaymentMethodType(Pix),          "https://.../pix/webhook"),
    ///     (ScopeIdentifier::PaymentMethodType(Boleto),       "https://.../boleto/webhook"),
    ///     (ScopeIdentifier::PaymentMethodType(PixAutoPush),  "https://.../push/webhook"),
    ///     (ScopeIdentifier::PaymentMethodType(PixAutoPush),  "https://.../push/alternate"),
    /// ]
    /// ```
    ///
    /// **Payload** with `Scope::EventTypes([Payment, Refund, Dispute])`:
    /// ```ignore
    /// vec![
    ///     (ScopeIdentifier::EventType(Payment), "https://.../events/payments"),
    ///     (ScopeIdentifier::EventType(Refund),  "https://.../events/refunds"),
    ///     (ScopeIdentifier::EventType(Dispute), "https://.../events/disputes"),
    /// ]
    /// ```
    ///
    /// **Adyen** with `Scope::NotSpecific`:
    /// ```ignore
    /// vec![
    ///     (ScopeIdentifier::NotSpecific, "https://.../webhooks"),
    /// ]
    /// ```
    fn get_webhook_registration_plan(
        &self,
        scope: &Scope,
        payment_methods_enabled: &[common_enums::PaymentMethodType],
    ) -> Vec<(ScopeIdentifier, String)>;
}
```

### Default Implementation

Provide a sensible default to avoid breaking existing connectors:

```rust
fn get_webhook_registration_plan(
    &self,
    _scope: &Scope,
    _payment_methods_enabled: &[common_enums::PaymentMethodType],
) -> Vec<(ScopeIdentifier, String)> {
    // Default: no plan. Connectors that support auto-configuration
    // must override this.
    Vec::new()
}
```

### Connector-Specific Implementations

#### Santander
```rust
impl ConnectorSpecifications for Santander {
    fn get_webhook_registration_plan(
        &self,
        scope: &Scope,
        payment_methods_enabled: &[common_enums::PaymentMethodType],
    ) -> Vec<(ScopeIdentifier, String)> {
        match scope {
            Scope::PaymentMethodTypes(requested_pmts) => {
                requested_pmts
                    .iter()
                    .flat_map(|pmt| match pmt {
                        PaymentMethodType::Pix => vec![(
                            ScopeIdentifier::PaymentMethodType(*pmt),
                            "https://apis.santander.com.br/webhooks/pix".to_string(),
                        )],
                        PaymentMethodType::Boleto => vec![(
                            ScopeIdentifier::PaymentMethodType(*pmt),
                            "https://apis.santander.com.br/webhooks/boleto".to_string(),
                        )],
                        PaymentMethodType::PixAutomaticoPush => vec![
                            (
                                ScopeIdentifier::PaymentMethodType(*pmt),
                                "https://apis.santander.com.br/webhooks/pix-push".to_string(),
                            ),
                            (
                                ScopeIdentifier::PaymentMethodType(*pmt),
                                "https://apis.santander.com.br/webhooks/pix-push-alt".to_string(),
                            ),
                        ],
                        PaymentMethodType::PixAutomaticoQr => vec![
                            (
                                ScopeIdentifier::PaymentMethodType(*pmt),
                                "https://apis.santander.com.br/webhooks/pix-qr-1".to_string(),
                            ),
                            (
                                ScopeIdentifier::PaymentMethodType(*pmt),
                                "https://apis.santander.com.br/webhooks/pix-qr-2".to_string(),
                            ),
                            (
                                ScopeIdentifier::PaymentMethodType(*pmt),
                                "https://apis.santander.com.br/webhooks/pix-qr-3".to_string(),
                            ),
                        ],
                        _ => Vec::new(),
                    })
                    .collect()
            }
            _ => Vec::new(), // Santander only supports PMT-scoped registration
        }
    }
}
```

#### Payload
```rust
impl ConnectorSpecifications for Payload {
    fn get_webhook_registration_plan(
        &self,
        scope: &Scope,
        _payment_methods_enabled: &[common_enums::PaymentMethodType],
    ) -> Vec<(ScopeIdentifier, String)> {
        match scope {
            Scope::EventTypes(requested_events) => requested_events
                .iter()
                .map(|evt| {
                    let url = format!("https://api.payload.com/v1/webhooks/{}", evt);
                    (ScopeIdentifier::EventType(*evt), url)
                })
                .collect(),
            _ => Vec::new(), // Payload only supports EventType-scoped registration
        }
    }
}
```

#### Adyen
```rust
impl ConnectorSpecifications for Adyen {
    fn get_webhook_registration_plan(
        &self,
        scope: &Scope,
        _payment_methods_enabled: &[common_enums::PaymentMethodType],
    ) -> Vec<(ScopeIdentifier, String)> {
        match scope {
            Scope::NotSpecific => vec![(
                ScopeIdentifier::NotSpecific,
                "https://management-test.adyen.com/v3/merchants/{merchantId}/webhooks".to_string(),
            )],
            _ => Vec::new(), // Adyen does not support scoped registration
        }
    }
}
```

---

## 4. Core Loop Logic

### High-Level Flow

```rust
// crates/router/src/core/merchant_connector_webhook_management.rs

pub async fn register_connector_webhook(
    state: SessionState,
    merchant_id: &id_type::MerchantId,
    profile_id: Option<id_type::ProfileId>,
    merchant_connector_id: &id_type::MerchantConnectorAccountId,
    req: api_models::merchant_connector_webhook_management::ConnectorWebhookRegisterRequest,
) -> RouterResponse<api_models::merchant_connector_webhook_management::RegisterConnectorWebhookResponse> {
    // 1. Fetch merchant connector account
    let mca = db.find_merchant_connector_account(...).await?;

    // 2. Get connector data
    let connector_data = api::ConnectorData::get_connector_by_name(...)?;

    // 3. Validate request against connector capabilities
    configure_connector_webhook_flow::validate_webhook_registration_request(
        &connector_data,
        req.clone(),
    ).await?;

    // 4. Build registration plan from connector specifications
    let enabled_payment_methods = mca.get_enabled_payment_method_types();
    let registration_plan = connector_data
        .connector
        .get_webhook_registration_plan(&req.scope, &enabled_payment_methods);

    // 5. Execute connector integration for each planned registration
    let mut results = Vec::new();
    for (identifier, webhook_url) in registration_plan {
        let scoped_request = ConnectorWebhookRegisterRequest {
            scope: derive_single_scope(&identifier),
            webhook_url,
            // ... other fields from `req` ...
        };

        let router_data = construct_webhook_register_router_data(
            &state,
            &mca,
            scoped_request,
        ).await?;

        let connector_integration = connector_data.connector.get_connector_integration();

        let response = services::execute_connector_processing_step(
            &state,
            connector_integration,
            &router_data,
            common_enums::CallConnectorAction::Trigger,
            None,
            None,
        )
        .await
        .to_webhook_configuration_failed_response()?;

        let result = match response.response {
            Ok(success) => WebhookRegistrationResult {
                identifier: identifier.clone(),
                status: success.status,
                connector_webhook_id: success.connector_webhook_id,
                error: None,
            },
            Err(err) => WebhookRegistrationResult {
                identifier: identifier.clone(),
                status: common_enums::WebhookRegistrationStatus::Failure,
                connector_webhook_id: None,
                error: Some(WebhookRegistrationError {
                    code: err.code,
                    message: err.message,
                }),
            },
        };

        results.push(result);

        // 6. Update DB with registration details
        update_connector_webhook_registration_details(&db, &mca, &result).await?;
    }

    // 7. Construct final response
    let response = RegisterConnectorWebhookResponse {
        scope_type: determine_scope_type(&req.scope),
        requested: extract_requested_identifiers(&req.scope),
        results,
    };

    Ok(service_api::ApplicationResponse::Json(response))
}
```

### Helper Functions

```rust
/// Derives a single-item Scope from a ScopeIdentifier for individual connector calls.
fn derive_single_scope(identifier: &ScopeIdentifier) -> Scope {
    match identifier {
        ScopeIdentifier::NotSpecific => Scope::NotSpecific,
        ScopeIdentifier::PaymentMethodType(pmt) => Scope::PaymentMethodTypes(vec![*pmt]),
        ScopeIdentifier::EventType(evt) => Scope::EventTypes(vec![*evt]),
    }
}

/// Maps the request Scope to the response ScopeType.
fn determine_scope_type(scope: &Scope) -> ScopeType {
    match scope {
        Scope::NotSpecific => ScopeType::NotSpecific,
        Scope::PaymentMethodTypes(_) => ScopeType::PaymentMethodType,
        Scope::EventTypes(_) => ScopeType::EventType,
    }
}

/// Extracts all requested identifiers from the Scope for the response.
fn extract_requested_identifiers(scope: &Scope) -> Vec<ScopeIdentifier> {
    match scope {
        Scope::NotSpecific => vec![ScopeIdentifier::NotSpecific],
        Scope::PaymentMethodTypes(pmts) => pmts
            .iter()
            .map(|pmt| ScopeIdentifier::PaymentMethodType(*pmt))
            .collect(),
        Scope::EventTypes(evts) => evts
            .iter()
            .map(|evt| ScopeIdentifier::EventType(*evt))
            .collect(),
    }
}
```

---

## 5. Updated Domain Request/Response Types

### Request (`crates/hyperswitch_domain_models/src/router_request_types/merchant_connector_webhook_management.rs`)

```rust
#[derive(Debug, Clone)]
pub struct ConnectorWebhookRegisterRequest {
    /// The scope of this webhook registration.
    pub scope: api_models::merchant_connector_webhook_management::Scope,

    /// The base webhook URL to register.
    pub webhook_url: String,

    /// Additional connector-specific metadata (optional).
    pub connector_metadata: Option<serde_json::Value>,
}
```

### Response (`crates/hyperswitch_domain_models/src/router_response_types/merchant_connector_webhook_management.rs`)

```rust
#[derive(Debug, Clone)]
pub struct ConnectorWebhookRegisterResponse {
    /// The scope identifier this response is for.
    pub identifier: api_models::merchant_connector_webhook_management::ScopeIdentifier,

    /// Status of the registration.
    pub status: common_enums::WebhookRegistrationStatus,

    /// Connector-generated webhook ID, if successful.
    pub connector_webhook_id: Option<String>,

    /// Error code, if the registration failed.
    pub error_code: Option<String>,

    /// Error message, if the registration failed.
    pub error_message: Option<String>,
}
```

---

## 6. Files to Modify

| File | Changes |
|------|---------|
| `crates/api_models/src/merchant_connector_webhook_management.rs` | Add `Scope`, `ScopeType`, `ScopeIdentifier`, `WebhookRegistrationResult`, `WebhookRegistrationError`, update `ConnectorWebhookRegisterRequest`, replace `RegisterConnectorWebhookResponse` |
| `crates/hyperswitch_domain_models/src/router_request_types/merchant_connector_webhook_management.rs` | Update `ConnectorWebhookRegisterRequest` to use new `Scope` |
| `crates/hyperswitch_domain_models/src/router_response_types/merchant_connector_webhook_management.rs` | Update `ConnectorWebhookRegisterResponse` to include `ScopeIdentifier` |
| `crates/hyperswitch_interfaces/src/api.rs` | Add `get_webhook_registration_plan` to `ConnectorSpecifications` trait |
| `crates/hyperswitch_connectors/src/connectors/santander.rs` | Implement `get_webhook_registration_plan` |
| `crates/hyperswitch_connectors/src/connectors/payload.rs` | Implement `get_webhook_registration_plan` |
| `crates/hyperswitch_connectors/src/connectors/adyen.rs` | Implement `get_webhook_registration_plan` |
| `crates/router/src/core/merchant_connector_webhook_management.rs` | Rewrite core loop to iterate over registration plan |
| `crates/router/src/core/merchant_connector_webhook_management/transformers.rs` | Update transformers to handle new types |

---

## 7. Backward Compatibility

### API Consumers
- Old clients sending `event_type` will need to migrate to the new `scope` field.
- Deprecation cycle: keep `event_type` as deprecated for one release, mapping internally to `Scope::EventTypes([event])`.

### Connectors
- Existing connectors without `get_webhook_registration_plan` will compile due to the default implementation returning `Vec::new()`.
- Connectors opting into the new flow must implement the method.

---

## 8. Future Extensibility

To add a new scope type (e.g., `RefundReason`):

1. **Add variant to `Scope`:**
   ```rust
   pub enum Scope {
       // ... existing ...
       RefundReasons(Vec<common_enums::RefundReason>),
   }
   ```

2. **Add variant to `ScopeType`:**
   ```rust
   pub enum ScopeType {
       // ... existing ...
       RefundReason,
   }
   ```

3. **Add variant to `ScopeIdentifier`:**
   ```rust
   pub enum ScopeIdentifier {
       // ... existing ...
       RefundReason(common_enums::RefundReason),
   }
   ```

4. **Implement `get_webhook_registration_plan` for relevant connectors.**

No changes needed to the response structure or core loop logic.
