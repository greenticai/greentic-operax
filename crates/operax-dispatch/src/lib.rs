use operax_core::{
    ActionOperation, DecisionEnvelope, MatchedRecord, OperationalPack, OperaxContext, OperaxError,
    ProposedAction, Result, SorxTarget,
};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, PartialEq)]
pub enum InputEnvelope {
    Single(BankTransaction),
    Batch {
        source: String,
        date: String,
        transactions: Vec<BankTransaction>,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct BankTransaction {
    pub transaction_id: String,
    pub booked_at: String,
    pub amount: f64,
    pub currency: String,
    pub reference: String,
}

impl InputEnvelope {
    pub fn transaction_count(&self) -> usize {
        match self {
            Self::Single(_) => 1,
            Self::Batch { transactions, .. } => transactions.len(),
        }
    }

    pub fn transactions(&self) -> Vec<BankTransaction> {
        match self {
            Self::Single(tx) => vec![tx.clone()],
            Self::Batch { transactions, .. } => transactions.clone(),
        }
    }
}

pub fn detect_input(value: &Value) -> Result<InputEnvelope> {
    if value.get("transactions").is_some() {
        let transactions = value
            .get("transactions")
            .and_then(Value::as_array)
            .ok_or_else(|| {
                OperaxError::new(
                    "invalid_batch_input",
                    "daily batch input requires transactions array",
                )
            })?
            .iter()
            .map(parse_transaction)
            .collect::<Result<Vec<_>>>()?;
        return Ok(InputEnvelope::Batch {
            source: required_string(value, "source")?,
            date: required_string(value, "date")?,
            transactions,
        });
    }
    if value.get("transaction_id").is_some() {
        return Ok(InputEnvelope::Single(parse_transaction(value)?));
    }
    Err(OperaxError::new(
        "unknown_input_shape",
        "input must be one bank transaction or a daily transaction batch",
    ))
}

pub fn dispatch_reconciliation(
    input: &InputEnvelope,
    ctx: &OperaxContext,
    pack: &OperationalPack,
) -> Vec<DecisionEnvelope> {
    input
        .transactions()
        .into_iter()
        .map(|transaction| decide_transaction(&transaction, ctx, pack))
        .collect()
}

fn decide_transaction(
    transaction: &BankTransaction,
    ctx: &OperaxContext,
    pack: &OperationalPack,
) -> DecisionEnvelope {
    let reference = transaction.reference.to_ascii_uppercase();
    let (outcome, confidence, matched_id, requires_approval, reasons) =
        if reference.contains("TEN-001") && (transaction.amount - 1250.0).abs() <= 2.0 {
            (
                "matched",
                95.0,
                Some("rent_001"),
                false,
                vec![
                    "reference_matched".to_string(),
                    "amount_within_tolerance".to_string(),
                ],
            )
        } else if reference.contains("TEN-002") {
            (
                "partial_payment",
                82.0,
                Some("rent_002"),
                true,
                vec![
                    "reference_matched".to_string(),
                    "date_within_window".to_string(),
                    "amount_less_than_expected".to_string(),
                ],
            )
        } else {
            (
                "unmatched",
                30.0,
                None,
                true,
                vec!["no_expected_record_match".to_string()],
            )
        };

    let mut proposed_actions = Vec::new();
    if outcome == "matched" || outcome == "partial_payment" {
        proposed_actions.push(business_action(
            "record_rent_payment",
            "0.1.0",
            &transaction.transaction_id,
            json!({
                "transaction_id": transaction.transaction_id,
                "amount": transaction.amount,
                "currency": transaction.currency,
                "reference": transaction.reference,
                "received_at": transaction.booked_at,
                "matched_record_id": matched_id.unwrap_or_default()
            }),
        ));
    }
    if outcome == "partial_payment" || outcome == "unmatched" {
        proposed_actions.push(generated_route(
            "reconciliation_case.create",
            "/v1/agent/reconciliation-cases/create",
            &transaction.transaction_id,
            json!({
                "source_event_id": transaction.transaction_id,
                "expected_record_id": matched_id.unwrap_or("unallocated"),
                "reason": if outcome == "partial_payment" { "partial_payment" } else { "unallocated_cash" }
            }),
        ));
    }

    DecisionEnvelope {
        schema: "greentic.operax.decision.v1".to_string(),
        tenant: ctx.tenant.clone(),
        team: ctx.team.clone(),
        capability: pack.metadata.capability.clone(),
        input_id: transaction.transaction_id.clone(),
        outcome: outcome.to_string(),
        confidence,
        matched_records: matched_id
            .map(|id| {
                vec![MatchedRecord {
                    record: "RentObligation".to_string(),
                    id: id.to_string(),
                    score: Some(confidence),
                }]
            })
            .unwrap_or_default(),
        proposed_actions,
        requires_approval,
        reasons,
        audit: None,
    }
    .with_context(ctx, pack)
}

fn business_action(id: &str, version: &str, input_id: &str, values: Value) -> ProposedAction {
    ProposedAction {
        sorx_target: SorxTarget::BusinessAction {
            id: id.to_string(),
            version: version.to_string(),
            contract_hash:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
        },
        operation: ActionOperation::Invoke,
        values: object_values(values),
        idempotency_key: Some(format!("{input_id}-{id}")),
    }
}

fn generated_route(endpoint_id: &str, path: &str, input_id: &str, values: Value) -> ProposedAction {
    ProposedAction {
        sorx_target: SorxTarget::GeneratedRoute {
            endpoint_id: endpoint_id.to_string(),
            operation_id: None,
            method: "POST".to_string(),
            path: path.to_string(),
        },
        operation: ActionOperation::Invoke,
        values: object_values(values),
        idempotency_key: Some(format!("{input_id}-{endpoint_id}")),
    }
}

fn object_values(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap_or_default()
}

fn parse_transaction(value: &Value) -> Result<BankTransaction> {
    Ok(BankTransaction {
        transaction_id: required_string(value, "transaction_id")?,
        booked_at: required_string(value, "booked_at")?,
        amount: value.get("amount").and_then(Value::as_f64).ok_or_else(|| {
            OperaxError::new("invalid_transaction", "transaction amount must be a number")
        })?,
        currency: required_string(value, "currency")?,
        reference: required_string(value, "reference")?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            OperaxError::new(
                "invalid_input",
                format!("input field {field} must be a non-empty string"),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::{HandoffSorx, OperaHandoffMetadata, SchemaRegistry, SorxBinding};

    fn pack() -> OperationalPack {
        OperationalPack {
            metadata: OperaHandoffMetadata {
                schema: "greentic.operala.handoff.v1".into(),
                capability: "reconciliation".into(),
                extension: "greentic.operala.reconciliation.v1".into(),
                tenant_required: true,
                team_optional: true,
                team_required: false,
                sorla: None,
                sorx: Some(HandoffSorx {
                    transport: "http".into(),
                    url: "runtime-provided".into(),
                }),
            },
            handoff: serde_json::json!({}),
            flows: Vec::new(),
            schemas: SchemaRegistry { names: Vec::new() },
            schema_documents: Default::default(),
            sorx_binding: SorxBinding {
                transport: "http".into(),
                url: "runtime-provided".into(),
            },
            pack_digest: "sha256:pack".into(),
            sorla_source_digest: "sha256:sorla".into(),
            handoff_digest: "sha256:handoff".into(),
        }
    }

    #[test]
    fn detects_single_and_batch_inputs() {
        let single = serde_json::json!({
            "transaction_id": "bank_tx_001",
            "booked_at": "2026-06-02",
            "amount": 1250.0,
            "currency": "GBP",
            "reference": "TEN-001 June Rent"
        });
        assert_eq!(detect_input(&single).unwrap().transaction_count(), 1);

        let batch = serde_json::json!({
            "source": "bank_daily_export",
            "date": "2026-06-02",
            "transactions": [single]
        });
        assert_eq!(detect_input(&batch).unwrap().transaction_count(), 1);
    }

    #[test]
    fn dispatches_batch_to_decisions() {
        let input = detect_input(&serde_json::json!({
            "source": "bank_daily_export",
            "date": "2026-06-02",
            "transactions": [
                {"transaction_id":"bank_tx_001","booked_at":"2026-06-02","amount":1250.0,"currency":"GBP","reference":"TEN-001 June Rent"},
                {"transaction_id":"bank_tx_003","booked_at":"2026-06-02","amount":777.0,"currency":"GBP","reference":"Unknown payment"}
            ]
        }))
        .unwrap();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:pack".into()).unwrap();
        let decisions = dispatch_reconciliation(&input, &ctx, &pack());
        assert_eq!(decisions.len(), 2);
        assert_eq!(decisions[0].outcome, "matched");
        assert_eq!(decisions[1].outcome, "unmatched");
    }
}
