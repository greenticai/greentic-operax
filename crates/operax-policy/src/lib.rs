use operax_core::{DecisionEnvelope, OperaxError, Result};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    AutoApply,
    RequiresApproval,
    Denied,
}

pub fn decide(decision: &DecisionEnvelope) -> Result<PolicyOutcome> {
    if decision.confidence < 0.0 || decision.confidence > 100.0 {
        return Err(OperaxError::new(
            "invalid_confidence",
            "decision confidence must be between 0 and 100",
        ));
    }
    if decision.requires_approval {
        return Ok(PolicyOutcome::RequiresApproval);
    }
    if decision.outcome == "matched" && decision.confidence >= 85.0 {
        return Ok(PolicyOutcome::AutoApply);
    }
    if decision.outcome == "duplicate_possible" {
        return Ok(PolicyOutcome::RequiresApproval);
    }
    Ok(PolicyOutcome::RequiresApproval)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_match_auto_applies() {
        let decision = DecisionEnvelope {
            schema: "greentic.operax.decision.v1".into(),
            tenant: "demo".into(),
            team: None,
            capability: "reconciliation".into(),
            input_id: "tx".into(),
            outcome: "matched".into(),
            confidence: 95.0,
            matched_records: Vec::new(),
            proposed_actions: Vec::new(),
            requires_approval: false,
            reasons: Vec::new(),
            audit: None,
        };
        assert_eq!(decide(&decision).unwrap(), PolicyOutcome::AutoApply);
    }
}
