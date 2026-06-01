use operax_core::{OperationalPack, OperaxContext, OperaxError, Result, RunReport};
use operax_sorx_http::{BusinessActionCall, GeneratedRouteCall, SorxClient, SorxRoute};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};

#[derive(Debug, Clone)]
struct BulkBindings {
    record_collections: BTreeMap<String, String>,
    actions: BTreeMap<String, String>,
    expected_counts: BTreeMap<String, usize>,
    validation: ValidationOptions,
}

#[derive(Debug, Default, Clone)]
struct ValidationOptions {
    atomic: bool,
    require_unique_ids: bool,
    validate_references: bool,
}

#[derive(Debug, Clone)]
struct Batch {
    batch_id: String,
    records: BTreeMap<String, Vec<Map<String, Value>>>,
}

#[derive(Debug, Clone)]
struct IdInfo {
    primary_field: String,
    id: String,
}

#[derive(Debug, Clone)]
struct PlannedOperation {
    op_id: String,
    collection: String,
    record_name: String,
    action: String,
    record: Map<String, Value>,
    dependency_ids: Vec<String>,
}

struct ReportData {
    status: &'static str,
    dry_run: bool,
    batch_id: Option<String>,
    planned_counts: Map<String, Value>,
    validation_errors: Vec<Value>,
    plan: Vec<PlannedOperation>,
    created_counts: Option<Map<String, Value>>,
    completed_operations: Option<Vec<Value>>,
    failed_operation: Option<Value>,
}

pub fn run_bulk_ingest<C: SorxClient + ?Sized>(
    pack: &OperationalPack,
    ctx: &OperaxContext,
    input_json: Value,
    cli_dry_run: bool,
    client: &C,
) -> Result<RunReport> {
    let mut errors = Vec::new();
    let bindings = parse_bindings(pack)?;

    if let Some(schema) = bulk_upload_schema(pack) {
        validate_against_schema(schema, &input_json, &mut errors);
    }

    let batch = match parse_batch(input_json) {
        Ok(batch) => batch,
        Err(error) => {
            errors.push(json!({"code": error.code, "message": error.message}));
            return Ok(report(ReportData {
                status: "validation_failed",
                dry_run: cli_dry_run,
                batch_id: None,
                planned_counts: Map::new(),
                validation_errors: errors,
                plan: Vec::new(),
                created_counts: None,
                completed_operations: None,
                failed_operation: None,
            }));
        }
    };

    validate_collections(&bindings, &batch, &mut errors);
    validate_expected_counts(&bindings, &batch, &mut errors);
    let id_index = validate_ids(&bindings, &batch, &mut errors);
    validate_references(&bindings, &batch, &id_index, &mut errors);
    let plan = build_plan(&bindings, &batch, &id_index, &mut errors);
    let plan = sort_plan(plan, &id_index, &mut errors);
    let planned_counts = planned_counts(&batch);
    let dry_run = cli_dry_run;
    let atomic = bindings.validation.atomic;

    if !errors.is_empty() {
        return Ok(report(ReportData {
            status: "validation_failed",
            dry_run,
            batch_id: Some(batch.batch_id),
            planned_counts,
            validation_errors: errors,
            plan,
            created_counts: None,
            completed_operations: None,
            failed_operation: None,
        }));
    }

    if dry_run {
        return Ok(report(ReportData {
            status: "planned",
            dry_run: true,
            batch_id: Some(batch.batch_id),
            planned_counts,
            validation_errors: Vec::new(),
            plan,
            created_counts: None,
            completed_operations: None,
            failed_operation: None,
        }));
    }

    execute_plan(ctx, client, batch.batch_id, planned_counts, plan, atomic)
}

