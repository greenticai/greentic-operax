use greentic_pack::{SigningPolicy, open_pack};
use operax_manager::{ManagerOptions, ManagerRuntime, start_manager_server};
use operax_sorx_http::HttpSorxClient;
use serde_json::{Value, json};
use std::collections::{BTreeSet, VecDeque};
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, ZipArchive, ZipWriter};

const WEBCHAT_REF: &str = "oci://ghcr.io/greenticai/packs/messaging/messaging-webchat-gui:stable";
const OPERAX_HOOK_MARKER: &str = "__greenticOperaxManagerSubmitHook";

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

#[derive(Debug, Clone)]
struct ParsedHttpUrl {
    base: String,
    host: String,
    port: u16,
}

impl ParsedHttpUrl {
    fn parse(input: &str, label: &str) -> Result<Self, String> {
        let base = input.trim().trim_end_matches('/').to_string();
        let rest = base
            .strip_prefix("http://")
            .ok_or_else(|| format!("{label} must look like http://host:port"))?;
        let host_port = rest
            .split('/')
            .next()
            .ok_or_else(|| format!("{label} must look like http://host:port"))?;
        let (host, port) = host_port
            .rsplit_once(':')
            .ok_or_else(|| format!("{label} must include a port"))?;
        let port = port
            .parse::<u16>()
            .map_err(|_| format!("{label} has an invalid port"))?;
        if host.trim().is_empty() {
            return Err(format!("{label} must include a host"));
        }
        let host = host.to_string();
        Ok(Self { base, host, port })
    }

    fn bind_addr(&self) -> String {
        format!("{}:{}", self.host, self.port)
    }
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
    println!("WebChat route:                {}", webchat_route_url(&ctx));
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
    println!("  WebChat URL:      {}", webchat_route_url(ctx));
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
    let value = json!({
        "wizard_id": "greentic-bundle.wizard.run",
        "schema_id": "greentic-bundle.wizard.answers",
        "schema_version": "1.0.0",
        "locale": ctx.options.locale,
        "answers": {
            "access_rules": [],
            "advanced_setup": false,
            "app_pack_entries": [],
            "app_packs": [],
            "bundle_id": ctx.bundle_id,
            "bundle_name": ctx.bundle_id,
            "export_intent": false,
            "extension_provider_entries": [{
                "detected_kind": "oci",
                "display_name": "Greentic Messaging WebChat GUI (stable)",
                "provider_id": "greentic.messaging.webchat-gui.stable",
                "reference": WEBCHAT_REF,
                "version": "stable"
            }],
            "extension_providers": [WEBCHAT_REF],
            "mode": "create",
            "output_dir": ctx.bundle_dir,
            "remote_catalogs": [],
            "setup_answers": {},
            "setup_execution_intent": false,
            "setup_specs": {}
        }
    });
    write_json(&ctx.create_answers, &value)
}

fn write_setup_answers_if_needed(ctx: &TestContext) -> Result<(), String> {
    if ctx.options.setup_answers.is_some() {
        return Ok(());
    }
    let value = json!({
        "bundle_source": ".",
        "env": "dev",
        "greentic_setup_version": "1.0.0",
        "platform_setup": {
            "deployment_targets": [],
            "static_routes": {
                "default_route_prefix_policy": "pack_declared",
                "public_base_url": ctx.webchat_url.base,
                "public_surface_policy": "enabled",
                "public_web_enabled": true,
                "tenant_path_policy": "pack_declared"
            },
            "tunnel": {"mode": "off"}
        },
        "setup_answers": {
            "messaging-webchat-gui": {
                "base_url": ctx.webchat_url.base,
                "jwt_signing_key": "operax-manager-local-signing-key-0123456789abcdef",
                "mode": "local_queue",
                "nav_links": [
                    {
                        "id": "operax-manager",
                        "label": "OperaX Manager",
                        "url": format!("{}/v1/operax/manager", ctx.operax_url.base)
                    },
                    {
                        "id": "operax-dashboard-card",
                        "label": "Dashboard Card",
                        "url": format!("{}/v1/operax/manager/cards/dashboard", ctx.operax_url.base)
                    }
                ],
                "presentation_mode": "standalone",
                "public_base_url": ctx.webchat_url.base,
                "route": "webchat",
                "skin": "default",
                "tenant_channel_id": "demo:webchat",
                "text_input_enabled": false
            }
        },
        "team": "default",
        "tenant": "demo"
    });
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

fn patch_hooks_source(source: &str) -> Result<String, String> {
    if source.contains(OPERAX_HOOK_MARKER) {
        return Ok(source.to_string());
    }
    let needle = "    const result = next(action);\n";
    let replacement = r#"    if (isGreenticOperaxManagerSubmitAction(action)) {
      handleGreenticOperaxManagerSubmit(store, action.payload.activity);
      return;
    }
    if (isGreenticOperaxManagerOpenAction(action)) {
      handleGreenticOperaxManagerOpen(store, action.payload.activity);
      return;
    }

    const result = next(action);
"#;
    if !source.contains(needle) {
        return Err("unable to find WebChat hook middleware insertion point".to_string());
    }
    let mut patched = source.replacen(needle, replacement, 1);
    patched.push_str(OPERAX_MANAGER_HOOK_JS);
    Ok(patched)
}

