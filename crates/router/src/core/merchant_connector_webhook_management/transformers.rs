use std::marker::PhantomData;

use api_models::merchant_connector_webhook_management::{
    ConnectorWebhookRegisterRequest, RegisterConnectorWebhookResponse, Scope,
    ScopeIdentifier, ScopeType, WebhookRegistrationResult,
};
use common_utils::ext_traits::ValueExt;
use error_stack::ResultExt;
use hyperswitch_interfaces::api::ConnectorSpecifications;
use router_env::tracing::{self, instrument};

use crate::{
    consts,
    core::errors::RouterResult,
    errors, types,
    types::{
        api::ConnectorData, domain,
        // Alias to distinguish domain-level request (scope + webhook_url) from API-level request (scope only).
        ConnectorWebhookRegisterRequest as ConnectorWebhookRegisterData,
        ConnectorWebhookRegisterResponse, ConnectorWebhookRegisterRouterData, ErrorResponse,
    },
    SessionState,
};
use hyperswitch_domain_models::connector_endpoints::Connectors;

#[cfg(feature = "v2")]
pub async fn construct_webhook_register_router_data(
    _state: &SessionState,
    _merchant_connector_account: domain::MerchantConnectorAccount,
    _webhook_register_request: ConnectorWebhookRegisterRequest,
) -> RouterResult<types::ConnectorWebhookRegisterRouterData> {
    todo!()
}

/// Builds a [`RouterData`] that carries the per-item webhook registration payload into the
/// connector integration layer.
///
/// CHANGED: We now receive `webhook_url` separately because the core orchestrator may invoke
/// multiple registrations with different URLs (e.g. Santander has one URL per PMT).
#[cfg(feature = "v1")]
#[instrument(skip_all)]
pub async fn construct_webhook_register_router_data<'a>(
    state: &'a SessionState,
    merchant_connector_account: &domain::MerchantConnectorAccount,
    webhook_register_request: hyperswitch_domain_models::router_request_types::merchant_connector_webhook_management::ConnectorWebhookRegisterRequest,
    webhook_url: String,
) -> RouterResult<ConnectorWebhookRegisterRouterData> {
    let auth_type = merchant_connector_account
        .get_connector_account_details()
        .change_context(errors::ApiErrorResponse::InternalServerError)?;

    // Bundle the narrowed scope + the specific webhook URL for this iteration.
    let request = ConnectorWebhookRegisterData {
        scope: webhook_register_request.scope,
        webhook_url,
    };

    Ok(types::RouterData {
        flow: PhantomData,
        merchant_id: merchant_connector_account.merchant_id.clone(),
        customer_id: None,
        connector_customer: None,
        connector: merchant_connector_account.connector_name.clone(),
        payment_id: consts::IRRELEVANT_PAYMENT_INTENT_ID.to_owned(),
        tenant_id: state.tenant.tenant_id.clone(),
        attempt_id: consts::IRRELEVANT_PAYMENT_ATTEMPT_ID.to_owned(),
        status: common_enums::AttemptStatus::default(),
        payment_method: common_enums::PaymentMethod::default(),
        payment_method_type: None,
        connector_auth_type: auth_type,
        description: None,
        address: types::PaymentAddress::default(),
        auth_type: common_enums::AuthenticationType::default(),
        connector_meta_data: merchant_connector_account.get_metadata().clone(),
        connector_wallets_details: merchant_connector_account.get_connector_wallets_details(),
        amount_captured: None,
        minor_amount_captured: None,
        access_token: None,
        session_token: None,
        reference_id: None,
        payment_method_token: None,
        recurring_mandate_payment_data: None,
        preprocessing_id: None,
        payment_method_balance: None,
        connector_api_version: None,
        request,
        response: Err(ErrorResponse::default()),
        connector_request_reference_id: consts::IRRELEVANT_CONNECTOR_REQUEST_REFERENCE_ID
            .to_owned(),
        #[cfg(feature = "payouts")]
        payout_method_data: None,
        #[cfg(feature = "payouts")]
        quote_id: None,
        test_mode: None,
        connector_http_status_code: None,
        external_latency: None,
        apple_pay_flow: None,
        frm_metadata: None,
        dispute_id: None,
        refund_id: None,
        payment_method_status: None,
        connector_response: None,
        integrity_check: Ok(()),
        additional_merchant_data: None,
        header_payload: None,
        connector_mandate_request_reference_id: None,
        authentication_id: None,
        psd2_sca_exemption_type: None,
        raw_connector_response: None,
        is_payment_id_from_merchant: None,
        l2_l3_data: None,
        minor_amount_capturable: None,
        authorized_amount: None,
        payout_id: None,
        customer_document_details: None,
        feature_data: None,
        sender_payment_instrument_id: None,
    })
}

