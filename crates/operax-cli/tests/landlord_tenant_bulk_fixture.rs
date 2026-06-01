use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

#[test]
fn landlord_tenant_bulk_fixture_feeds_non_zero_metric_inputs() {
    let workspace = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|path| path.parent())
        .expect("workspace root");
    let fixture_path = workspace.join("examples/bulk-ingest/landlord-tenant-batch.json");
    let fixture: Value =
        serde_json::from_slice(&std::fs::read(&fixture_path).expect("read bulk fixture"))
            .expect("parse bulk fixture");
    let records = fixture["records"].as_object().expect("records object");

    let buildings_with_units = records["unit"]
        .as_array()
        .expect("unit records")
        .iter()
        .filter_map(|unit| unit["building_id"].as_str())
        .collect::<BTreeSet<_>>();
    let active_tenancies_by_building = grouped_count(
        records["tenancy"].as_array().expect("tenancy records"),
        "building_id",
        |tenancy| tenancy["status"] == "active",
    );
    let monthly_revenue_by_tenancy = grouped_sum(
        records["payment"].as_array().expect("payment records"),
        "tenancy_id",
        "amount",
        |payment| payment["status"] == "settled",
    );
    let open_maintenance_by_building = grouped_count(
        records["maintenance_request"]
            .as_array()
            .expect("maintenance records"),
        "building_id",
        |request| request["status"] == "open",
    );

    assert!(
        active_tenancies_by_building
            .values()
            .any(|count| *count > 0),
        "expected active tenancy metric inputs"
    );
    assert!(
        monthly_revenue_by_tenancy
            .values()
            .any(|amount| *amount > 0.0),
        "expected settled payment metric inputs"
    );
    assert!(
        open_maintenance_by_building
            .values()
            .any(|count| *count > 0),
        "expected open maintenance metric inputs"
    );
    for building_id in active_tenancies_by_building.keys() {
        assert!(
            buildings_with_units.contains(building_id.as_str()),
            "active tenancy references building without units: {building_id}"
        );
    }
}

fn grouped_count(
    records: &[Value],
    group_field: &str,
    predicate: impl Fn(&Value) -> bool,
) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for record in records.iter().filter(|record| predicate(record)) {
        let group = record[group_field]
            .as_str()
            .unwrap_or_else(|| panic!("missing group field {group_field}"));
        *counts.entry(group.to_string()).or_insert(0) += 1;
    }
    counts
}

fn grouped_sum(
    records: &[Value],
    group_field: &str,
    sum_field: &str,
    predicate: impl Fn(&Value) -> bool,
) -> BTreeMap<String, f64> {
    let mut sums = BTreeMap::new();
    for record in records.iter().filter(|record| predicate(record)) {
        let group = record[group_field]
            .as_str()
            .unwrap_or_else(|| panic!("missing group field {group_field}"));
        let amount = record[sum_field]
            .as_f64()
            .unwrap_or_else(|| panic!("missing numeric sum field {sum_field}"));
        *sums.entry(group.to_string()).or_insert(0.0) += amount;
    }
    sums
}
