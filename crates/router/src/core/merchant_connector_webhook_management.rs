mod transformers;
use common_utils::id_type;
use error_stack::ResultExt;
use hyperswitch_domain_models::{
    merchant_connector_account::MerchantConnectorAccountUpdate,
    router_request_types::merchant_connector_webhook_management::ConnectorWebhookRegisterRequest,
    router_response_types::merchant_connector_webhook_management::ConnectorWebhookRegisterResponse,
};
// Renamed to avoid ambiguity with the domain-models type (which now contains `scope` + `webhook_url`).
use api_models::merchant_connector_webhook_management::ConnectorWebhookRegisterRequest as ApiConnectorWebhookRegisterRequest;
// NEW: Import `ConnectorSpecifications` so we can call `get_webhook_registration_plan(...)`.
use hyperswitch_interfaces::api::ConnectorSpecifications;
use transformers as configure_connector_webhook_flow;

use crate::{
    core::{
        errors::{self, RouterResponse, StorageErrorExt},
        utils as core_utils,
    },
    errors::utils::ConnectorErrorExt,
    routes::SessionState,
    services::{
        self,
        api::{self as service_api},
    },
    types::api,
};

/// Registers webhooks for a connector.
///
/// REDESIGNED FLOW (multi-scope support):
/// ───────────────────────────────────────
/// 1. Resolve the connector & validate that it supports auto-configuration.
/// 2. Extract enabled payment-method types from the MCA (needed for PMT-scoped connectors).
/// 3. Call `ConnectorSpecifications::get_webhook_registration_plan(...)` which returns
///    `Vec<(ScopeIdentifier, webhook_url)>` — one entry per connector API call.
/// 4. Loop over the plan, build a per-item `ConnectorWebhookRegisterRequest`, execute
///    the connector integration, and collect success / failure into `WebhookRegistrationResult`s.
/// 5. Persist registration metadata back into the MCA record.
/// 6. Aggregate everything into the unified `RegisterConnectorWebhookResponse`.
///
/// WHY THIS DESIGN?
/// • Connectors like **Santander** need one call *per payment-method type* (each with its own URL).
/// • Connectors like **Payload** need one call *per event type*.
/// • Connectors like **Adyen** need a single call with no scoping (`NotSpecific`).
/// • Using a `registration_plan` keeps the orchestration generic while letting each connector
///   decide how many calls it needs and what URLs to use.
#[cfg(feature = "v1")]
pub async fn register_connector_webhook(
    state: SessionState,
    merchant_id: &id_type::MerchantId,
    profile_id: Option<id_type::ProfileId>,
    merchant_connector_id: &id_type::MerchantConnectorAccountId,
    req: ApiConnectorWebhookRegisterRequest,
) -> RouterResponse<
    api_models::merchant_connector_webhook_management::RegisterConnectorWebhookResponse,
