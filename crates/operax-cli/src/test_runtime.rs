mod answers;
mod cards;
mod hooks;
mod http;
mod i18n;
mod ids;
mod packing;

use answers::{WEBCHAT_REF, create_answers_value, setup_answers_value};
use cards::{
    collect_navigable_targets, normalize_card_for_webchat, placeholder_dashboard_card, welcome_card,
};
use hooks::patch_hooks_source;
use http::ParsedHttpUrl;
use i18n::write_card_i18n;
use ids::{route_card_id, sanitize_id};
use packing::{extract_zip_to_dir, pack_dir, write_json};

use greentic_pack::{SigningPolicy, open_pack};
use operax_manager::{ManagerOptions, ManagerRuntime, start_manager_server};
use operax_sorx_http::HttpSorxClient;
use serde_json::Value;
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug)]
pub struct TestOptions {
    pub artifact: PathBuf,
    pub tenant: String,
    pub team: Option<String>,
    pub sorx_url: String,
    pub operax_url: String,
    pub webchat_url: String,
    pub locale: String,
    pub audit_dir: Option<PathBuf>,
    pub bundle_dir: Option<PathBuf>,
    pub setup_answers: Option<PathBuf>,
    pub force: bool,
    pub no_start: bool,
    pub sorx_token_env: String,
}

struct TestContext {
    options: TestOptions,
    artifact_abs: PathBuf,
    artifact_base: String,
    pack_id: String,
    bundle_id: String,
    bundle_dir: PathBuf,
    work_dir: PathBuf,
    create_answers: PathBuf,
    setup_answers: PathBuf,
    operax_url: ParsedHttpUrl,
    webchat_url: ParsedHttpUrl,
}

