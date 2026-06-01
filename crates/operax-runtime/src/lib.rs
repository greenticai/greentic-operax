use operax_audit::JsonlAuditSink;
use operax_core::{
    ActionOperation, OperaHandoffMetadata, OperationalPack, OperaxContext, OperaxError, Result,
    RunReport,
};
use operax_dispatch::{detect_input, dispatch_reconciliation};
use operax_pack_loader::load_operational_pack;
use operax_policy::{PolicyOutcome, decide};
use operax_sorx_http::{
    SorxCapabilityClient, SorxClient, action_to_business_call, action_to_generated_route,
    invoke_action_capability,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

mod bulk_ingest;

#[derive(Debug, Clone)]
pub struct RunRequest {
    pub artifact: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub locale: Option<String>,
    pub caller_role: Option<String>,
    pub input: Value,
    pub dry_run: bool,
    pub audit_dir: Option<PathBuf>,
}

pub fn run_artifact_with_client<C: SorxClient>(
    request: RunRequest,
    client: &C,
) -> Result<RunReport> {
    let pack = load_operational_pack(&request.artifact)?;
    run_pack_with_client(pack, request, client)
}

pub fn run_pack_with_client<C: SorxClient>(
    pack: OperationalPack,
    request: RunRequest,
    client: &C,
) -> Result<RunReport> {
    if pack.metadata.team_required && request.team.is_none() {
        return Err(OperaxError::new(
            "missing_team",
            "this handoff requires a team",
        ));
    }
    reject_secret_like_input(&request.input)?;
    let ctx = OperaxContext::new(
        request.tenant,
        request.team,
        request.locale,
        pack.pack_digest.clone(),
    )?;
    let ctx = match request.caller_role {
        Some(caller_role) => ctx.with_caller_role(caller_role)?,
        None => ctx,
    };
    run_loaded_pack(
        &pack,
        &ctx,
        request.input,
        request.dry_run,
        request.audit_dir.as_deref(),
        client,
    )
}

pub fn run_loaded_pack<C: SorxClient + ?Sized>(
    pack: &OperationalPack,
    ctx: &OperaxContext,
    input_json: Value,
    dry_run: bool,
    audit_dir: Option<&Path>,
    client: &C,
) -> Result<RunReport> {
    reject_secret_like_input(&input_json)?;
    let audit = JsonlAuditSink::new(audit_dir);
    audit.run_started(ctx)?;

    let capability = pack
        .handoff
        .get("capability")
        .and_then(Value::as_str)
        .unwrap_or(pack.metadata.capability.as_str());
    if capability == "bulk_ingest" {
        let report = bulk_ingest::run_bulk_ingest(pack, ctx, input_json, dry_run, client)?;
        audit.run_completed(ctx)?;
        return Ok(report);
    }

    let input = detect_input(&input_json)?;
    audit.write_event(
        "operax.input.accepted",
        &serde_json::json!({"tenant": ctx.tenant, "count": input.transaction_count()}),
    )?;

    let decisions = dispatch_reconciliation(&input, ctx, pack);
    let mut applied_actions = 0usize;
    let mut skipped_actions = 0usize;

    for decision in &decisions {
        audit.decision_created(decision)?;
        let outcome = decide(decision)?;
        for action in &decision.proposed_actions {
            if dry_run {
                skipped_actions += 1;
                audit.action_skipped(ctx, action)?;
                continue;
            }
            match outcome {
                PolicyOutcome::Denied | PolicyOutcome::RequiresApproval => {
                    skipped_actions += 1;
                    audit.action_skipped(ctx, action)?;
                }
                PolicyOutcome::AutoApply => {
                    apply_pack_action(client, ctx, action, &pack.metadata)?;
                    applied_actions += 1;
                    audit.action_applied(ctx, action)?;
                }
            }
        }
    }

    let report = RunReport::reconciliation(
        input.transaction_count(),
        decisions,
        applied_actions,
        skipped_actions,
    );
    audit.run_completed(ctx)?;
    Ok(report)
}

pub fn apply_action<C: SorxClient + ?Sized>(
    client: &C,
    ctx: &OperaxContext,
    action: &operax_core::ProposedAction,
) -> Result<()> {
    apply_pack_action(client, ctx, action, &default_metadata())
}

fn apply_pack_action<C: SorxClient + ?Sized>(
    client: &C,
    ctx: &OperaxContext,
    action: &operax_core::ProposedAction,
    metadata: &OperaHandoffMetadata,
) -> Result<()> {
    match action.operation {
        ActionOperation::DryRun => {
            if let Some(call) = action_to_business_call(action)? {
                client.dry_run_business_action(ctx, call)?;
            }
        }
        ActionOperation::Invoke => {
            let capability_client = SorxCapabilityClient::new(client);
            let declared_capability =
                declared_business_action_capability(metadata, &action.sorx_target);
            if invoke_action_capability(&capability_client, ctx, action, declared_capability)?
                .is_none()
                && let Some(route) = action_to_generated_route(action)?
            {
                client.invoke_generated_route(ctx, route)?;
            }
        }
    }
    Ok(())
}

fn declared_business_action_capability<'a>(
    metadata: &'a OperaHandoffMetadata,
    target: &operax_core::SorxTarget,
) -> Option<&'a str> {
    let operax_core::SorxTarget::BusinessAction { id, .. } = target else {
        return None;
    };
    metadata.business_function_capability_for_action(id)
}

