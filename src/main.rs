//! cargo-chronoscope — Cargo build performance observer.
//!
//! Entry point that parses CLI arguments and dispatches to the appropriate
//! command handler. Assembles all async tasks and manages graceful shutdown.

mod anomaly;
mod broker;
mod cli;
mod diff;
mod model;
mod parser;
mod persist;
mod supervisor;
mod tui;

use std::path::PathBuf;
use std::sync::Arc;

use clap::{CommandFactory, Parser};
use tokio_util::sync::CancellationToken;

use crate::cli::{Cli, Command};
use crate::model::BuildId;
use crate::persist::BuildRepository;

/// Default DB directory name within the project root.
const DB_DIR: &str = ".cargo-chronoscope";
/// Default DB file name.
const DB_FILE: &str = "history.db";

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let cancel = CancellationToken::new();

    // Set up Ctrl-C handler.
    let cancel_clone = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to listen for Ctrl-C");
        cancel_clone.cancel();
    });

    if let Command::Completions { shell } = cli.command {
        let mut cmd = Cli::command();
        clap_complete::generate(shell, &mut cmd, "cargo-chronoscope", &mut std::io::stdout());
        return Ok(());
    }

    // Determine workspace directory and DB path.
    let workspace_dir = std::env::current_dir()?;
    let db_dir = workspace_dir.join(DB_DIR);
    std::fs::create_dir_all(&db_dir)?;
    let db_path = db_dir.join(DB_FILE);

    match cli.command {
        Command::Record { cargo_args } => {
            cmd_record(cargo_args, workspace_dir, &db_path, cancel).await?;
        }
        Command::Watch { cargo_args } => {
            cmd_watch(cargo_args, workspace_dir, &db_path, cancel).await?;
        }
        Command::Ls { last, format } => {
            cmd_ls(&db_path, last, format).await?;
        }
        Command::Diff {
            before,
            after,
            format,
        } => {
            cmd_diff(&db_path, before, after, format).await?;
        }
        Command::Completions { .. } => unreachable!("handled before database setup"),
    }

    Ok(())
}

/// Record a build: Supervisor → Parser → Persister (3-task pipeline).
async fn cmd_record(
    cargo_args: Vec<String>,
    workspace_dir: PathBuf,
    db_path: &std::path::Path,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let commit_hash = read_git_head();
    let profile = infer_profile(&cargo_args);

    let repo = Arc::new(persist::SqliteRepository::open(db_path).await?);

    let (line_rx, handle) = supervisor::spawn_build(cargo_args.clone(), workspace_dir).await?;

    // Kill the cargo child when the user cancels (Ctrl-C).
    let cancel_for_supervisor = cancel.clone();
    tokio::spawn(async move {
        cancel_for_supervisor.cancelled().await;
        handle.cancel();
    });

    let config = parser::ParserConfig {
        commit_hash,
        cargo_args,
        profile,
    };
    let event_rx = parser::run_parser(line_rx, config).await?;

    let build_id = persist::run_persister(repo.clone(), event_rx).await?;
    finalize_or_discard(repo, build_id, &cancel).await
}

/// Watch a build: Supervisor → Parser → Broker → (Persister + TUI) fan-out.
async fn cmd_watch(
    cargo_args: Vec<String>,
    workspace_dir: PathBuf,
    db_path: &std::path::Path,
    cancel: CancellationToken,
) -> anyhow::Result<()> {
    let commit_hash = read_git_head();
    let profile = infer_profile(&cargo_args);

    let repo = Arc::new(persist::SqliteRepository::open(db_path).await?);

    let (line_rx, handle) = supervisor::spawn_build(cargo_args.clone(), workspace_dir).await?;

    // Kill the cargo child when the user cancels (Ctrl-C or TUI `q`).
    let cancel_for_supervisor = cancel.clone();
    tokio::spawn(async move {
        cancel_for_supervisor.cancelled().await;
        handle.cancel();
    });

    let config = parser::ParserConfig {
        commit_hash,
        cargo_args,
        profile,
    };
    let event_rx = parser::run_parser(line_rx, config).await?;

    // Set up broker with two subscribers: persister and TUI.
    let mut event_broker = broker::EventBroker::new();
    let persister_rx = event_broker.subscribe(1024);
    let tui_rx = event_broker.subscribe(1024);

    let repo_clone = repo.clone();
    let cancel_clone = cancel.clone();

    // Run all tasks concurrently.
    let (broker_result, persister_result, tui_result) = tokio::try_join!(
        event_broker.publish_loop(event_rx, cancel.clone()),
        persist::run_persister(repo_clone, persister_rx),
        tui::run_tui(tui_rx, repo.clone(), cancel_clone),
    )?;

    let _ = (broker_result, tui_result);
    finalize_or_discard(repo, persister_result, &cancel).await
}

/// Either announce the recorded build, or — if the user cancelled — delete the
/// partial DB rows so they don't pollute baselines and clutter `chrono ls`.
///
/// "Cancelled" is detected via the shared `CancellationToken`, which is fired
/// by the Ctrl-C signal handler in `main()` and by the TUI on `q` / Ctrl-C.
/// Real cargo failures (compile errors that exit non-zero on their own) keep
/// the build record so the user can still inspect what happened.
async fn finalize_or_discard(
    repo: Arc<dyn BuildRepository>,
    build_id: BuildId,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    if cancel.is_cancelled() {
        repo.delete_build(build_id).await?;
        eprintln!("Build interrupted — not recorded.");
    } else {
        println!("Build {} recorded.", build_id);
    }
    Ok(())
}

/// List recent builds.
async fn cmd_ls(db_path: &std::path::Path, last: usize, format: cli::Format) -> anyhow::Result<()> {
    let repo = persist::SqliteRepository::open(db_path).await?;
    let builds = repo.list_builds(last).await?;
    match format {
        cli::Format::Text => cli::render_ls(&builds),
        cli::Format::Json => cli::json::render_ls_json(&builds)?,
    }
    Ok(())
}

/// Diff two builds.
async fn cmd_diff(
    db_path: &std::path::Path,
    before: i64,
    after: i64,
    format: cli::Format,
) -> anyhow::Result<()> {
    let repo = persist::SqliteRepository::open(db_path).await?;
    let build_diff = diff::compute_diff(&repo, BuildId(before), BuildId(after)).await?;
    match format {
        cli::Format::Text => cli::render_diff(&build_diff),
        cli::Format::Json => cli::json::render_diff_json(&build_diff)?,
    }
    Ok(())
}

/// Attempt to read the current git HEAD commit hash.
fn read_git_head() -> Option<String> {
    std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()
        .and_then(|output| {
            if output.status.success() {
                Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
            } else {
                None
            }
        })
}

/// Infer the build profile from cargo arguments.
fn infer_profile(cargo_args: &[String]) -> model::BuildProfile {
    if cargo_args.iter().any(|a| a == "--release") {
        model::BuildProfile::Release
    } else {
        model::BuildProfile::Dev
    }
}