/// Persists connector webhook registration metadata into the MCA row.
///
/// CHANGED: Instead of storing a flat `event_type`, we now serialise the full `Scope`
/// so that later retrievals know *what* was registered (PMT, event type, or not specific).
#[cfg(feature = "v1")]
pub fn construct_connector_webhook_registration_details(
    register_webhook_response: &ConnectorWebhookRegisterResponse,
    merchant_connector_account: &domain::MerchantConnectorAccount,
    connector_webhook_register_data: &ConnectorWebhookRegisterData,
) -> RouterResult<domain::MerchantConnectorAccountUpdate> {
    if let Some(connector_webhook_id) = register_webhook_response.connector_webhook_id.clone() {
        let mut connector_webhook_registration_details = merchant_connector_account
            .get_connector_webhook_registration_details()
            .unwrap_or_else(|| serde_json::Value::Object(Default::default()));

        let map = connector_webhook_registration_details
            .as_object_mut()
            .ok_or(errors::ApiErrorResponse::InternalServerError)?;

        // Encode the scope that was just registered so the DB row stays self-describing.
        let entry_value = match &connector_webhook_register_data.scope {
            ScopeIdentifier::NotSpecific => serde_json::json!({"type": "not_specific"}),
            ScopeIdentifier::PaymentMethodType(pmt) => {
                serde_json::json!({"type": "payment_method_type", "value": pmt})
            }
            ScopeIdentifier::EventType(evt) => {
                serde_json::json!({"type": "event_type", "value": evt})
            }
        };

        map.insert(connector_webhook_id, entry_value);

        Ok(
            domain::MerchantConnectorAccountUpdate::ConnectorWebhookRegisterationUpdate {
                connector_webhook_registration_details: Some(
                    connector_webhook_registration_details,
                ),
            },
        )
    } else {
        Ok(
            domain::MerchantConnectorAccountUpdate::ConnectorWebhookRegisterationUpdate {
                connector_webhook_registration_details: None,
            },
        )
    }
}

/// Validates that the requested scope can actually be handled by this connector.
///
/// REPLACED the old event-type-based validation with a scope-plan check:
/// if the connector returns an empty registration plan for the requested scope,
/// the request is rejected early.
#[cfg(feature = "v1")]
#[instrument(skip_all)]
pub async fn validate_webhook_registration_request(
    connector_data: &ConnectorData,
    webhook_register_request: ConnectorWebhookRegisterRequest,
    connectors: &Connectors,
) -> RouterResult<()> {
    let config = connector_data.connector.get_api_webhook_config();

    if !config.is_webhook_auto_configuration_supported {
        return Err(errors::ApiErrorResponse::FlowNotSupported {
            flow: "Webhook Registration".to_string(),
            connector: connector_data.connector_name.to_string(),
        }
        .into());
    }

    // NEW: Ask the connector for a plan. Empty plan == unsupported scope.
    let plan = connector_data.connector.get_webhook_registration_plan(
        &webhook_register_request.scope,
        &[],
        connectors,
    );

    if plan.is_empty() {
        return Err(errors::ApiErrorResponse::InvalidRequestData {
            message: "Webhook registration is not supported for the requested scope".to_string(),
        }
        .into());
    }

    Ok(())
}

/// Parses the opaque `payment_methods_enabled` blobs attached to the MCA into concrete
/// `PaymentMethodType`s so the orchestrator can feed them into `get_webhook_registration_plan`.
#[cfg(feature = "v1")]
pub fn get_enabled_payment_method_types(
    merchant_connector_account: &domain::MerchantConnectorAccount,
) -> Vec<common_enums::PaymentMethodType> {


    merchant_connector_account
        .payment_methods_enabled
        .clone()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|pm| {
            pm.parse_value::<api_models::admin::PaymentMethodsEnabled>("payment_methods_enabled")
                .inspect_err(|err| {
                    router_env::logger::error!("Unable to deserialize payment methods enabled: {:?}", err);
                })
                .ok()
        })
        .flat_map(|parsed| {
            parsed
                .payment_method_types
                .unwrap_or_default()
                .into_iter()
                .map(|pmt| pmt.payment_method_type)
        })
        .collect()
}

/// Maps the request `Scope` onto the response discriminator `ScopeType`.
pub fn determine_scope_type(scope: &Scope) -> ScopeType {
    match scope {
        Scope::NotSpecific => ScopeType::NotSpecific,
        Scope::PaymentMethodTypes(_) => ScopeType::PaymentMethodType,
        Scope::EventTypes(_) => ScopeType::EventType,
        _ => ScopeType::NotSpecific,
    }
}

/// Expands a `Scope` into the flat list of identifiers that appear under `requested` in the response.
pub fn extract_requested_identifiers(scope: &Scope) -> Vec<ScopeIdentifier> {
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
        _ => vec![ScopeIdentifier::NotSpecific],
    }
}

/// Aggregates per-item results into the final API response struct.
#[cfg(feature = "v1")]
pub fn construct_connector_webhook_registration_response(
    results: Vec<WebhookRegistrationResult>,
    scope_type: ScopeType,
    requested: Vec<ScopeIdentifier>,
) -> RouterResult<RegisterConnectorWebhookResponse> {
    Ok(RegisterConnectorWebhookResponse {
        scope_type,
        requested,
        results,
    })
}

/// Unchanged legacy helper — converts the raw MCA webhook JSON blob into API response structs.
#[cfg(feature = "v1")]
pub fn get_connector_webhook_list_response(
    register_webhook_response: &Option<serde_json::Value>,
) -> RouterResult<Vec<api_models::merchant_connector_webhook_management::ConnectorWebhookResponse>>
{
    use std::collections::HashMap;

    let webhook_map: HashMap<String, domain::ConnectorWebhookData> = match register_webhook_response
    {
        Some(webhook_response) => serde_json::from_value(webhook_response.clone())
            .change_context(errors::ApiErrorResponse::InternalServerError)?,
        None => HashMap::new(),
    };

    let webhooks = webhook_map
        .into_iter()
        .map(|(connector_webhook_id, webhook_data)| {
            api_models::merchant_connector_webhook_management::ConnectorWebhookResponse {
                event_type: webhook_data.event_type,
                connector_webhook_id,
            }
        })
        .collect();

    Ok(webhooks)
}