> {
    let db = state.store.as_ref();
    let key_store = db
        .get_merchant_key_store_by_merchant_id(merchant_id, &db.get_master_key().to_vec().into())
        .await
        .to_not_found_response(errors::ApiErrorResponse::MerchantAccountNotFound)?;

    let mca = db
        .find_by_merchant_connector_account_merchant_id_merchant_connector_id(
            merchant_id,
            merchant_connector_id,
            &key_store,
        )
        .await
        .to_not_found_response(errors::ApiErrorResponse::MerchantConnectorAccountNotFound {
            id: merchant_connector_id.get_string_repr().to_string(),
        })?;
    core_utils::validate_profile_id_from_auth_layer(profile_id, &mca)?;
    let connector_name = mca.connector_name.clone();
    let profile_id_str = mca.profile_id.clone().get_string_repr().to_string();

    let connector_data = api::ConnectorData::get_connector_by_name(
        &state.conf.connectors,
        &connector_name,
        api::GetToken::Connector,
        Some(mca.merchant_connector_id.clone()),
    )?;

    // Step 1: Validate that the connector supports webhook auto-registration.
    configure_connector_webhook_flow::validate_webhook_registration_request(
        &connector_data,
        req.clone(),
        &state.conf.connectors,
    )
    .await?;

    // Step 2: Extract payment-method types enabled for this MCA.
    // Used by connectors like Santander to build per-PMT registration plans.
    let enabled_payment_methods =
        configure_connector_webhook_flow::get_enabled_payment_method_types(&mca);

    // Step 3: Ask the connector for its registration plan.
    // Returns a list of (scope_identifier, webhook_url) tuples.
    // Example for Santander + Pix/Boleto:
    //   [(PaymentMethodType(Pix), url1), (PaymentMethodType(Boleto), url2), ...]
    let registration_plan = connector_data
        .connector
        .get_webhook_registration_plan(&req.scope, &enabled_payment_methods, &state.conf.connectors);

    // Derive metadata for the final aggregated response.
    let scope_type = configure_connector_webhook_flow::determine_scope_type(&req.scope);
    let requested = configure_connector_webhook_flow::extract_requested_identifiers(&req.scope);

    let mut results = Vec::new();

    // Step 4: Iterate over the plan and call the connector integration once per item.
    for (identifier, webhook_url) in registration_plan {
        // Build a scoped request carrying exactly one identifier + the URL returned by the plan.
        let scoped_request = ConnectorWebhookRegisterRequest {
            scope: identifier.clone(),
            webhook_url: webhook_url.clone(),
        };

        let connector_integration: services::BoxedConnectorWebhookConfigurationInterface<
            api::ConnectorWebhookRegister,
            ConnectorWebhookRegisterRequest,
            ConnectorWebhookRegisterResponse,
        > = connector_data.connector.get_connector_integration();

        // Build the RouterData that flows through the connector integration.
        let router_data = configure_connector_webhook_flow::construct_webhook_register_router_data(
            &state,
            &mca,
            scoped_request,
            webhook_url,
        )
        .await?;

        // Execute the connector processing step.
        let response = services::execute_connector_processing_step(
            &state,
            connector_integration,
            &router_data,
            common_enums::CallConnectorAction::Trigger,
            None,
            None,
        )
        .await
        .to_webhook_configuration_failed_response()
        .attach_printable("Failed while calling register webhook connector api")?;

        // Convert the raw connector response into our result model.
        let result = match response.response {
            Ok(success) => api_models::merchant_connector_webhook_management::WebhookRegistrationResult {
                identifier: identifier.clone(),
                status: success.status,
                connector_webhook_id: success.connector_webhook_id,
                error: None,
            },
            Err(err) => api_models::merchant_connector_webhook_management::WebhookRegistrationResult {
                identifier: identifier.clone(),
                status: common_enums::WebhookRegistrationStatus::Failure,
                connector_webhook_id: None,
                error: Some(api_models::merchant_connector_webhook_management::WebhookRegistrationError {
                    code: err.code,
                    message: err.message,
                }),
            },
        };

        // Step 5: Persist metadata back to the MCA (e.g. connector_webhook_id).
        let connector_webhook_registration_details =
            configure_connector_webhook_flow::construct_connector_webhook_registration_details(
                &ConnectorWebhookRegisterResponse {
                    identifier: identifier.clone(),
                    status: result.status,
                    connector_webhook_id: result.connector_webhook_id.clone(),
                    error_code: result.error.as_ref().map(|e| e.code.clone()),
                    error_message: result.error.as_ref().map(|e| e.message.clone()),
                },
                &mca,
                &router_data.request,
            )?;

        let should_update_db = matches!(
            connector_webhook_registration_details,
            MerchantConnectorAccountUpdate::ConnectorWebhookRegisterationUpdate {
                connector_webhook_registration_details: Some(_)
            }
        );

        if should_update_db {
            db.update_merchant_connector_account(
                mca.clone(),
                connector_webhook_registration_details.into(),
                &key_store,
            )
            .await
            .change_context(
                errors::ApiErrorResponse::DuplicateMerchantConnectorAccount {
                    profile_id: profile_id_str.clone(),
                    connector_label: connector_name.to_owned(),
                },
            )
            .attach_printable_lazy(|| {
                format!(
                    "Failed while updating MerchantConnectorAccount: id: {merchant_connector_id:?}",
                )
            })?;
        }

        results.push(result);
    }

    // Step 6: Build the final aggregated response.
    let response =
        configure_connector_webhook_flow::construct_connector_webhook_registration_response(
            results,
            scope_type,
            requested,
        )?;

    Ok(service_api::ApplicationResponse::Json(response))
}

#[cfg(feature = "v1")]
pub async fn fetch_connector_webhook(
    state: SessionState,
    merchant_id: id_type::MerchantId,
    profile_id: Option<id_type::ProfileId>,
    merchant_connector_id: id_type::MerchantConnectorAccountId,
) -> RouterResponse<api_models::merchant_connector_webhook_management::ConnectorWebhookListResponse>
{
    let store = state.store.as_ref();
    let key_store = store
        .get_merchant_key_store_by_merchant_id(
            &merchant_id,
            &store.get_master_key().to_vec().into(),
        )
        .await
        .to_not_found_response(errors::ApiErrorResponse::MerchantAccountNotFound)?;

    let mca = store
        .find_by_merchant_connector_account_merchant_id_merchant_connector_id(
            &merchant_id,
            &merchant_connector_id,
            &key_store,
        )
        .await
        .to_not_found_response(errors::ApiErrorResponse::MerchantConnectorAccountNotFound {
            id: merchant_connector_id.get_string_repr().to_string(),
        })?;

    let connector_webook_data =
        configure_connector_webhook_flow::get_connector_webhook_list_response(
            &mca.connector_webhook_registration_details,
        )?;

    core_utils::validate_profile_id_from_auth_layer(profile_id, &mca)?;

    Ok(service_api::ApplicationResponse::Json(
        api_models::merchant_connector_webhook_management::ConnectorWebhookListResponse {
            connector: mca.connector_name.clone(),
            webhooks: connector_webook_data,
        },
    ))
}
