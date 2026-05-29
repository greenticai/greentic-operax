use clap::{CommandFactory, Parser, Subcommand};
use operax_core::{OperaxError, Result};
use operax_runtime::{RunRequest, run_artifact_with_client};
use operax_sorx_http::HttpSorxClient;
use serde_json::Value;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;

mod test_runtime;

#[derive(Debug, Parser)]
#[command(
    name = "greentic-operax",
    version,
    about = "Greentic operational handoff runner"
)]
pub struct Cli {
    /// Locale code for localized CLI text, such as en or nl.
    #[arg(long, global = true)]
    locale: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
pub enum Commands {
    /// Run an OperaLa operational handoff artifact.
    Run(RunArgs),
    /// Start a local OperaX manager for testing a handoff artifact.
    Test(TestArgs),
}

#[derive(Debug, Parser)]
pub struct RunArgs {
    /// OperaLa handoff directory or pilot .gtpack artifact.
    artifact: PathBuf,
    /// Tenant id to pass to SORX.
    #[arg(long)]
    tenant: String,
    /// SORX base URL.
    #[arg(long)]
    sorx_url: String,
    /// JSON input file to process.
    #[arg(long)]
    input: PathBuf,
    /// Optional team id.
    #[arg(long)]
    team: Option<String>,
    /// Run without mutating SORX.
    #[arg(long)]
    dry_run: bool,
    /// Directory for audit.jsonl.
    #[arg(long)]
    audit_dir: Option<PathBuf>,
    /// Environment variable containing the SORX token.
    #[arg(long, default_value = "SORX_TOKEN")]
    sorx_token_env: String,
    /// Emit the run report as JSON.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Parser)]
pub struct TestArgs {
    /// OperaLa handoff directory or pilot .gtpack artifact.
    artifact: PathBuf,
    /// Tenant id to pass to OperaX and SORX.
    #[arg(long)]
    tenant: Option<String>,
    /// SORX base URL used when actions are applied.
    #[arg(long)]
    sorx_url: Option<String>,
    /// OperaX manager base URL.
    #[arg(long, default_value = "http://127.0.0.1:8797")]
    operax_url: String,
    /// WebChat base URL.
    #[arg(long, default_value = "http://127.0.0.1:8080")]
    webchat_url: String,
    /// Optional team id.
    #[arg(long)]
    team: Option<String>,
    /// Directory for audit.jsonl.
    #[arg(long)]
    audit_dir: Option<PathBuf>,
    /// Bundle workspace directory.
    #[arg(long)]
    bundle_dir: Option<PathBuf>,
    /// gtc setup answers file.
    #[arg(long)]
    answers: Option<PathBuf>,
    /// Replace an existing OperaX-created test bundle directory.
    #[arg(long)]
    force: bool,
    /// Prepare the bundle but do not start the manager or WebChat runtime.
    #[arg(long)]
    no_start: bool,
    /// Environment variable containing the SORX token.
    #[arg(long, default_value = "SORX_TOKEN")]
    sorx_token_env: String,
}

pub fn run<I, T>(args: I) -> ExitCode
where
    I: IntoIterator<Item = T>,
    T: Into<OsString> + Clone,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<OsString>>();
    if localized_help_requested(&args)
        && let Some(locale) = locale_from_args(&args)
        && locale != "en"
    {
        print!("{}", localized_help_for_args(&locale, &args));
        return ExitCode::SUCCESS;
    }

    match Cli::try_parse_from(args) {
        Ok(cli) => match dispatch(cli) {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                eprintln!("{error}");
                ExitCode::from(exit_code(&error) as u8)
            }
        },
        Err(error) => {
            let _ = error.print();
            match error.kind() {
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                    ExitCode::SUCCESS
                }
                _ => ExitCode::from(2),
            }
        }
    }
}

pub fn command() -> clap::Command {
    Cli::command()
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Run(mut args) => {
            if args.team.is_none() {
                args.team = None;
            }
            run_operax(args, cli.locale)
        }
        Commands::Test(args) => run_test_manager(args, cli.locale),
    }
}

fn run_operax(args: RunArgs, locale: Option<String>) -> Result<()> {
    let input_text = std::fs::read_to_string(&args.input)?;
    let input_json: Value = serde_json::from_str(&input_text)?;
    let client = HttpSorxClient::new(
        args.sorx_url.clone(),
        std::env::var(&args.sorx_token_env)
            .ok()
            .filter(|token| !token.is_empty()),
    );
    let locale_for_output = locale.clone();
    let report = run_artifact_with_client(
        RunRequest {
            artifact: args.artifact,
            tenant: args.tenant,
            team: args.team,
            locale,
            input: input_json,
            dry_run: args.dry_run,
            audit_dir: args.audit_dir,
        },
        &client,
    )?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        let catalog = I18nCatalog::load(locale_for_output.as_deref().unwrap_or("en"));
        println!(
            "{}",
            catalog
                .text("operax.output.completed")
                .replace("{input_count}", &report.input_count.to_string())
                .replace("{decision_count}", &report.decisions.len().to_string())
                .replace("{applied_actions}", &report.applied_actions.to_string())
                .replace("{skipped_actions}", &report.skipped_actions.to_string())
        );
    }
    Ok(())
}