fn parse_bindings(pack: &OperationalPack) -> Result<BulkBindings> {
    let bindings = pack
        .handoff
        .get("bindings")
        .and_then(Value::as_object)
        .ok_or_else(|| {
            OperaxError::new(
                "missing_bulk_bindings",
                "bulk_ingest handoff is missing bindings",
            )
        })?;
    Ok(BulkBindings {
        record_collections: string_map(bindings.get("record_collections"))?,
        actions: string_map(bindings.get("actions"))?,
        expected_counts: count_map(bindings.get("expected_counts"))?,
        validation: validation_options(bindings.get("validation")),
    })
}

fn parse_batch(input: Value) -> Result<Batch> {
    let object = input
        .as_object()
        .ok_or_else(|| OperaxError::new("invalid_batch_input", "batch input must be an object"))?;
    let batch_id = object
        .get("batch_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            OperaxError::new("invalid_batch_input", "batch_id must be a non-empty string")
        })?
        .to_string();
    let records = object
        .get("records")
        .and_then(Value::as_object)
        .ok_or_else(|| OperaxError::new("invalid_batch_input", "records must be an object"))?;

    let mut parsed = BTreeMap::new();
    for (collection, value) in records {
        let items = value.as_array().ok_or_else(|| {
            OperaxError::new(
                "invalid_batch_input",
                format!("records.{collection} must be an array"),
            )
        })?;
        let mut parsed_items = Vec::new();
        for item in items {
            let object = item.as_object().ok_or_else(|| {
                OperaxError::new(
                    "invalid_batch_input",
                    format!("records.{collection} items must be objects"),
                )
            })?;
            parsed_items.push(object.clone());
        }
        parsed.insert(collection.clone(), parsed_items);
    }

    Ok(Batch {
        batch_id,
        records: parsed,
    })
}

fn validate_collections(bindings: &BulkBindings, batch: &Batch, errors: &mut Vec<Value>) {
    for collection in bindings.record_collections.keys() {
        if !batch.records.contains_key(collection) {
            errors.push(json!({
                "code": "missing_collection",
                "collection": collection,
            }));
        }
    }
    for collection in batch.records.keys() {
        if !bindings.record_collections.contains_key(collection) {
            errors.push(json!({
                "code": "unknown_collection",
                "collection": collection,
            }));
        }
    }
}

fn validate_expected_counts(bindings: &BulkBindings, batch: &Batch, errors: &mut Vec<Value>) {
    let record_to_collection = record_to_collection(bindings);
    for (record_name, expected) in &bindings.expected_counts {
        match record_to_collection.get(record_name) {
            Some(collection) => {
                let actual = batch.records.get(collection).map(Vec::len).unwrap_or(0);
                if actual != *expected {
                    errors.push(json!({
                        "code": "expected_count_mismatch",
                        "record_name": record_name,
                        "collection": collection,
                        "expected": expected,
                        "actual": actual,
                    }));
                }
            }
            None => errors.push(json!({
                "code": "expected_count_unknown_record",
                "record_name": record_name,
                "expected": expected,
            })),
        }
    }
}

fn validate_ids(
    bindings: &BulkBindings,
    batch: &Batch,
    errors: &mut Vec<Value>,
) -> HashMap<(String, usize), IdInfo> {
    let mut by_record = HashMap::new();
    let mut seen_in_collection: HashMap<(String, String), usize> = HashMap::new();
    let mut seen_global: HashMap<String, String> = HashMap::new();

    for (collection, records) in &batch.records {
        let Some(record_name) = bindings.record_collections.get(collection) else {
            continue;
        };
        for (index, record) in records.iter().enumerate() {
            let Some((field, value)) = primary_id(collection, record_name, record) else {
                if bindings.validation.require_unique_ids || bindings.validation.validate_references
                {
                    errors.push(json!({
                        "code": "missing_primary_id",
                        "collection": collection,
                        "index": index,
                    }));
                }
                continue;
            };
            if seen_in_collection
                .insert((collection.clone(), value.clone()), index)
                .is_some()
            {
                errors.push(json!({
                    "code": "duplicate_id_in_collection",
                    "collection": collection,
                    "index": index,
                    "id": value,
                }));
            }
            if let Some(first_collection) = seen_global.insert(value.clone(), collection.clone())
                && first_collection != *collection
            {
                errors.push(json!({
                    "code": "duplicate_id_across_collections",
                    "collection": collection,
                    "other_collection": first_collection,
                    "index": index,
                    "id": value,
                }));
            }
            by_record.insert(
                (collection.clone(), index),
                IdInfo {
                    primary_field: field,
                    id: value,
                },
            );
        }
    }
    by_record
}

