# operax-event-bridge

NATS event bridge for the Greentic OperaX runtime.

Consumes `greentic.operala.request.v1` messages from NATS, invokes the local
OperaX runtime through the [`OperaxInvoker`] seam, and publishes
`greentic.operala.response.v1` echoing the correlation id.

This mirrors the `sorx-event-bridge` crate in shape so that a flow's
`operala.call` node can dispatch work to OperaX over NATS pub/sub in the same
way `sorla.call` dispatches to SORX.