fn install_initial_cards(ctx: &TestContext) -> Result<(), String> {
    let pack_path = ctx.bundle_dir.join("packs/default.gtpack");
    validate_gtpack(&pack_path)?;
    let work_dir = ctx.work_dir.join("default-app-pack");
    extract_zip_to_dir(&pack_path, &work_dir)?;
    let cards_dir = work_dir.join("assets/cards");
    fs::create_dir_all(&cards_dir)
        .map_err(|err| format!("failed to create {}: {err}", cards_dir.display()))?;
    write_json(&cards_dir.join("welcome_card.json"), &welcome_card(ctx))?;
    write_json(&cards_dir.join("welcome.json"), &welcome_card(ctx))?;
    write_json(
        &cards_dir.join("operax_dashboard.json"),
        &placeholder_dashboard_card(ctx),
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
    normalize_card_for_webchat(&mut dashboard, ctx);
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
        normalize_card_for_webchat(&mut card, ctx);
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
    parse_http_json_response(&response)
}

fn parse_http_json_response(bytes: &[u8]) -> Result<Value, String> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "HTTP response did not include headers".to_string())?;
    let headers = String::from_utf8_lossy(&bytes[..split]);
    let status_line = headers
        .lines()
        .next()
        .ok_or_else(|| "HTTP response was empty".to_string())?;
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse::<u16>().ok())
        .ok_or_else(|| format!("invalid HTTP status line: {status_line}"))?;
    if !(200..300).contains(&status) {
        return Err(format!("HTTP {status}: {status_line}"));
    }
    serde_json::from_slice(&bytes[split + 4..])
        .map_err(|err| format!("HTTP response body is invalid JSON: {err}"))
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

fn write_json(path: &Path, value: &Value) -> Result<(), String> {
    let mut bytes = serde_json::to_vec_pretty(value)
        .map_err(|err| format!("failed to encode JSON for {}: {err}", path.display()))?;
    bytes.push(b'\n');
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
    }
    fs::write(path, bytes).map_err(|err| format!("failed to write {}: {err}", path.display()))
}

fn extract_zip_to_dir(pack_path: &Path, target: &Path) -> Result<(), String> {
    if target.exists() {
        fs::remove_dir_all(target)
            .map_err(|err| format!("failed to clean {}: {err}", target.display()))?;
    }
    fs::create_dir_all(target)
        .map_err(|err| format!("failed to create {}: {err}", target.display()))?;
    let file = fs::File::open(pack_path)
        .map_err(|err| format!("failed to open {}: {err}", pack_path.display()))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|err| format!("failed to read zip {}: {err}", pack_path.display()))?;
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|err| format!("failed to read zip entry {index}: {err}"))?;
        if entry.is_dir() {
            continue;
        }
        let Some(name) = entry.enclosed_name() else {
            return Err(format!("unsafe zip entry path `{}`", entry.name()));
        };
        let out_path = target.join(name);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|err| format!("failed to create {}: {err}", parent.display()))?;
        }
        let mut out = fs::File::create(&out_path)
            .map_err(|err| format!("failed to create {}: {err}", out_path.display()))?;
        std::io::copy(&mut entry, &mut out)
            .map_err(|err| format!("failed to extract {}: {err}", out_path.display()))?;
    }
    Ok(())
}