fn validate_references(
    bindings: &BulkBindings,
    batch: &Batch,
    id_index: &HashMap<(String, usize), IdInfo>,
    errors: &mut Vec<Value>,
) {
    if !bindings.validation.validate_references {
        return;
    }
    let ids_by_collection = ids_by_collection(id_index);
    for (collection, records) in &batch.records {
        let Some(record_name) = bindings.record_collections.get(collection) else {
            continue;
        };
        for (index, record) in records.iter().enumerate() {
            let primary_field = id_index
                .get(&(collection.clone(), index))
                .map(|info| info.primary_field.clone())
                .or_else(|| primary_id(collection, record_name, record).map(|(field, _)| field));
            for (field, value) in record {
                if !field.ends_with("_id") || Some(field.as_str()) == primary_field.as_deref() {
                    continue;
                }
                let Some(target_collection) = candidate_target_collection(bindings, field) else {
                    continue;
                };
                let Some(referenced_id) = value.as_str().map(str::to_string).or_else(|| {
                    if value.is_number() || value.is_boolean() {
                        Some(value.to_string())
                    } else {
                        None
                    }
                }) else {
                    continue;
                };
                if !ids_by_collection
                    .get(&target_collection)
                    .is_some_and(|ids| ids.contains(&referenced_id))
                {
                    errors.push(json!({
                        "code": "missing_internal_reference",
                        "collection": collection,
                        "index": index,
                        "field": field,
                        "referenced_id": referenced_id,
                        "candidate_target": target_collection,
                    }));
                }
            }
        }
    }
}

fn build_plan(
    bindings: &BulkBindings,
    batch: &Batch,
    id_index: &HashMap<(String, usize), IdInfo>,
    errors: &mut Vec<Value>,
) -> Vec<PlannedOperation> {
    let mut plan = Vec::new();
    for (collection, records) in &batch.records {
        let Some(record_name) = bindings.record_collections.get(collection) else {
            continue;
        };
        let Some(action) = action_for_collection(bindings, collection, record_name, records) else {
            errors.push(json!({
                "code": "unsupported_collection_action",
                "collection": collection,
                "record_name": record_name,
            }));
            continue;
        };
        for (index, record) in records.iter().enumerate() {
            let dependency_ids = dependency_ids(bindings, collection, record_name, record);
            let op_id = id_index
                .get(&(collection.clone(), index))
                .map(|id| format!("{collection}:{}", id.id))
                .unwrap_or_else(|| format!("{collection}:{index}"));
            plan.push(PlannedOperation {
                op_id,
                collection: collection.clone(),
                record_name: record_name.clone(),
                action: action.clone(),
                record: record.clone(),
                dependency_ids,
            });
        }
    }
    plan
}