pub fn run(options: TestOptions) -> Result<(), String> {
    let ctx = prepare_context(options)?;
    prepare_workspace(&ctx)?;
    print_summary(&ctx);
    write_create_answers(&ctx)?;
    run_command(
        Command::new("greentic-bundle")
            .arg("wizard")
            .arg("apply")
            .arg("--answers")
            .arg(&ctx.create_answers),
        "greentic-bundle wizard apply",
    )?;

    patch_webchat_manager_hook(&ctx)?;
    write_setup_answers_if_needed(&ctx)?;
    run_command(
        Command::new("gtc")
            .arg("setup")
            .arg(&ctx.bundle_dir)
            .arg("--no-ui")
            .arg("--non-interactive")
            .arg("--answers")
            .arg(&ctx.setup_answers),
        "gtc setup",
    )?;

    install_initial_cards(&ctx)?;

    println!();
    println!(
        "OperaX manager card endpoint: {}/v1/operax/manager/cards/dashboard",
        ctx.operax_url.base
    );
    println!(
        "OperaX flow endpoint:         {}/v1/operax/runs",
        ctx.operax_url.base
    );
    println!(
        "WebChat route:                {}/v1/web/webchat/demo/",
        ctx.webchat_url.base
    );
    println!("SORX runtime URL:             {}", ctx.options.sorx_url);
    println!();

    if ctx.options.no_start {
        println!("Bundle is ready at {}", ctx.bundle_dir.display());
        return Ok(());
    }

    ensure_port_available(&ctx.operax_url)?;
    let runtime = Arc::new(
        ManagerRuntime::load(
            ManagerOptions {
                artifact: ctx.artifact_abs.clone(),
                tenant: ctx.options.tenant.clone(),
                team: ctx.options.team.clone(),
                locale: Some(ctx.options.locale.clone()),
                bind: ctx.operax_url.bind_addr(),
                audit_dir: ctx.options.audit_dir.clone(),
            },
            Arc::new(HttpSorxClient::new(
                ctx.options.sorx_url.clone(),
                std::env::var(&ctx.options.sorx_token_env)
                    .ok()
                    .filter(|token| !token.is_empty()),
            )),
        )
        .map_err(|err| err.to_string())?,
    );
    let bind = ctx.operax_url.bind_addr();
    std::thread::spawn(move || {
        if let Err(err) = start_manager_server(runtime, &bind) {
            eprintln!("OperaX manager server stopped: {err}");
        }
    });
    wait_for_manager_card(&ctx, Duration::from_secs(45))?;
    refresh_live_cards(&ctx)?;

    println!("Starting WebChat bundle; press Ctrl-C to stop.");
    let status = Command::new("gtc")
        .arg("start")
        .arg(&ctx.bundle_dir)
        .status()
        .map_err(|err| format!("failed to start WebChat bundle: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("gtc start exited with status {status}"))
    }
}

fn prepare_context(options: TestOptions) -> Result<TestContext, String> {
    if !options.artifact.exists() {
        return Err(format!(
            "OperaX artifact not found: {}",
            options.artifact.display()
        ));
    }
    if options.locale.trim().is_empty() || options.webchat_url.trim().is_empty() {
        return Err("--webchat-url and --locale require non-empty values".to_string());
    }
    let requested_operax_url = ParsedHttpUrl::parse(&options.operax_url, "--operax-url")?;
    let operax_url = if options.no_start {
        requested_operax_url
    } else {
        first_available_http_url(requested_operax_url, "--operax-url")?
    };
    let requested_webchat_url = ParsedHttpUrl::parse(&options.webchat_url, "--webchat-url")?;
    let webchat_url = if options.no_start {
        requested_webchat_url
    } else {
        first_available_http_url(requested_webchat_url, "--webchat-url")?
    };
    let artifact_abs = fs::canonicalize(&options.artifact)
        .map_err(|err| format!("failed to resolve {}: {err}", options.artifact.display()))?;
    let artifact_base = artifact_abs
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| format!("invalid artifact path {}", artifact_abs.display()))?
        .to_string();
    let pack_id = sanitize_id(
        artifact_base
            .strip_suffix(".gtpack")
            .unwrap_or(&artifact_base),
    );
    let bundle_id = format!("operax-manager-{pack_id}");
    let bundle_dir = match &options.bundle_dir {
        Some(path) => absolutize(path)?,
        None => std::env::temp_dir().join(format!("{bundle_id}-bundle")),
    };
    let work_dir = bundle_dir.join(".test-operax");
    let create_answers = work_dir.join("create-answers.json");
    let setup_answers = options
        .setup_answers
        .clone()
        .unwrap_or_else(|| work_dir.join("setup-answers.json"));
    Ok(TestContext {
        options,
        artifact_abs,
        artifact_base,
        pack_id,
        bundle_id,
        bundle_dir,
        work_dir,
        create_answers,
        setup_answers,
        operax_url,
        webchat_url,
    })
}

fn prepare_workspace(ctx: &TestContext) -> Result<(), String> {
    let marker = ctx.work_dir.join("created-by-greentic-operax-test");
    if ctx.bundle_dir.exists() && !marker.is_file() && !ctx.options.force {
        return Err(format!(
            "bundle directory already exists and was not created by greentic-operax test: {}\npass --force to replace it",
            ctx.bundle_dir.display()
        ));
    }
    if ctx.bundle_dir.exists() {
        fs::remove_dir_all(&ctx.bundle_dir).map_err(|err| {
            format!(
                "failed to remove existing bundle directory {}: {err}",
                ctx.bundle_dir.display()
            )
        })?;
    }
    fs::create_dir_all(&ctx.work_dir)
        .map_err(|err| format!("failed to create {}: {err}", ctx.work_dir.display()))?;
    fs::write(marker, b"greentic-operax test\n").map_err(|err| {
        format!(
            "failed to write workspace marker in {}: {err}",
            ctx.work_dir.display()
        )
    })
}