fn default_metadata() -> OperaHandoffMetadata {
    OperaHandoffMetadata {
        schema: "greentic.operala.handoff.v1".into(),
        capability: "unknown".into(),
        extension: "unknown".into(),
        tenant_required: false,
        team_optional: false,
        team_required: false,
        sorla: None,
        sorx: None,
        requires: Vec::new(),
        consumes: Vec::new(),
    }
}

fn reject_secret_like_input(input: &Value) -> Result<()> {
    if operax_core::looks_secret_like(input) {
        return Err(OperaxError::new(
            "secret_like_value",
            "uploaded input contains a secret-like value",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::{CapabilityRequirement, EventSubscription, HandoffSorx};
    use operax_sorx_http::MockSorxClient;

    #[test]
    fn rejects_secret_like_input() {
        let err = reject_secret_like_input(&serde_json::json!({
            "transaction_id": "x",
            "booked_at": "2026-06-02",
            "amount": 1.0,
            "currency": "GBP",
            "reference": "TEN-001",
            "client_secret": "secret"
        }))
        .unwrap_err();
        assert_eq!(err.code, "secret_like_value");
    }

    #[test]
    fn applies_business_action_through_client() {
        let client = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let action = operax_core::ProposedAction {
            sorx_target: operax_core::SorxTarget::BusinessAction {
                id: "record_rent_payment".into(),
                version: "0.1.0".into(),
                contract_hash: "sha256:x".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };
        apply_action(&client, &ctx, &action).unwrap();
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            ["invoke_business_action:record_rent_payment"]
        );
    }

    #[test]
    fn generated_route_fallback_still_uses_sorx_client() {
        let client = MockSorxClient::default();
        let ctx = OperaxContext::new("demo".into(), None, None, "sha256:x".into()).unwrap();
        let action = operax_core::ProposedAction {
            sorx_target: operax_core::SorxTarget::GeneratedRoute {
                endpoint_id: "reconciliation_case.create".into(),
                operation_id: None,
                method: "POST".into(),
                path: "/v1/agent/reconciliation-cases/create".into(),
            },
            operation: ActionOperation::Invoke,
            values: serde_json::Map::new(),
            idempotency_key: None,
        };
        apply_action(&client, &ctx, &action).unwrap();
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            ["invoke_generated_route:/v1/agent/reconciliation-cases/create"]
        );
    }

    #[test]
    fn declared_capability_is_discovered_for_business_action() {
        let metadata = OperaHandoffMetadata {
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
            requires: vec![CapabilityRequirement {
                id: "operax.needs.record-rent-payment".into(),
                capability: "cap://greentic/sorx/tenancy/v1/functions/record-rent-payment".into(),
                metadata: serde_json::Map::from_iter([
                    ("kind".into(), serde_json::json!("business_function")),
                    ("action_id".into(), serde_json::json!("record_rent_payment")),
                ]),
            }],
            consumes: Vec::<EventSubscription>::new(),
        };
        let target = operax_core::SorxTarget::BusinessAction {
            id: "record_rent_payment".into(),
            version: "0.1.0".into(),
            contract_hash: "sha256:x".into(),
        };

        assert_eq!(
            declared_business_action_capability(&metadata, &target),
            Some("cap://greentic/sorx/tenancy/v1/functions/record-rent-payment")
        );
    }
}