fn sort_plan(
    plan: Vec<PlannedOperation>,
    id_index: &HashMap<(String, usize), IdInfo>,
    errors: &mut Vec<Value>,
) -> Vec<PlannedOperation> {
    let known_ids = id_index
        .values()
        .map(|info| info.id.clone())
        .collect::<BTreeSet<_>>();
    let op_by_id = plan
        .iter()
        .filter_map(|op| {
            op.op_id
                .split_once(':')
                .map(|(_, id)| (id.to_string(), op.op_id.clone()))
        })
        .collect::<HashMap<_, _>>();
    let mut indegree = plan
        .iter()
        .map(|op| (op.op_id.clone(), 0usize))
        .collect::<HashMap<_, _>>();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for op in &plan {
        for dependency_id in &op.dependency_ids {
            if !known_ids.contains(dependency_id) {
                continue;
            }
            if let Some(dep_op) = op_by_id.get(dependency_id) {
                *indegree.entry(op.op_id.clone()).or_default() += 1;
                dependents
                    .entry(dep_op.clone())
                    .or_default()
                    .push(op.op_id.clone());
            }
        }
    }

    let plan_by_op = plan
        .into_iter()
        .map(|op| (op.op_id.clone(), op))
        .collect::<BTreeMap<_, _>>();
    let mut queue = plan_by_op
        .keys()
        .filter(|op_id| indegree.get(*op_id).copied().unwrap_or(0) == 0)
        .cloned()
        .collect::<VecDeque<_>>();
    let mut sorted = Vec::new();

    while let Some(op_id) = queue.pop_front() {
        if let Some(op) = plan_by_op.get(&op_id) {
            sorted.push(op.clone());
        }
        if let Some(children) = dependents.get(&op_id) {
            let mut children = children.clone();
            children.sort();
            for child in children {
                if let Some(count) = indegree.get_mut(&child) {
                    *count -= 1;
                    if *count == 0 {
                        queue.push_back(child);
                    }
                }
            }
        }
    }

    if sorted.len() != plan_by_op.len() {
        let cycle = plan_by_op
            .keys()
            .filter(|op_id| !sorted.iter().any(|op| &op.op_id == *op_id))
            .cloned()
            .collect::<Vec<_>>();
        errors.push(json!({
            "code": "dependency_cycle",
            "operations": cycle,
        }));
    }
    sorted
}

fn execute_plan<C: SorxClient + ?Sized>(
    ctx: &OperaxContext,
    client: &C,
    batch_id: String,
    planned_counts: Map<String, Value>,
    plan: Vec<PlannedOperation>,
    atomic: bool,
) -> Result<RunReport> {
    let mut created_counts = Map::new();
    let mut completed = Vec::new();
    let mut responses = Vec::new();
    let routes = client.routes(ctx).unwrap_or_default();

    for op in &plan {
        let response = match invoke_operation(ctx, client, op, &batch_id, &routes) {
            Ok(response) => response,
            Err(error) => {
                let failed = planned_operation_value(op);
                return Ok(report(ReportData {
                    status: "partial_failure",
                    dry_run: false,
                    batch_id: Some(batch_id),
                    planned_counts,
                    validation_errors: Vec::new(),
                    plan,
                    created_counts: Some(created_counts),
                    completed_operations: Some(completed),
                    failed_operation: Some(json!({
                        "operation": failed,
                        "error": {"code": error.code, "message": error.message},
                        "atomic_requested": atomic,
                        "rollback_performed": false,
                    })),
                }));
            }
        };
        *created_counts
            .entry(op.record_name.clone())
            .or_insert_with(|| json!(0)) = json!(
            created_counts
                .get(&op.record_name)
                .and_then(Value::as_u64)
                .unwrap_or(0)
                + 1
        );
        completed.push(planned_operation_value(op));
        responses.push(response);
    }

    let mut report = report(ReportData {
        status: "completed",
        dry_run: false,
        batch_id: Some(batch_id),
        planned_counts,
        validation_errors: Vec::new(),
        plan,
        created_counts: Some(created_counts),
        completed_operations: Some(completed),
        failed_operation: None,
    });
    report.sorx_responses = Some(responses);
    Ok(report)
}