fn pack_dir(source: &Path, pack_path: &Path) -> Result<(), String> {
    let tmp = pack_path.with_extension("gtpack.tmp");
    let file = fs::File::create(&tmp)
        .map_err(|err| format!("failed to create {}: {err}", tmp.display()))?;
    let mut writer = ZipWriter::new(file);
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .unix_permissions(0o644);
    for path in sorted_files(source)? {
        let rel = path
            .strip_prefix(source)
            .map_err(|err| format!("failed to compute relative zip path: {err}"))?
            .to_string_lossy()
            .replace('\\', "/");
        writer
            .start_file(rel, options)
            .map_err(|err| format!("failed to start zip entry: {err}"))?;
        let bytes =
            fs::read(&path).map_err(|err| format!("failed to read {}: {err}", path.display()))?;
        writer
            .write_all(&bytes)
            .map_err(|err| format!("failed to write zip entry: {err}"))?;
    }
    writer
        .finish()
        .map_err(|err| format!("failed to finish {}: {err}", tmp.display()))?;
    fs::rename(&tmp, pack_path).map_err(|err| {
        format!(
            "failed to replace {} with {}: {err}",
            pack_path.display(),
            tmp.display()
        )
    })
}

fn sorted_files(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    collect_files(root, &mut files)?;
    files.sort();
    Ok(files)
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<(), String> {
    for entry in
        fs::read_dir(root).map_err(|err| format!("failed to read {}: {err}", root.display()))?
    {
        let entry = entry.map_err(|err| format!("failed to read directory entry: {err}"))?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn welcome_card(ctx: &TestContext) -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "lang": ctx.options.locale,
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "metadata": {"locale": ctx.options.locale, "schema": "greentic.operax.manager-card.v1"},
        "body": [
            {"type": "TextBlock", "text": "OperaX Manager", "size": "Large", "weight": "Bolder", "wrap": true},
            {"type": "TextBlock", "text": format!("Testing {}", ctx.artifact_base), "wrap": true}
        ],
        "actions": [{
            "type": "Action.Submit",
            "title": "Open Dashboard",
            "data": {
                "action": "operax_manager_open",
                "manager_target": "dashboard",
                "manager_cards_base_url": format!("{}/v1/operax/manager/cards", ctx.operax_url.base),
                "routeToCardId": "operax_dashboard",
                "cardId": "operax_dashboard",
                "step": "open"
            }
        }]
    })
}

fn placeholder_dashboard_card(ctx: &TestContext) -> Value {
    json!({
        "type": "AdaptiveCard",
        "version": "1.5",
        "lang": ctx.options.locale,
        "$schema": "http://adaptivecards.io/schemas/adaptive-card.json",
        "metadata": {
            "locale": ctx.options.locale,
            "schema": "greentic.operax.manager-card.v1",
            "kind": "manager.dashboard.placeholder"
        },
        "body": [
            {"type": "TextBlock", "text": "OperaX dashboard is starting", "size": "Large", "weight": "Bolder", "wrap": true},
            {"type": "TextBlock", "text": "The live dashboard card will be injected after the OperaX manager is ready.", "wrap": true}
        ],
        "actions": []
    })
}

fn normalize_card_for_webchat(card: &mut Value, ctx: &TestContext) {
    normalize_actions(card, ctx);
    normalize_card_items(card);
}

fn normalize_actions(value: &mut Value, ctx: &TestContext) {
    match value {
        Value::Object(map) => {
            let is_submit = map
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|value| value == "Action.Submit");
            if is_submit && let Some(data) = map.get_mut("data").and_then(Value::as_object_mut) {
                if data.get("action").and_then(Value::as_str) == Some("operax_manager_submit") {
                    set_absolute_manager_url(
                        data,
                        "manager_submit_url",
                        &format!("{}/v1/operax/manager/submit", ctx.operax_url.base),
                    );
                }
                if let Some(target) = data
                    .get("manager_target")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
                {
                    set_absolute_manager_url(
                        data,
                        "manager_cards_base_url",
                        &format!("{}/v1/operax/manager/cards", ctx.operax_url.base),
                    );
                    data.insert("routeToCardId".to_string(), json!(route_card_id(&target)));
                    data.entry("cardId".to_string())
                        .or_insert_with(|| json!(route_card_id(&target)));
                    data.entry("step".to_string())
                        .or_insert_with(|| json!("open"));
                    data.entry("action".to_string())
                        .or_insert_with(|| json!("operax_manager_open"));
                }
            }
            for child in map.values_mut() {
                normalize_actions(child, ctx);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_actions(child, ctx);
            }
        }
        _ => {}
    }
}

