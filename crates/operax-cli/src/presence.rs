//! SoRX presence subscriber: the OperaX consumer half of NATS push-discovery
//! (#3). Mirrors `business_events::run_subscriber`'s shape (dedicated
//! current-thread tokio runtime, `async_nats::connect`, `client.subscribe(...)`,
//! malformed-skip logging), but for `SorxPresence` announcements published by
//! the SoRX producer (greentic-sorx PR #58) at boot on
//! `greentic.presence.<tenant>.sorx.<sor>`.
//!
//! Slice 1: subscribe, maintain an in-memory directory, and log it. Not
//! wired into `HttpSorxClient` yet, and eviction is not yet meaningful — the
//! producer only announces once at boot today, not on a heartbeat cadence.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use futures::StreamExt;
use serde::Deserialize;

/// Local mirror of SoRX's `SorxPresence` wire shape (greentic-sorx PR #58).
/// Kept local rather than depending on `greentic-sorx-core`: this slice only
/// routes and displays presence, so `offers` is left untyped.
#[derive(Debug, Clone, Deserialize)]
pub struct SorxPresence {
    pub schema: String,
    pub instance_id: String,
    pub tenant: String,
    pub environment: String,
    pub sor: String,
    pub pack_version: String,
    pub base_url: String,
    pub reachable: bool,
    pub offers: serde_json::Value,
    pub ts: String,
}

/// A discovered SoR instance plus the tick (seconds) it was last announced at.
#[derive(Debug, Clone)]
pub struct DirectoryEntry {
    pub presence: SorxPresence,
    pub last_seen: u64,
}

/// In-memory directory of discovered SoR instances, keyed by `instance_id`.
pub type Directory = HashMap<String, DirectoryEntry>;

/// Default staleness window before a directory entry would be considered
/// stale, once the producer sends heartbeats (see `evict_stale`).
const PRESENCE_TTL_SECS: u64 = 300;

/// Upserts `presence` into `dir` keyed by `instance_id`, refreshing
/// `last_seen`. Pure: no I/O.
pub fn apply_presence(dir: &mut Directory, presence: SorxPresence, now: u64) {
    dir.insert(
        presence.instance_id.clone(),
        DirectoryEntry {
            presence,
            last_seen: now,
        },
    );
}

/// Resolve the freshest reachable SoRX `base_url` for a (tenant, sor) pair.
/// Pure: no I/O.
// Consumed by `PresenceResolver` in Task 8; unused in the non-test lib build until then.
#[allow(dead_code)]
pub fn resolve_endpoint(dir: &Directory, tenant: &str, sor: &str) -> Option<String> {
    dir.values()
        .filter(|e| e.presence.reachable && e.presence.tenant == tenant && e.presence.sor == sor)
        .max_by_key(|e| e.last_seen)
        .map(|e| e.presence.base_url.clone())
}

/// Removes entries whose age (`now - last_seen`) exceeds `ttl`. Pure: no I/O.
///
/// Only meaningful once the producer sends heartbeats; today's boot-only
/// announcement means an evicted instance won't reappear until it restarts.
/// Exercised per-message in [`run_presence_subscriber`] for slice 1 anyway,
/// so the directory self-heals automatically once heartbeats land.
pub fn evict_stale(dir: &mut Directory, now: u64, ttl: u64) {
    dir.retain(|_, entry| now.saturating_sub(entry.last_seen) <= ttl);
}

/// Decodes a `SorxPresence` from raw NATS payload bytes. Never panics;
/// malformed input is surfaced as an `Err` for the caller to skip-and-log.
fn decode_presence(payload: &[u8]) -> Result<SorxPresence, serde_json::Error> {
    serde_json::from_slice(payload)
}

/// Configuration for [`run_presence_subscriber`]: the NATS endpoint to
/// connect to and an optional tenant to scope the subscription subject.
pub struct PresenceSubscriberConfig {
    pub nats_url: String,
    pub tenant: Option<String>,
}

/// Current tick (Unix seconds) used to timestamp directory entries. Falls
/// back to `0` rather than panicking if the clock is somehow before the
/// epoch.
fn now_ticks() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

fn log_directory(dir: &Directory) {
    eprintln!("presence directory ({} entries):", dir.len());
    for (instance_id, entry) in dir {
        let presence = &entry.presence;
        eprintln!(
            "  {instance_id}: schema={} tenant={} environment={} sor={} pack_version={} \
             base_url={} reachable={} offers={} ts={} last_seen={}",
            presence.schema,
            presence.tenant,
            presence.environment,
            presence.sor,
            presence.pack_version,
            presence.base_url,
            presence.reachable,
            presence.offers,
            presence.ts,
            entry.last_seen
        );
    }
}