fn print_summary(ctx: &TestContext) {
    println!("Preparing OperaX/WebChat test bundle");
    println!("  OperaX pack:      {}", ctx.artifact_abs.display());
    println!("  pack id:          {}", ctx.pack_id);
    println!("  bundle workspace: {}", ctx.bundle_dir.display());
    println!(
        "  manager card URL: {}/v1/operax/manager/cards/dashboard",
        ctx.operax_url.base
    );
    println!(
        "  WebChat URL:      {}/v1/web/webchat/demo/",
        ctx.webchat_url.base
    );
    println!("  SORX URL:         {}", ctx.options.sorx_url);
    println!(
        "  tenant/team:      {} / {}",
        ctx.options.tenant,
        ctx.options.team.as_deref().unwrap_or("(none)")
    );
    println!("  selected locale:  {}", ctx.options.locale);
    println!("  WebChat pack ref: {WEBCHAT_REF}");
}

fn write_create_answers(ctx: &TestContext) -> Result<(), String> {
    let value = create_answers_value(&ctx.options.locale, &ctx.bundle_id, &ctx.bundle_dir);
    write_json(&ctx.create_answers, &value)
}

fn write_setup_answers_if_needed(ctx: &TestContext) -> Result<(), String> {
    if ctx.options.setup_answers.is_some() {
        return Ok(());
    }
    let value = setup_answers_value(&ctx.webchat_url.base, &ctx.operax_url.base);
    write_json(&ctx.setup_answers, &value)
}

fn patch_webchat_manager_hook(ctx: &TestContext) -> Result<(), String> {
    let pack_path = ctx
        .bundle_dir
        .join("providers/messaging/messaging-webchat-gui.gtpack");
    validate_gtpack(&pack_path)?;
    let work_dir = ctx.work_dir.join("webchat-provider-pack");
    extract_zip_to_dir(&pack_path, &work_dir)?;
    let hooks_path = work_dir.join("assets/webchat-gui/skins/default/webchat/hooks.js");
    let source = fs::read_to_string(&hooks_path)
        .map_err(|err| format!("failed to read {}: {err}", hooks_path.display()))?;
    let patched = patch_hooks_source(&source)?;
    fs::write(&hooks_path, patched)
        .map_err(|err| format!("failed to write {}: {err}", hooks_path.display()))?;
    pack_dir(&work_dir, &pack_path)?;
    println!("Patched WebChat OperaX manager hooks");
    Ok(())
}

fn install_initial_cards(ctx: &TestContext) -> Result<(), String> {
    let pack_path = ctx.bundle_dir.join("packs/default.gtpack");
    validate_gtpack(&pack_path)?;
    let work_dir = ctx.work_dir.join("default-app-pack");
    extract_zip_to_dir(&pack_path, &work_dir)?;
    let cards_dir = work_dir.join("assets/cards");
    fs::create_dir_all(&cards_dir)
        .map_err(|err| format!("failed to create {}: {err}", cards_dir.display()))?;
    let wc = welcome_card(
        &ctx.options.locale,
        &ctx.artifact_base,
        &ctx.operax_url.base,
    );
    write_json(&cards_dir.join("welcome_card.json"), &wc)?;
    write_json(&cards_dir.join("welcome.json"), &wc)?;
    write_json(
        &cards_dir.join("operax_dashboard.json"),
        &placeholder_dashboard_card(&ctx.options.locale),
    )?;
    write_card_i18n(&cards_dir, &ctx.options.locale)?;
    pack_dir(&work_dir, &pack_path)?;
    println!("Installed OperaX manager placeholder cards into default.gtpack");
    Ok(())
}