fn normalize_card_items(value: &mut Value) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("TextBlock") {
                for key in ["size", "weight"] {
                    if let Some(text) = map.get(key).and_then(Value::as_str) {
                        let mut chars = text.chars();
                        if let Some(first) = chars.next() {
                            *map.get_mut(key).expect("key exists") = Value::String(format!(
                                "{}{}",
                                first.to_uppercase(),
                                chars.as_str()
                            ));
                        }
                    }
                }
            }
            if map.get("type").and_then(Value::as_str) == Some("Input.Text") {
                let label = map
                    .get("label")
                    .or_else(|| map.get("placeholder"))
                    .or_else(|| map.get("id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(label) = label {
                    map.entry("label".to_string())
                        .or_insert_with(|| json!(label));
                }
            }
            for child in map.values_mut() {
                normalize_card_items(child);
            }
        }
        Value::Array(items) => {
            for child in items {
                normalize_card_items(child);
            }
        }
        _ => {}
    }
}

fn set_absolute_manager_url(map: &mut serde_json::Map<String, Value>, key: &str, url: &str) {
    let needs_set = map
        .get(key)
        .and_then(Value::as_str)
        .is_none_or(|value| value.starts_with('/') || value.trim().is_empty());
    if needs_set {
        map.insert(key.to_string(), json!(url));
    }
}

fn webchat_route_url(ctx: &TestContext) -> String {
    format!("{}/v1/web/webchat/demo/", ctx.webchat_url.base)
}

fn collect_navigable_targets(card: &Value) -> Vec<String> {
    let mut targets = BTreeSet::new();
    collect_targets(card, &mut targets);
    targets.into_iter().collect()
}

fn collect_targets(value: &Value, targets: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            if map.get("type").and_then(Value::as_str) == Some("Action.Submit")
                && let Some(target) = map
                    .get("data")
                    .and_then(Value::as_object)
                    .and_then(|data| data.get("manager_target"))
                    .and_then(Value::as_str)
            {
                targets.insert(target.to_string());
            }
            for child in map.values() {
                collect_targets(child, targets);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_targets(child, targets);
            }
        }
        _ => {}
    }
}

fn write_card_i18n(cards_dir: &Path, locale: &str) -> Result<(), String> {
    let i18n_dir = cards_dir
        .parent()
        .ok_or_else(|| format!("cards directory has no parent: {}", cards_dir.display()))?
        .join("i18n");
    fs::create_dir_all(&i18n_dir)
        .map_err(|err| format!("failed to create {}: {err}", i18n_dir.display()))?;
    let mut en = serde_json::Map::new();
    for file in sorted_files(cards_dir)? {
        if file.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(card_name) = file.file_stem().and_then(|name| name.to_str()) else {
            continue;
        };
        let Ok(value) = fs::read_to_string(&file)
            .ok()
            .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            .ok_or(())
        else {
            continue;
        };
        collect_i18n(card_name, &value, Vec::new(), &mut en);
    }
    write_json(
        &i18n_dir.join("_manifest.json"),
        &json!({"locales": locale_codes(locale)}),
    )?;
    let en_value = Value::Object(en);
    write_json(&i18n_dir.join("en.json"), &en_value)?;
    for code in locale_codes(locale) {
        if code != "en" {
            write_json(&i18n_dir.join(format!("{code}.json")), &en_value)?;
        }
    }
    Ok(())
}