fn run_test_manager(args: TestArgs, locale: Option<String>) -> Result<()> {
    let tenant = args
        .tenant
        .or_else(|| std::env::var("OPERAX_TEST_TENANT").ok())
        .ok_or_else(|| {
            OperaxError::new(
                "missing_tenant",
                "greentic-operax test requires --tenant or OPERAX_TEST_TENANT",
            )
        })?;
    let sorx_url = args
        .sorx_url
        .or_else(|| std::env::var("SORX_URL").ok())
        .ok_or_else(|| {
            OperaxError::new(
                "missing_sorx_url",
                "greentic-operax test requires --sorx-url or SORX_URL",
            )
        })?;
    let options = test_runtime::TestOptions {
        artifact: args.artifact,
        tenant,
        team: args.team.or_else(|| std::env::var("OPERAX_TEST_TEAM").ok()),
        sorx_url,
        operax_url: args.operax_url,
        webchat_url: args.webchat_url,
        locale: locale
            .or_else(|| std::env::var("OPERAX_TEST_LOCALE").ok())
            .unwrap_or_else(|| "en".to_string()),
        audit_dir: args.audit_dir,
        bundle_dir: args.bundle_dir.or_else(|| {
            std::env::var("OPERAX_TEST_BUNDLE_DIR")
                .ok()
                .map(PathBuf::from)
        }),
        setup_answers: args.answers.or_else(|| {
            std::env::var("OPERAX_TEST_SETUP_ANSWERS")
                .ok()
                .map(PathBuf::from)
        }),
        force: args.force,
        no_start: args.no_start || std::env::var("OPERAX_TEST_NO_START").is_ok(),
        sorx_token_env: args.sorx_token_env,
    };
    test_runtime::run(options).map_err(|message| OperaxError::new("test_runtime", message))
}

fn exit_code(error: &OperaxError) -> i32 {
    match error.code.as_str() {
        "missing_tenant" | "missing_team" => 2,
        "unsupported_artifact"
        | "missing_operala_yaml"
        | "missing_operala_handoff"
        | "invalid_handoff_schema"
        | "invalid_gtpack"
        | "invalid_archive_path"
        | "secret_like_value" => 3,
        "unknown_input_shape" | "invalid_input" | "invalid_batch_input" => 4,
        "policy_denied" => 7,
        _ => 1,
    }
}

fn help_requested(args: &[OsString]) -> bool {
    args.iter().any(|arg| arg == "--help" || arg == "-h")
}

fn localized_help_requested(args: &[OsString]) -> bool {
    help_requested(args) || help_subcommand_requested(args)
}

fn help_subcommand_requested(args: &[OsString]) -> bool {
    let mut skip_next = false;
    for arg in args.iter().skip(1).filter_map(|arg| arg.to_str()) {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--locale" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--locale=") {
            continue;
        }
        return arg == "help";
    }
    false
}

fn locale_from_args(args: &[OsString]) -> Option<String> {
    for (index, arg) in args.iter().enumerate() {
        if arg == "--locale" {
            return args
                .get(index + 1)
                .and_then(|value| value.to_str())
                .map(ToString::to_string);
        }
        let Some(arg) = arg.to_str() else {
            continue;
        };
        if let Some(value) = arg.strip_prefix("--locale=") {
            return Some(value.to_string());
        }
    }
    None
}

fn localized_help_for_args(locale: &str, args: &[OsString]) -> String {
    let catalog = I18nCatalog::load(locale);
    let mut cmd = localized_command(&catalog);
    let mut help = if args
        .iter()
        .filter_map(|arg| arg.to_str())
        .any(|arg| arg == "run")
    {
        cmd.find_subcommand_mut("run")
            .map(|run| run.render_long_help().to_string())
            .unwrap_or_else(|| cmd.render_long_help().to_string())
    } else {
        cmd.render_long_help().to_string()
    };
    for (from, to) in catalog.replacements() {
        help = help.replace(from, &to);
    }
    help
}

fn localized_command(catalog: &I18nCatalog) -> clap::Command {
    let mut cmd = command();
    cmd = cmd.about(catalog.text("cli.about"));
    set_subcommand_about(&mut cmd, "run", catalog.text("cli.command.run.about"));
    set_subcommand_about(&mut cmd, "help", catalog.text("cli.command.help.about"));
    localize_run_subcommand(&mut cmd, catalog);
    cmd.mut_arg("locale", |arg| {
        arg.help(catalog.text("cli.option.locale.help"))
    })
    .help_template(catalog.text("cli.help.template"))
}