fn refresh_live_cards(ctx: &TestContext) -> Result<(), String> {
    let pack_path = ctx.bundle_dir.join("packs/default.gtpack");
    validate_gtpack(&pack_path)?;
    let work_dir = ctx.work_dir.join("default-app-pack-live");
    extract_zip_to_dir(&pack_path, &work_dir)?;
    let cards_dir = work_dir.join("assets/cards");
    fs::create_dir_all(&cards_dir)
        .map_err(|err| format!("failed to create {}: {err}", cards_dir.display()))?;
    let mut dashboard = manager_card(ctx, "dashboard")?;
    normalize_card_for_webchat(&mut dashboard, &ctx.operax_url);
    write_json(&cards_dir.join("operax_dashboard.json"), &dashboard)?;
    let mut queue = VecDeque::from(collect_navigable_targets(&dashboard));
    let mut seen = BTreeSet::new();
    while let Some(target) = queue.pop_front() {
        if seen.len() >= 20 || !seen.insert(target.clone()) {
            continue;
        }
        let Ok(mut card) = manager_card(ctx, &target) else {
            continue;
        };
        normalize_card_for_webchat(&mut card, &ctx.operax_url);
        write_json(
            &cards_dir.join(format!("{}.json", route_card_id(&target))),
            &card,
        )?;
        for next in collect_navigable_targets(&card) {
            if !seen.contains(&next) {
                queue.push_back(next);
            }
        }
    }
    write_card_i18n(&cards_dir, &ctx.options.locale)?;
    pack_dir(&work_dir, &pack_path)?;
    println!("Injected live OperaX manager cards into default.gtpack");
    Ok(())
}

fn manager_card(ctx: &TestContext, target: &str) -> Result<Value, String> {
    let path = format!("/v1/operax/manager/cards/{target}");
    http_get_json(ctx, &path)
}