fn invoke_operation<C: SorxClient + ?Sized>(
    ctx: &OperaxContext,
    client: &C,
    op: &PlannedOperation,
    batch_id: &str,
    routes: &[SorxRoute],
) -> Result<Value> {
    let idempotency_key = Some(format!("{batch_id}:{}", op.op_id));
    if let Some(route) = route_for_action(routes, &op.action) {
        return client.invoke_generated_route(
            ctx,
            GeneratedRouteCall {
                method: route.method.clone(),
                path: route.path.clone(),
                values: Value::Object(op.record.clone()),
                idempotency_key,
            },
        );
    }
    client.invoke_business_action(
        ctx,
        BusinessActionCall {
            id: op.action.clone(),
            version: "0.1.0".to_string(),
            contract_hash:
                "sha256:0000000000000000000000000000000000000000000000000000000000000000"
                    .to_string(),
            values: Value::Object(op.record.clone()),
            idempotency_key,
        },
    )
}

fn route_for_action<'a>(routes: &'a [SorxRoute], action: &str) -> Option<&'a SorxRoute> {
    routes
        .iter()
        .find(|route| route.endpoint_id == action || route.operation_id.as_deref() == Some(action))
}

fn report(data: ReportData) -> RunReport {
    RunReport {
        status: Some(data.status.to_string()),
        input_count: data
            .planned_counts
            .values()
            .filter_map(Value::as_u64)
            .sum::<u64>() as usize,
        decisions: Vec::new(),
        applied_actions: data
            .completed_operations
            .as_ref()
            .map(Vec::len)
            .unwrap_or(0),
        skipped_actions: if data.dry_run { data.plan.len() } else { 0 },
        dry_run: Some(data.dry_run),
        batch_id: data.batch_id,
        planned_counts: Some(data.planned_counts),
        validation_errors: data.validation_errors,
        planned_operations: Some(data.plan.iter().map(planned_operation_value).collect()),
        created_counts: data.created_counts,
        completed_operations: data.completed_operations,
        failed_operation: data.failed_operation,
        sorx_responses: None,
    }
}

fn planned_counts(batch: &Batch) -> Map<String, Value> {
    batch
        .records
        .iter()
        .map(|(collection, records)| (collection.clone(), json!(records.len())))
        .collect()
}

fn planned_operation_value(op: &PlannedOperation) -> Value {
    json!({
        "operation_id": op.op_id,
        "collection": op.collection,
        "record_name": op.record_name,
        "action": op.action,
        "record": op.record,
        "dependency_ids": op.dependency_ids,
    })
}

fn string_map(value: Option<&Value>) -> Result<BTreeMap<String, String>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        OperaxError::new("invalid_bulk_bindings", "binding map must be an object")
    })?;
    object
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                value
                    .as_str()
                    .ok_or_else(|| {
                        OperaxError::new(
                            "invalid_bulk_bindings",
                            format!("binding value for {key} must be a string"),
                        )
                    })?
                    .to_string(),
            ))
        })
        .collect()
}

fn count_map(value: Option<&Value>) -> Result<BTreeMap<String, usize>> {
    let Some(value) = value else {
        return Ok(BTreeMap::new());
    };
    let object = value.as_object().ok_or_else(|| {
        OperaxError::new("invalid_bulk_bindings", "expected_counts must be an object")
    })?;
    object
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                value.as_u64().ok_or_else(|| {
                    OperaxError::new(
                        "invalid_bulk_bindings",
                        format!("expected count for {key} must be an integer"),
                    )
                })? as usize,
            ))
        })
        .collect()
}

fn validation_options(value: Option<&Value>) -> ValidationOptions {
    let object = value.and_then(Value::as_object);
    ValidationOptions {
        atomic: bool_field(object, "atomic"),
        require_unique_ids: bool_field(object, "require_unique_ids"),
        validate_references: bool_field(object, "validate_references"),
    }
}