fn collect_i18n(
    card_name: &str,
    value: &Value,
    path: Vec<String>,
    out: &mut serde_json::Map<String, Value>,
) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if ["text", "title", "label", "placeholder", "errorMessage"].contains(&key.as_str())
                {
                    if let Some(text) = child.as_str() {
                        out.insert(
                            format!("cards.{card_name}.{}.{}", path.join("."), key),
                            json!(text),
                        );
                    }
                } else {
                    let mut next = path.clone();
                    next.push(key.clone());
                    collect_i18n(card_name, child, next, out);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let mut next = path.clone();
                next.push(format!("i{index}"));
                collect_i18n(card_name, child, next, out);
            }
        }
        _ => {}
    }
}

fn locale_codes(locale: &str) -> Vec<String> {
    let mut codes = vec!["en".to_string()];
    if !codes.iter().any(|value| value == locale) {
        codes.push(locale.to_string());
    }
    if let Some(language) = locale.split('-').next()
        && !language.is_empty()
        && !codes.iter().any(|value| value == language)
    {
        codes.push(language.to_string());
    }
    codes
}

fn route_card_id(target: &str) -> String {
    if target == "dashboard" {
        return "operax_dashboard".to_string();
    }
    target
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
                ch
            } else {
                '_'
            }
        })
        .collect()
}