/// Runs the NATS presence-subscription loop until the connection ends.
/// Blocking: spins its own current-thread tokio runtime, so the caller
/// should run this on a dedicated thread — mirrors
/// `business_events::run_subscriber`.
///
/// Not exercised by this task's unit tests (no live NATS broker available
/// here); a live-NATS integration test is deferred to a follow-up task.
/// Wired to the CLI via the `operax presence subscribe` subcommand
/// (`lib.rs::run_presence_subscribe`).
pub fn run_presence_subscriber(config: PresenceSubscriberConfig) -> anyhow::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        let client = async_nats::connect(&config.nats_url).await?;
        let subject = match &config.tenant {
            Some(tenant) => format!("greentic.presence.{tenant}.>"),
            None => "greentic.presence.>".to_string(),
        };
        let mut subscriber = client.subscribe(subject.clone()).await?;
        eprintln!("subscribed to {subject}");
        let mut directory: Directory = HashMap::new();
        while let Some(msg) = subscriber.next().await {
            let presence = match decode_presence(&msg.payload) {
                Ok(presence) => presence,
                Err(err) => {
                    eprintln!("skip undecodable presence on {}: {err}", msg.subject);
                    continue;
                }
            };
            let now = now_ticks();
            apply_presence(&mut directory, presence, now);
            evict_stale(&mut directory, now, PRESENCE_TTL_SECS);
            log_directory(&directory);
        }
        Ok::<(), anyhow::Error>(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn presence(instance_id: &str, sor: &str) -> SorxPresence {
        SorxPresence {
            schema: "sorx.presence.v1".to_string(),
            instance_id: instance_id.to_string(),
            tenant: "tenant-1".to_string(),
            environment: "prod".to_string(),
            sor: sor.to_string(),
            pack_version: "1.0.0".to_string(),
            base_url: "http://127.0.0.1:9000".to_string(),
            reachable: true,
            offers: serde_json::json!({}),
            ts: "2026-07-27T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn apply_presence_upserts_and_updates_last_seen() {
        let mut dir: Directory = HashMap::new();

        apply_presence(&mut dir, presence("inst-1", "landlord"), 10);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir["inst-1"].last_seen, 10);
        assert_eq!(dir["inst-1"].presence.sor, "landlord");

        // Re-announce with an updated field + later tick: upsert in place,
        // not a second entry.
        apply_presence(&mut dir, presence("inst-1", "landlord-v2"), 20);
        assert_eq!(dir.len(), 1);
        assert_eq!(dir["inst-1"].last_seen, 20);
        assert_eq!(dir["inst-1"].presence.sor, "landlord-v2");
    }

    #[test]
    fn evict_stale_drops_old_keeps_fresh() {
        let mut dir: Directory = HashMap::new();
        apply_presence(&mut dir, presence("stale", "a"), 0);
        apply_presence(&mut dir, presence("fresh", "b"), 90);

        evict_stale(&mut dir, 100, 50);

        assert!(!dir.contains_key("stale"));
        assert!(dir.contains_key("fresh"));
    }

    #[test]
    fn malformed_json_is_skipped() {
        assert!(decode_presence(b"not json").is_err());
    }

    #[test]
    fn resolve_picks_freshest_reachable() {
        fn pres(
            instance: &str,
            tenant: &str,
            sor: &str,
            url: &str,
            reachable: bool,
        ) -> SorxPresence {
            SorxPresence {
                schema: "greentic.sorx.presence.v1".into(),
                instance_id: instance.into(),
                tenant: tenant.into(),
                environment: "prod".into(),
                sor: sor.into(),
                pack_version: "1.0.0".into(),
                base_url: url.into(),
                reachable,
                offers: serde_json::Value::Null,
                ts: "t".into(),
            }
        }
        let mut dir = Directory::new();
        apply_presence(&mut dir, pres("i1", "t1", "orders", "http://old", true), 10);
        apply_presence(&mut dir, pres("i2", "t1", "orders", "http://new", true), 20);
        apply_presence(
            &mut dir,
            pres("i3", "t1", "orders", "http://down", false),
            30,
        );
        apply_presence(
            &mut dir,
            pres("i4", "t1", "billing", "http://other", true),
            40,
        );
        assert_eq!(
            resolve_endpoint(&dir, "t1", "orders").as_deref(),
            Some("http://new")
        );
        assert_eq!(resolve_endpoint(&dir, "t1", "unknown"), None);
        assert_eq!(resolve_endpoint(&dir, "t2", "orders"), None);
    }
}