fn bool_field(object: Option<&Map<String, Value>>, key: &str) -> bool {
    object
        .and_then(|object| object.get(key))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn record_to_collection(bindings: &BulkBindings) -> BTreeMap<String, String> {
    bindings
        .record_collections
        .iter()
        .map(|(collection, record)| (record.clone(), collection.clone()))
        .collect()
}

fn primary_id(
    collection: &str,
    record_name: &str,
    record: &Map<String, Value>,
) -> Option<(String, String)> {
    let candidates = [
        "id".to_string(),
        format!("{collection}_id"),
        format!("{}_id", snake_case(record_name)),
    ];
    candidates.iter().find_map(|field| {
        record
            .get(field)
            .and_then(|value| {
                value.as_str().map(str::to_string).or_else(|| {
                    if value.is_number() || value.is_boolean() {
                        Some(value.to_string())
                    } else {
                        None
                    }
                })
            })
            .map(|id| (field.clone(), id))
    })
}

fn ids_by_collection(
    id_index: &HashMap<(String, usize), IdInfo>,
) -> HashMap<String, BTreeSet<String>> {
    let mut out: HashMap<String, BTreeSet<String>> = HashMap::new();
    for ((collection, _), info) in id_index {
        out.entry(collection.clone())
            .or_default()
            .insert(info.id.clone());
    }
    out
}

fn candidate_target_collection(bindings: &BulkBindings, field: &str) -> Option<String> {
    let prefix = field.strip_suffix("_id")?;
    if bindings.record_collections.contains_key(prefix) {
        return Some(prefix.to_string());
    }
    bindings
        .record_collections
        .iter()
        .find(|(_, record_name)| snake_case(record_name) == prefix)
        .map(|(collection, _)| collection.clone())
}

fn dependency_ids(
    bindings: &BulkBindings,
    collection: &str,
    record_name: &str,
    record: &Map<String, Value>,
) -> Vec<String> {
    let primary_field = primary_id(collection, record_name, record).map(|(field, _)| field);
    let mut out = record
        .iter()
        .filter_map(|(field, value)| {
            if !field.ends_with("_id") || Some(field.as_str()) == primary_field.as_deref() {
                return None;
            }
            candidate_target_collection(bindings, field)?;
            value.as_str().map(str::to_string).or_else(|| {
                if value.is_number() || value.is_boolean() {
                    Some(value.to_string())
                } else {
                    None
                }
            })
        })
        .collect::<Vec<_>>();
    out.sort();
    out.dedup();
    out
}

fn action_for_collection(
    bindings: &BulkBindings,
    collection: &str,
    record_name: &str,
    records: &[Map<String, Value>],
) -> Option<String> {
    let fallback = best_token_matched_action(bindings, collection, record_name, records);
    let direct = [
        format!("add_{collection}"),
        format!("record_{collection}"),
        format!("upsert_{collection}"),
        format!("create_{collection}"),
        collection.to_string(),
    ]
    .iter()
    .find(|key| bindings.actions.contains_key(*key))
    .cloned();

    match (direct, fallback) {
        (Some(direct), Some(fallback))
            if bindings.actions.get(&direct) == bindings.actions.get(&fallback)
                && direct.starts_with("create_")
                && !fallback.starts_with("create_") =>
        {
            Some(fallback)
        }
        (Some(direct), _) => Some(direct),
        (None, fallback) => fallback,
    }
}

fn best_token_matched_action(
    bindings: &BulkBindings,
    collection: &str,
    record_name: &str,
    records: &[Map<String, Value>],
) -> Option<String> {
    let direct_keys = bindings
        .record_collections
        .keys()
        .flat_map(|collection| {
            [
                format!("create_{collection}"),
                format!("record_{collection}"),
                format!("add_{collection}"),
                format!("upsert_{collection}"),
                collection.to_string(),
            ]
        })
        .collect::<BTreeSet<_>>();
    let wanted = action_tokens(collection, record_name, records);
    bindings
        .actions
        .iter()
        .filter(|(key, _)| !direct_keys.contains(*key))
        .filter_map(|(key, action)| {
            let score = tokenize(key)
                .into_iter()
                .filter(|token| wanted.contains(token))
                .count();
            (score > 0).then(|| (score, key.clone(), action.clone()))
        })
        .max_by(|left, right| left.0.cmp(&right.0).then_with(|| right.1.cmp(&left.1)))
        .map(|(_, key, _)| key)
}

fn action_tokens(
    collection: &str,
    record_name: &str,
    records: &[Map<String, Value>],
) -> BTreeSet<String> {
    let mut out = tokenize(collection);
    out.extend(tokenize(record_name));
    for record in records {
        for field in record.keys().filter(|field| field.ends_with("_id")) {
            if let Some(prefix) = field.strip_suffix("_id") {
                out.extend(tokenize(prefix));
            }
        }
    }
    out
}

fn tokenize(input: &str) -> BTreeSet<String> {
    snake_case(input)
        .split('_')
        .filter(|token| !token.is_empty())
        .map(str::to_string)
        .collect()
}

fn snake_case(input: &str) -> String {
    let mut out = String::new();
    let mut previous_was_lower_or_digit = false;
    for character in input.chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase() && previous_was_lower_or_digit {
                out.push('_');
            }
            out.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
        } else {
            if !out.ends_with('_') && !out.is_empty() {
                out.push('_');
            }
            previous_was_lower_or_digit = false;
        }
    }
    out.trim_matches('_').to_string()
}