fn sanitize_id(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .collect()
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

const OPERAX_MANAGER_HOOK_JS: &str = r#"

// OperaX manager Adaptive Cards are rendered as WebChat card assets, but their
// submit and open actions need to call the live manager API.
var __greenticOperaxManagerSubmitHook = true;

function isGreenticOperaxManagerSubmitAction(action) {
  var activity = action && action.payload && action.payload.activity;
  var value = activity && activity.value;
  return action && action.type === 'DIRECT_LINE/POST_ACTIVITY' &&
    value && value.action === 'operax_manager_submit';
}

function isGreenticOperaxManagerOpenAction(action) {
  var activity = action && action.payload && action.payload.activity;
  var value = activity && activity.value;
  return action && action.type === 'DIRECT_LINE/POST_ACTIVITY' &&
    value && (value.action === 'operax_manager_open' || value.manager_target);
}

function greenticOperaxHeaderValue(value, fallback) {
  return value == null || value === '' ? fallback : String(value);
}

function greenticOperaxHeaders(value) {
  var locale = document.documentElement.getAttribute('lang') ||
    document.querySelector('[data-webchat-locale]')?.getAttribute('data-webchat-locale') ||
    'en';
  return {
    'Content-Type': 'application/json',
    'Accept': 'application/json',
    'X-Greentic-Tenant-Id': greenticOperaxHeaderValue(window.__TENANT__, 'demo'),
    'X-Greentic-Caller-Id': greenticOperaxHeaderValue(window.__GUEST_ID__, 'webchat-user'),
    'X-Greentic-Caller-Role': 'operator',
    'X-Greentic-Channel': 'webchat',
    'X-Greentic-Locale': locale,
    'Accept-Language': locale
  };
}

function greenticOperaxCardsBase(value) {
  if (value.manager_cards_base_url && !String(value.manager_cards_base_url).startsWith('/')) {
    window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ = value.manager_cards_base_url;
    return value.manager_cards_base_url;
  }
  if (value.manager_submit_url && !String(value.manager_submit_url).startsWith('/')) {
    var derived = String(value.manager_submit_url).replace(/\/submit(?:[?#].*)?$/, '/cards');
    window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ = derived;
    return derived;
  }
  return window.__GREENTIC_OPERAX_MANAGER_CARDS_BASE_URL__ || null;
}

function greenticOperaxSubmitUrl(value) {
  if (value.manager_submit_url) return value.manager_submit_url;
  var base = greenticOperaxCardsBase(value);
  return base ? String(base).replace(/\/cards\/?$/, '/submit') : null;
}

function greenticOperaxCardUrl(value) {
  var base = greenticOperaxCardsBase(value);
  var target = value.manager_target || 'dashboard';
  return base ? String(base).replace(/\/+$/, '') + '/' + String(target) : null;
}

function greenticOperaxNormalizeCard(value, sourceValue) {
  var cardsBase = greenticOperaxCardsBase(sourceValue || {});
  if (!cardsBase || value == null) return value;
  if (Array.isArray(value)) {
    value.forEach(function (item) { greenticOperaxNormalizeCard(item, sourceValue); });
    return value;
  }
  if (typeof value !== 'object') return value;
  if (value.type === 'Action.Submit') {
    value.data = value.data || {};
    if (value.data.manager_target || value.data.action === 'operax_manager_open') {
      value.data.manager_cards_base_url = cardsBase;
      value.data.action = value.data.action || 'operax_manager_open';
    }
    if (value.data.action === 'operax_manager_submit') {
      value.data.manager_cards_base_url = cardsBase;
      value.data.manager_submit_url = String(cardsBase).replace(/\/cards\/?$/, '/submit');
    }
  }
  Object.keys(value).forEach(function (key) {
    greenticOperaxNormalizeCard(value[key], sourceValue);
  });
  return value;
}

function greenticOperaxInput(value) {
  if (value.input !== undefined) return value.input;
  if (value.json !== undefined) return value.json;
  if (value.payload !== undefined) return value.payload;
  if (value.input_json !== undefined) {
    try { return JSON.parse(value.input_json); } catch (_) { return value.input_json; }
  }
  var input = {};
  Object.keys(value || {}).forEach(function (key) {
    if (/^(action|cardId|routeToCardId|step|manager_|_)/.test(key)) return;
    if (key === 'operation' || key === 'mode') return;
    input[key] = value[key];
  });
  return input;
}

function greenticOperaxIncomingCardActivity(card) {
  return {
    type: 'message',
    id: 'greentic-operax-manager-' + Date.now(),
    timestamp: new Date().toISOString(),
    from: { id: 'operax-manager', name: 'OperaX Manager', role: 'bot' },
    attachments: [{ contentType: 'application/vnd.microsoft.card.adaptive', content: card }]
  };
}

function greenticOperaxIncomingTextActivity(text) {
  return {
    type: 'message',
    id: 'greentic-operax-manager-error-' + Date.now(),
    timestamp: new Date().toISOString(),
    from: { id: 'operax-manager', name: 'OperaX Manager', role: 'bot' },
    text: text
  };
}

async function handleGreenticOperaxManagerSubmit(store, activity) {
  var value = Object.assign({}, activity && activity.value || {});
  var submitUrl = greenticOperaxSubmitUrl(value);
  if (!submitUrl) return;
  greenticOperaxCardsBase(value);
  var body = Object.assign({}, value, { input: greenticOperaxInput(value) });
  try {
    var submitResponse = await fetch(submitUrl, {
      method: 'POST',
      headers: greenticOperaxHeaders(value),
      body: JSON.stringify(body)
    });
    if (!submitResponse.ok) throw new Error('OperaX manager submit failed with HTTP ' + submitResponse.status);
    var card = greenticOperaxNormalizeCard(await submitResponse.json(), value);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingCardActivity(card) }
    });
  } catch (err) {
    console.error('[operax-manager-submit]', err);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingTextActivity('Unable to submit this OperaX manager form. Please try again.') }
    });
  }
}

async function handleGreenticOperaxManagerOpen(store, activity) {
  var value = Object.assign({}, activity && activity.value || {});
  var cardUrl = greenticOperaxCardUrl(value);
  if (!cardUrl) return;
  try {
    var cardResponse = await fetch(cardUrl, {
      method: 'GET',
      headers: greenticOperaxHeaders(value)
    });
    if (!cardResponse.ok) throw new Error('OperaX manager card load failed with HTTP ' + cardResponse.status);
    var card = greenticOperaxNormalizeCard(await cardResponse.json(), value);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingCardActivity(card) }
    });
  } catch (err) {
    console.error('[operax-manager-open]', err);
    store.dispatch({
      type: 'DIRECT_LINE/INCOMING_ACTIVITY',
      payload: { activity: greenticOperaxIncomingTextActivity('Unable to open this OperaX manager card. Please try again.') }
    });
  }
}
"#;

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
        let mut card = json!({
            "type": "AdaptiveCard",
            "actions": [{
                "type": "Action.Submit",
                "title": "Paste JSON",
                "data": {"action": "operax_manager_submit", "manager_target": "input"}
            }]
        });
        normalize_card_for_webchat(&mut card, &ctx);
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