fn http_get_json(ctx: &TestContext, path: &str) -> Result<Value, String> {
    let mut stream = TcpStream::connect(ctx.operax_url.bind_addr())
        .map_err(|err| format!("failed to connect to {}: {err}", ctx.operax_url.bind_addr()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|err| format!("failed to set HTTP read timeout: {err}"))?;
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {}\r\nAccept: application/json\r\nX-Greentic-Tenant-Id: {}\r\nX-Greentic-Caller-Id: local-test\r\nX-Greentic-Caller-Role: operator\r\nX-Greentic-Team: {}\r\nX-Greentic-Channel: webchat\r\nX-Greentic-Locale: {}\r\nAccept-Language: {}\r\nConnection: close\r\n\r\n",
        ctx.operax_url.host,
        ctx.options.tenant,
        ctx.options.team.as_deref().unwrap_or("default"),
        ctx.options.locale,
        ctx.options.locale
    );
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("failed to write HTTP request: {err}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .map_err(|err| format!("failed to read HTTP response: {err}"))?;
    http::parse_http_json_response(&response)
}

fn wait_for_manager_card(ctx: &TestContext, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut last_error = None;
    while Instant::now() < deadline {
        match manager_card(ctx, "dashboard") {
            Ok(_) => return Ok(()),
            Err(err) => last_error = Some(err),
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(format!(
        "OperaX dashboard card endpoint did not become ready: {}",
        last_error.unwrap_or_else(|| "unknown error".to_string())
    ))
}

fn ensure_port_available(url: &ParsedHttpUrl) -> Result<(), String> {
    TcpListener::bind(url.bind_addr()).map(|_| ()).map_err(|err| {
        format!(
            "cannot start OperaX manager on {}: address is already in use or unavailable ({err})",
            url.bind_addr()
        )
    })
}

fn first_available_http_url(mut url: ParsedHttpUrl, label: &str) -> Result<ParsedHttpUrl, String> {
    let requested = url.port;
    for port in requested..requested.saturating_add(20) {
        let candidate = format!("{}:{port}", url.host);
        if TcpListener::bind(&candidate).is_ok() {
            if port != requested {
                eprintln!(
                    "{label} port {requested} is in use; using http://{}:{port} instead",
                    url.host
                );
            }
            url.port = port;
            url.base = format!("http://{}:{port}", url.host);
            return Ok(url);
        }
    }
    Err(format!(
        "{label} port range {}-{} is unavailable on {}",
        requested,
        requested.saturating_add(19),
        url.host
    ))
}

fn validate_gtpack(path: &Path) -> Result<(), String> {
    if !path.is_file() {
        return Err(format!("gtpack not found: {}", path.display()));
    }
    open_pack(path, SigningPolicy::DevOk)
        .map(|_| ())
        .map_err(|err| {
            format!(
                "greentic-pack-lib failed to open {}: {}",
                path.display(),
                err.message
            )
        })
}

fn run_command(command: &mut Command, label: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|err| format!("failed to run {label}: {err}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{label} exited with status {status}"))
    }
}

fn absolutize(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map_err(|err| format!("failed to resolve current directory: {err}"))
            .map(|cwd| cwd.join(path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hook_patch_adds_operax_submit_and_open_handlers() {
        let source = "function createStoreMiddleware(store) {\n  return next => action => {\n    const result = next(action);\n    return result;\n  };\n}\n";
        let patched = patch_hooks_source(source).unwrap();
        assert!(patched.contains("__greenticOperaxManagerSubmitHook"));
        assert!(patched.contains("operax_manager_submit"));
        assert!(patched.contains("operax_manager_open"));
        assert_eq!(patched, patch_hooks_source(&patched).unwrap());
    }

    #[test]
    fn create_answers_uses_webchat_provider_and_operax_bundle_id() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("handoff");
        fs::create_dir_all(&artifact).unwrap();
        let ctx = prepare_context(TestOptions {
            artifact,
            tenant: "demo-tenant".to_string(),
            team: Some("property-ops".to_string()),
            sorx_url: "http://127.0.0.1:8787".to_string(),
            operax_url: "http://127.0.0.1:8797".to_string(),
            webchat_url: "http://127.0.0.1:8080".to_string(),
            locale: "en-GB".to_string(),
            audit_dir: None,
            bundle_dir: Some(temp.path().join("bundle")),
            setup_answers: None,
            force: false,
            no_start: true,
            sorx_token_env: "SORX_TOKEN".to_string(),
        })
        .unwrap();
        prepare_workspace(&ctx).unwrap();
        write_create_answers(&ctx).unwrap();
        let value: Value =
            serde_json::from_str(&fs::read_to_string(&ctx.create_answers).unwrap()).unwrap();
        assert_eq!(value["answers"]["bundle_id"], "operax-manager-handoff");
        assert_eq!(value["answers"]["extension_providers"][0], WEBCHAT_REF);
    }

    #[test]
    fn card_normalization_adds_live_manager_urls() {
        let temp = tempfile::tempdir().unwrap();
        let artifact = temp.path().join("handoff");
        fs::create_dir_all(&artifact).unwrap();
        let ctx = prepare_context(TestOptions {
            artifact,
            tenant: "demo-tenant".to_string(),
            team: None,
            sorx_url: "http://127.0.0.1:8787".to_string(),
            operax_url: "http://127.0.0.1:8797".to_string(),
            webchat_url: "http://127.0.0.1:8080".to_string(),
            locale: "en".to_string(),
            audit_dir: None,
            bundle_dir: Some(temp.path().join("bundle")),
            setup_answers: None,
            force: false,
            no_start: true,
            sorx_token_env: "SORX_TOKEN".to_string(),
        })
        .unwrap();
        let mut card = serde_json::json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "title": "Paste JSON",
                "data": {"action": "operax_manager_submit", "manager_target": "input"}
            }]
        });
        normalize_card_for_webchat(&mut card, &ctx.operax_url);
        let data = &card["actions"][0]["data"];
        assert_eq!(
            data["manager_submit_url"],
            "http://127.0.0.1:8797/v1/operax/manager/submit"
        );
        assert_eq!(
            data["manager_cards_base_url"],
            "http://127.0.0.1:8797/v1/operax/manager/cards"
        );
        assert_eq!(data["routeToCardId"], "input");
    }
}