fn bulk_upload_schema(pack: &OperationalPack) -> Option<&Value> {
    pack.handoff
        .get("schemas")
        .and_then(|schemas| schemas.get("bulk-upload"))
        .or_else(|| pack.schema_documents.get("bulk-upload.schema.json"))
        .or_else(|| pack.schema_documents.get("bulk-upload"))
}

fn validate_against_schema(schema: &Value, input: &Value, errors: &mut Vec<Value>) {
    match jsonschema::validator_for(schema) {
        Ok(validator) => {
            for error in validator.iter_errors(input) {
                errors.push(json!({
                    "code": "schema_validation_failed",
                    "path": error.instance_path().to_string(),
                    "schema_path": error.schema_path().to_string(),
                    "message": error.to_string(),
                }));
            }
        }
        Err(error) => errors.push(json!({
            "code": "invalid_bulk_upload_schema",
            "message": error.to_string(),
        })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use operax_core::{HandoffSorx, OperaHandoffMetadata, SchemaRegistry, SorxBinding};
    use operax_sorx_http::MockSorxClient;

    fn pack() -> OperationalPack {
        OperationalPack {
            metadata: OperaHandoffMetadata {
                schema: "greentic.operala.handoff.v1".into(),
                capability: "bulk_ingest".into(),
                extension: "greentic.operala.bulk_ingest.v1".into(),
                tenant_required: true,
                team_optional: false,
                team_required: false,
                sorla: None,
                sorx: Some(HandoffSorx {
                    transport: "http".into(),
                    url: "runtime-provided".into(),
                }),
                requires: Vec::new(),
                consumes: Vec::new(),
            },
            handoff: json!({
                "schema": "greentic.operala.handoff.v1",
                "capability": "bulk_ingest",
                "extension": "greentic.operala.bulk_ingest.v1",
                "bindings": {
                    "name": "generic_bulk_upload",
                    "record_collections": {
                        "customer": "Customer",
                        "invoice": "Invoice"
                    },
                    "actions": {
                        "create_customer": "CreateCustomer",
                        "create_invoice": "CreateInvoice"
                    },
                    "expected_counts": {
                        "Customer": 1,
                        "Invoice": 1
                    },
                    "validation": {
                        "atomic": true,
                        "require_unique_ids": true,
                        "validate_references": true
                    }
                },
                "schemas": {
                    "bulk-upload": {
                        "type": "object",
                        "required": ["batch_id", "records"],
                        "properties": {
                            "batch_id": {"type": "string"},
                            "dry_run": {"type": "boolean"},
                            "records": {"type": "object"}
                        }
                    }
                }
            }),
            flows: Vec::new(),
            schemas: SchemaRegistry {
                names: vec!["bulk-upload.schema.json".into()],
            },
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

    fn ctx() -> OperaxContext {
        OperaxContext::new("demo".into(), None, None, "sha256:pack".into()).unwrap()
    }

    fn valid_batch(dry_run: bool) -> Value {
        json!({
            "batch_id": "bulk_001",
            "dry_run": dry_run,
            "records": {
                "invoice": [{"id": "i1", "customer_id": "c1", "total": 10}],
                "customer": [{"id": "c1", "name": "Ada"}]
            }
        })
    }

    fn codes(report: &RunReport) -> Vec<String> {
        report
            .validation_errors
            .iter()
            .filter_map(|error| {
                error
                    .get("code")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect()
    }

    #[test]
    fn reports_expected_count_mismatch() {
        let client = MockSorxClient::default();
        let input = json!({
            "batch_id": "bulk_001",
            "records": {
                "customer": [],
                "invoice": [{"id": "i1", "customer_id": "c1"}]
            }
        });
        let report = run_bulk_ingest(&pack(), &ctx(), input, true, &client).unwrap();
        assert_eq!(report.status.as_deref(), Some("validation_failed"));
        assert!(codes(&report).contains(&"expected_count_mismatch".to_string()));
    }

    #[test]
    fn reports_duplicate_ids_within_and_across_collections() {
        let client = MockSorxClient::default();
        let input = json!({
            "batch_id": "bulk_001",
            "records": {
                "customer": [{"id": "same"}, {"id": "same"}],
                "invoice": [{"id": "same", "customer_id": "same"}]
            }
        });
        let report = run_bulk_ingest(&pack(), &ctx(), input, true, &client).unwrap();
        let codes = codes(&report);
        assert!(codes.contains(&"duplicate_id_in_collection".to_string()));
        assert!(codes.contains(&"duplicate_id_across_collections".to_string()));
    }

    #[test]
    fn reports_missing_internal_reference() {
        let client = MockSorxClient::default();
        let input = json!({
            "batch_id": "bulk_001",
            "records": {
                "customer": [{"id": "c1"}],
                "invoice": [{"id": "i1", "customer_id": "missing"}]
            }
        });
        let report = run_bulk_ingest(&pack(), &ctx(), input, true, &client).unwrap();
        assert!(codes(&report).contains(&"missing_internal_reference".to_string()));
    }

    #[test]
    fn ignores_payload_dry_run_without_cli_flag() {
        let client = MockSorxClient::default();
        let input = json!({
            "batch_id": "bulk_001",
            "dry_run": true,
            "records": {
                "customer": [{"id": "c1", "external_account_id": "acct_1"}],
                "invoice": [{"id": "i1", "customer_id": "c1"}]
            }
        });
        let report = run_bulk_ingest(&pack(), &ctx(), input, false, &client).unwrap();
        assert_eq!(report.status.as_deref(), Some("completed"));
        assert!(report.validation_errors.is_empty());
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            [
                "routes",
                "invoke_business_action:create_customer",
                "invoke_business_action:create_invoice"
            ]
        );
    }

    #[test]
    fn dry_run_returns_dependency_order_without_sorx_calls() {
        let client = MockSorxClient::default();
        let report = run_bulk_ingest(&pack(), &ctx(), valid_batch(false), true, &client).unwrap();
        assert_eq!(report.status.as_deref(), Some("planned"));
        let operations = report.planned_operations.unwrap();
        assert_eq!(operations[0]["collection"], "customer");
        assert_eq!(operations[1]["collection"], "invoice");
        assert!(client.calls.lock().unwrap().is_empty());
    }

    #[test]
    fn real_run_invokes_sorx_in_dependency_order() {
        let client = MockSorxClient::default();
        let report = run_bulk_ingest(&pack(), &ctx(), valid_batch(false), false, &client).unwrap();
        assert_eq!(report.status.as_deref(), Some("completed"));
        assert_eq!(
            client.calls.lock().unwrap().as_slice(),
            [
                "routes",
                "invoke_business_action:create_customer",
                "invoke_business_action:create_invoice"
            ]
        );
    }
}
