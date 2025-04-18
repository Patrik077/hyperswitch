use common_enums::{enums, Currency};
use common_utils::{consts::BASE64_ENGINE, date_time, types::MinorUnit};
use error_stack::ResultExt;
use hyperswitch_domain_models::{
    payment_method_data::PaymentMethodData,
    router_data::{ConnectorAuthType, RouterData},
    router_flow_types::refunds::{Execute, RSync},
    router_request_types::ResponseId,
    router_response_types::{PaymentsResponseData, RefundsResponseData},
    types,
};
use hyperswitch_interfaces::errors;
use masking::{PeekInterface, Secret};
use ring::digest;
use serde::{Deserialize, Serialize};

use crate::{
    types::{RefundsResponseRouterData, ResponseRouterData},
    utils::{
        self, generate_random_bytes, BrowserInformationData, CardData as _,
        PaymentsAuthorizeRequestData, PaymentsSyncRequestData, RouterData as _,
    },
};

pub struct PlacetopayRouterData<T> {
    pub amount: MinorUnit,
    pub router_data: T,
}

impl<T> From<(MinorUnit, T)> for PlacetopayRouterData<T> {
    fn from((amount, item): (MinorUnit, T)) -> Self {
        Self {
            amount,
            router_data: item,
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayPaymentsRequest {
    auth: PlacetopayAuth,
    payment: PlacetopayPayment,
    instrument: PlacetopayInstrument,
    ip_address: Secret<String, common_utils::pii::IpAddress>,
    user_agent: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlacetopayAuthorizeAction {
    Checkin,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayAuthType {
    login: Secret<String>,
    tran_key: Secret<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayAuth {
    login: Secret<String>,
    tran_key: Secret<String>,
    nonce: Secret<String>,
    seed: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayPayment {
    reference: String,
    description: String,
    amount: PlacetopayAmount,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayAmount {
    currency: Currency,
    total: MinorUnit,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayInstrument {
    card: PlacetopayCard,
}

#[derive(Debug, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayCard {
    number: cards::CardNumber,
    expiration: Secret<String>,
    cvv: Secret<String>,
}

impl TryFrom<&PlacetopayRouterData<&types::PaymentsAuthorizeRouterData>>
    for PlacetopayPaymentsRequest
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: &PlacetopayRouterData<&types::PaymentsAuthorizeRouterData>,
    ) -> Result<Self, Self::Error> {
        let browser_info = item.router_data.request.get_browser_info()?;
        let ip_address = browser_info.get_ip_address()?;
        let user_agent = browser_info.get_user_agent()?;
        let auth = PlacetopayAuth::try_from(&item.router_data.connector_auth_type)?;
        let payment = PlacetopayPayment {
            reference: item.router_data.connector_request_reference_id.clone(),
            description: item.router_data.get_description()?,
            amount: PlacetopayAmount {
                currency: item.router_data.request.currency,
                total: item.amount,
            },
        };
        match item.router_data.request.payment_method_data.clone() {
            PaymentMethodData::Card(req_card) => {
                let card = PlacetopayCard {
                    number: req_card.card_number.clone(),
                    expiration: req_card
                        .clone()
                        .get_card_expiry_month_year_2_digit_with_delimiter("/".to_owned())?,
                    cvv: req_card.card_cvc.clone(),
                };
                Ok(Self {
                    ip_address,
                    user_agent,
                    auth,
                    payment,
                    instrument: PlacetopayInstrument {
                        card: card.to_owned(),
                    },
                })
            }
            PaymentMethodData::Wallet(_)
            | PaymentMethodData::CardRedirect(_)
            | PaymentMethodData::PayLater(_)
            | PaymentMethodData::BankRedirect(_)
            | PaymentMethodData::BankDebit(_)
            | PaymentMethodData::BankTransfer(_)
            | PaymentMethodData::Crypto(_)
            | PaymentMethodData::MandatePayment
            | PaymentMethodData::Reward
            | PaymentMethodData::RealTimePayment(_)
            | PaymentMethodData::MobilePayment(_)
            | PaymentMethodData::Upi(_)
            | PaymentMethodData::Voucher(_)
            | PaymentMethodData::GiftCard(_)
            | PaymentMethodData::OpenBanking(_)
            | PaymentMethodData::CardToken(_)
            | PaymentMethodData::NetworkToken(_)
            | PaymentMethodData::CardDetailsForNetworkTransactionId(_) => {
                Err(errors::ConnectorError::NotImplemented(
                    utils::get_unimplemented_payment_method_error_message("Placetopay"),
                )
                .into())
            }
        }
    }
}

impl TryFrom<&ConnectorAuthType> for PlacetopayAuth {
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(auth_type: &ConnectorAuthType) -> Result<Self, Self::Error> {
        let placetopay_auth = PlacetopayAuthType::try_from(auth_type)?;
        let nonce_bytes = generate_random_bytes(16);
        let now = date_time::date_as_yyyymmddthhmmssmmmz()
            .change_context(errors::ConnectorError::RequestEncodingFailed)?;
        let seed = format!("{}+00:00", now.split_at(now.len() - 5).0);
        let mut context = digest::Context::new(&digest::SHA256);
        context.update(&nonce_bytes);
        context.update(seed.as_bytes());
        context.update(placetopay_auth.tran_key.peek().as_bytes());
        let encoded_digest = base64::Engine::encode(&BASE64_ENGINE, context.finish());
        let nonce = Secret::new(base64::Engine::encode(&BASE64_ENGINE, &nonce_bytes));
        Ok(Self {
            login: placetopay_auth.login,
            tran_key: encoded_digest.into(),
            nonce,
            seed,
        })
    }
}

impl TryFrom<&ConnectorAuthType> for PlacetopayAuthType {
    type Error = error_stack::Report<errors::ConnectorError>;

    fn try_from(auth_type: &ConnectorAuthType) -> Result<Self, Self::Error> {
        if let ConnectorAuthType::BodyKey { api_key, key1 } = auth_type {
            Ok(Self {
                login: api_key.to_owned(),
                tran_key: key1.to_owned(),
            })
        } else {
            Err(errors::ConnectorError::FailedToObtainAuthType)?
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlacetopayTransactionStatus {
    Ok,
    Failed,
    Approved,
    // ApprovedPartial,
    // PartialExpired,
    Rejected,
    Pending,
    PendingValidation,
    PendingProcess,
    // Refunded,
    // Reversed,
    Error,
    // Unknown,
    // Manual,
    // Dispute,
    //The statuses that are commented out are awaiting clarification on the connector.
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayStatusResponse {
    status: PlacetopayTransactionStatus,
}

impl From<PlacetopayTransactionStatus> for enums::AttemptStatus {
    fn from(item: PlacetopayTransactionStatus) -> Self {
        match item {
            PlacetopayTransactionStatus::Approved | PlacetopayTransactionStatus::Ok => {
                Self::Charged
            }
            PlacetopayTransactionStatus::Failed
            | PlacetopayTransactionStatus::Rejected
            | PlacetopayTransactionStatus::Error => Self::Failure,
            PlacetopayTransactionStatus::Pending
            | PlacetopayTransactionStatus::PendingValidation
            | PlacetopayTransactionStatus::PendingProcess => Self::Pending,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayPaymentsResponse {
    status: PlacetopayStatusResponse,
    internal_reference: u64,
    authorization: Option<String>,
}

impl<F, T> TryFrom<ResponseRouterData<F, PlacetopayPaymentsResponse, T, PaymentsResponseData>>
    for RouterData<F, T, PaymentsResponseData>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: ResponseRouterData<F, PlacetopayPaymentsResponse, T, PaymentsResponseData>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            status: enums::AttemptStatus::from(item.response.status.status),
            response: Ok(PaymentsResponseData::TransactionResponse {
                resource_id: ResponseId::ConnectorTransactionId(
                    item.response.internal_reference.to_string(),
                ),
                redirection_data: Box::new(None),
                mandate_reference: Box::new(None),
                connector_metadata: item
                    .response
                    .authorization
                    .clone()
                    .map(|authorization| serde_json::json!(authorization)),
                network_txn_id: None,
                connector_response_reference_id: None,
                incremental_authorization_allowed: None,
                charges: None,
            }),
            ..item.data
        })
    }
}

// REFUND :
// Type definition for RefundRequest
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayRefundRequest {
    auth: PlacetopayAuth,
    internal_reference: u64,
    action: PlacetopayNextAction,
    authorization: Option<String>,
}

impl<F> TryFrom<&types::RefundsRouterData<F>> for PlacetopayRefundRequest {
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(item: &types::RefundsRouterData<F>) -> Result<Self, Self::Error> {
        if item.request.minor_refund_amount == item.request.minor_payment_amount {
            let auth = PlacetopayAuth::try_from(&item.connector_auth_type)?;

            let internal_reference = item
                .request
                .connector_transaction_id
                .parse::<u64>()
                .change_context(errors::ConnectorError::RequestEncodingFailed)?;
            let action = PlacetopayNextAction::Reverse;
            let authorization = match item.request.connector_metadata.clone() {
                Some(metadata) => metadata.as_str().map(|auth| auth.to_string()),
                None => None,
            };
            Ok(Self {
                auth,
                internal_reference,
                action,
                authorization,
            })
        } else {
            Err(errors::ConnectorError::NotSupported {
                message: "Partial Refund".to_string(),
                connector: "placetopay",
            }
            .into())
        }
    }
}

impl From<PlacetopayRefundStatus> for enums::RefundStatus {
    fn from(item: PlacetopayRefundStatus) -> Self {
        match item {
            PlacetopayRefundStatus::Ok
            | PlacetopayRefundStatus::Approved
            | PlacetopayRefundStatus::Refunded => Self::Success,
            PlacetopayRefundStatus::Failed
            | PlacetopayRefundStatus::Rejected
            | PlacetopayRefundStatus::Error => Self::Failure,
            PlacetopayRefundStatus::Pending
            | PlacetopayRefundStatus::PendingProcess
            | PlacetopayRefundStatus::PendingValidation => Self::Pending,
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlacetopayRefundStatus {
    Ok,
    Failed,
    Approved,
    // ApprovedPartial,
    // PartialExpired,
    Rejected,
    Pending,
    PendingValidation,
    PendingProcess,
    Refunded,
    // Reversed,
    Error,
    // Unknown,
    // Manual,
    // Dispute,
    //The statuses that are commented out are awaiting clarification on the connector.
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayRefundStatusResponse {
    status: PlacetopayRefundStatus,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayRefundResponse {
    status: PlacetopayRefundStatusResponse,
    internal_reference: u64,
}

impl TryFrom<RefundsResponseRouterData<Execute, PlacetopayRefundResponse>>
    for types::RefundsRouterData<Execute>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: RefundsResponseRouterData<Execute, PlacetopayRefundResponse>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            response: Ok(RefundsResponseData {
                connector_refund_id: item.response.internal_reference.to_string(),
                refund_status: enums::RefundStatus::from(item.response.status.status),
            }),
            ..item.data
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayRsyncRequest {
    auth: PlacetopayAuth,
    internal_reference: u64,
}

impl TryFrom<&types::RefundsRouterData<RSync>> for PlacetopayRsyncRequest {
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(item: &types::RefundsRouterData<RSync>) -> Result<Self, Self::Error> {
        let auth = PlacetopayAuth::try_from(&item.connector_auth_type)?;
        let internal_reference = item
            .request
            .connector_transaction_id
            .parse::<u64>()
            .change_context(errors::ConnectorError::RequestEncodingFailed)?;
        Ok(Self {
            auth,
            internal_reference,
        })
    }
}

impl TryFrom<RefundsResponseRouterData<RSync, PlacetopayRefundResponse>>
    for types::RefundsRouterData<RSync>
{
    type Error = error_stack::Report<errors::ConnectorError>;
    fn try_from(
        item: RefundsResponseRouterData<RSync, PlacetopayRefundResponse>,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            response: Ok(RefundsResponseData {
                connector_refund_id: item.response.internal_reference.to_string(),
                refund_status: enums::RefundStatus::from(item.response.status.status),
            }),
            ..item.data
        })
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayErrorResponse {
    pub status: PlacetopayError,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayError {
    pub status: PlacetopayErrorStatus,
    pub message: String,
    pub reason: String,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum PlacetopayErrorStatus {
    Failed,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayPsyncRequest {
    auth: PlacetopayAuth,
    internal_reference: u64,
}

impl TryFrom<&types::PaymentsSyncRouterData> for PlacetopayPsyncRequest {
    type Error = error_stack::Report<errors::ConnectorError>;

    fn try_from(item: &types::PaymentsSyncRouterData) -> Result<Self, Self::Error> {
        let auth = PlacetopayAuth::try_from(&item.connector_auth_type)?;
        let internal_reference = item
            .request
            .get_connector_transaction_id()?
            .parse::<u64>()
            .change_context(errors::ConnectorError::RequestEncodingFailed)?;
        Ok(Self {
            auth,
            internal_reference,
        })
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PlacetopayNextActionRequest {
    auth: PlacetopayAuth,
    internal_reference: u64,
    action: PlacetopayNextAction,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum PlacetopayNextAction {
    Refund,
    Reverse,
    Void,
    Process,
    Checkout,
}

impl TryFrom<&types::PaymentsCaptureRouterData> for PlacetopayNextActionRequest {
    type Error = error_stack::Report<errors::ConnectorError>;

    fn try_from(item: &types::PaymentsCaptureRouterData) -> Result<Self, Self::Error> {
        let auth = PlacetopayAuth::try_from(&item.connector_auth_type)?;
        let internal_reference = item
            .request
            .connector_transaction_id
            .parse::<u64>()
            .change_context(errors::ConnectorError::RequestEncodingFailed)?;
        let action = PlacetopayNextAction::Checkout;
        Ok(Self {
            auth,
            internal_reference,
            action,
        })
    }
}

impl TryFrom<&types::PaymentsCancelRouterData> for PlacetopayNextActionRequest {
    type Error = error_stack::Report<errors::ConnectorError>;

    fn try_from(item: &types::PaymentsCancelRouterData) -> Result<Self, Self::Error> {
        let auth = PlacetopayAuth::try_from(&item.connector_auth_type)?;
        let internal_reference = item
            .request
            .connector_transaction_id
            .parse::<u64>()
            .change_context(errors::ConnectorError::RequestEncodingFailed)?;
        let action = PlacetopayNextAction::Void;
        Ok(Self {
            auth,
            internal_reference,
            action,
        })
    }
}