fn localize_run_subcommand(cmd: &mut clap::Command, catalog: &I18nCatalog) {
    if let Some(run) = cmd.find_subcommand_mut("run") {
        let mut next = std::mem::take(run)
            .about(catalog.text("cli.command.run.about"))
            .help_template(catalog.text("cli.run.help.template"));
        for (arg, key) in [
            ("artifact", "cli.run.arg.artifact.help"),
            ("tenant", "cli.run.option.tenant.help"),
            ("sorx_url", "cli.run.option.sorx_url.help"),
            ("input", "cli.run.option.input.help"),
            ("team", "cli.run.option.team.help"),
            ("dry_run", "cli.run.option.dry_run.help"),
            ("audit_dir", "cli.run.option.audit_dir.help"),
            ("sorx_token_env", "cli.run.option.sorx_token_env.help"),
            ("json", "cli.run.option.json.help"),
        ] {
            next = next.mut_arg(arg, |arg| arg.help(catalog.text(key)));
        }
        *run = next;
    }
}

fn set_subcommand_about(cmd: &mut clap::Command, name: &str, about: String) {
    if let Some(subcommand) = cmd.find_subcommand_mut(name) {
        let next = std::mem::take(subcommand).about(about);
        *subcommand = next;
    }
}

#[derive(Debug, Clone)]
struct I18nCatalog {
    values: serde_json::Map<String, serde_json::Value>,
}

impl I18nCatalog {
    fn load(locale: &str) -> Self {
        let requested = read_i18n_catalog(locale);
        let fallback = read_i18n_catalog("en")
            .or_else(|| serde_json::from_str(include_str!("../i18n/en.json")).ok())
            .and_then(|value: serde_json::Value| value.as_object().cloned())
            .unwrap_or_default();
        let mut values = fallback;
        if let Some(requested) = requested.and_then(|value| value.as_object().cloned()) {
            values.extend(requested);
        }
        Self { values }
    }

    fn text(&self, key: &str) -> String {
        self.values
            .get(key)
            .and_then(serde_json::Value::as_str)
            .unwrap_or(key)
            .to_string()
    }

    fn replacements(&self) -> Vec<(&'static str, String)> {
        vec![
            ("Arguments:", self.text("cli.help.heading.arguments")),
            ("Commands:", self.text("cli.help.heading.commands")),
            ("Options:", self.text("cli.help.heading.options")),
            ("[default:", self.text("cli.help.default.prefix")),
            ("Print help", self.text("cli.option.help.help")),
            ("Print version", self.text("cli.option.version.help")),
            (
                "Print this message or the help of the given subcommand(s)",
                self.text("cli.command.help.about"),
            ),
        ]
    }
}

fn read_i18n_catalog(locale: &str) -> Option<serde_json::Value> {
    let relative = PathBuf::from("i18n").join(format!("{locale}.json"));
    let raw = fs::read_to_string(&relative).ok().or_else(|| {
        fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(relative),
        )
        .ok()
    });
    let raw = raw.as_deref().or_else(|| embedded_i18n_catalog(locale))?;
    serde_json::from_str(raw).ok()
}

include!(concat!(env!("OUT_DIR"), "/embedded_i18n.rs"));

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_usage_errors_to_exit_code_two() {
        assert_eq!(exit_code(&OperaxError::new("missing_tenant", "")), 2);
    }

    #[test]
    fn localized_help_uses_requested_catalog() {
        let help = localized_help_for_args(
            "nl",
            &[OsString::from("greentic-operax"), OsString::from("--help")],
        );
        assert!(help.contains("Voer OperaLa"));
        assert!(help.contains("Gebruik:"));
    }

    #[test]
    fn localized_run_help_uses_requested_catalog() {
        let help = localized_help_for_args(
            "nl",
            &[
                OsString::from("greentic-operax"),
                OsString::from("--locale"),
                OsString::from("nl"),
                OsString::from("run"),
                OsString::from("--help"),
            ],
        );
        assert!(help.contains("Argumenten en opties:"));
        assert!(help.contains("Argumenten:"));
        assert!(help.contains("Opties:"));
        assert!(help.contains("SORX-basis-URL."));
        assert!(help.contains("OperaLa-overdrachtsmap"));
        assert!(help.contains("[standaard: SORX_TOKEN]"));
        assert!(!help.contains("Arguments:"));
    }

    #[test]
    fn locale_from_args_accepts_equals_form() {
        let args = [
            OsString::from("greentic-operax"),
            OsString::from("--locale=nl"),
        ];
        assert_eq!(locale_from_args(&args).as_deref(), Some("nl"));
    }

    #[test]
    fn help_subcommand_requests_localized_help() {
        let args = [
            OsString::from("greentic-operax"),
            OsString::from("help"),
            OsString::from("run"),
            OsString::from("--locale"),
            OsString::from("nl"),
        ];
        assert!(localized_help_requested(&args));

        let run_args = [
            OsString::from("greentic-operax"),
            OsString::from("run"),
            OsString::from("help"),
            OsString::from("--locale"),
            OsString::from("nl"),
        ];
        assert!(!localized_help_requested(&run_args));
    }

    #[test]
    fn locale_catalogs_are_embedded_in_binary() {
        let nl = embedded_i18n_catalog("nl").expect("Dutch catalog should be embedded");
        assert!(nl.contains("Voer OperaLa"));
        let ar = embedded_i18n_catalog("ar").expect("Arabic catalog should be embedded");
        assert!(!ar.contains("Run OperaLa"));
        assert!(embedded_i18n_catalog("missing").is_none());
    }
}
