//! CLI definition and output rendering.
//!
//! Owned by the Integrator. Uses clap 4 derive macros for argument parsing.

pub mod json;

use clap::{Parser, Subcommand, ValueEnum};
use clap_complete::Shell;

use crate::model::{Build, BuildDiff, BuildId, CrateChange, DurationChange};

/// Output format for `ls` and `diff`.
///
/// `Text` is the default human-readable form. `Json` emits a single-line
/// JSON document on stdout, suitable for CI integrations and downstream
/// tooling. The JSON schema is defined in [`json`] and is treated as a
/// stable wire format.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum Format {
    /// Human-readable table / report (default).
    #[default]
    Text,
    /// Single-line JSON object on stdout.
    Json,
}

/// cargo-chronoscope — Cargo build performance observer.
#[derive(Parser, Debug)]
#[command(name = "cargo-chronoscope", version, about)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

/// Available subcommands.
#[derive(Subcommand, Debug)]
pub enum Command {
    /// Run a build and record it to the local database.
    Record {
        /// Arguments forwarded to `cargo build`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// Run a build with a real-time TUI dashboard.
    Watch {
        /// Arguments forwarded to `cargo build`.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cargo_args: Vec<String>,
    },

    /// List recent builds from the database.
    Ls {
        /// Number of most recent builds to display.
        #[arg(long, default_value = "10")]
        last: usize,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Compare two recorded builds.
    Diff {
        /// Build ID of the "before" build.
        before: i64,
        /// Build ID of the "after" build.
        after: i64,
        /// Output format.
        #[arg(long, value_enum, default_value_t = Format::Text)]
        format: Format,
    },

    /// Generate a shell completion script.
    Completions {
        /// Shell to generate completions for.
        #[arg(value_enum)]
        shell: Shell,
    },
}

/// Render a list of builds to stdout.
///
/// Displays a simple table with build ID, timestamp, profile, duration, and status.
pub fn render_ls(builds: &[Build]) {
    if builds.is_empty() {
        println!("No builds recorded yet.");
        return;
    }

    let header = format!(
        "{:<6} {:<20} {:<8} {:<10} {}",
        "ID", "Started", "Profile", "Duration", "Status"
    );
    println!("{header}");
    println!("{}", "-".repeat(60));

    for build in builds {
        let duration_str = match build.total_duration {
            Some(d) => format!("{:.1}s", d.as_secs_f64()),
            None => "—".to_string(),
        };
        let status = match build.success {
            Some(true) => "ok",
            Some(false) => "FAIL",
            None => "???",
        };
        println!(
            "{:<6} {:<20} {:<8} {:<10} {}",
            BuildId(build.id.0),
            &build.started_at[..std::cmp::min(19, build.started_at.len())],
            build.profile,
            duration_str,
            status,
        );
    }
}

/// Render a build diff to stdout.
///
/// Shows total duration change, per-crate changes (Added/Removed/Changed only —
/// Unchanged crates are summarised as a single count), and the head of each
/// critical path. Use `--verbose` (future) to see Unchanged entries.
pub fn render_diff(diff: &BuildDiff) {
    println!("Build {} → Build {}", diff.before, diff.after);
    render_duration_change("Total", &diff.total_change);
    println!();

    let mut added = Vec::new();
    let mut removed = Vec::new();
    let mut changed = Vec::new();
    let mut unchanged_count = 0usize;

    for change in &diff.crate_changes {
        match change {
            CrateChange::Added { .. } => added.push(change),
            CrateChange::Removed { .. } => removed.push(change),
            CrateChange::Changed { .. } => changed.push(change),
            CrateChange::Unchanged { .. } => unchanged_count += 1,
        }
    }

    // Sort Changed by absolute delta descending so the biggest movers come first.
    changed.sort_by_key(|c| match c {
        CrateChange::Changed { change, .. } => std::cmp::Reverse(change.abs_delta_ms.abs()),
        _ => std::cmp::Reverse(0),
    });

    if added.is_empty() && removed.is_empty() && changed.is_empty() {
        println!("  No crate-level changes.");
    } else {
        for change in changed.iter().chain(added.iter()).chain(removed.iter()) {
            match change {
                CrateChange::Added { crate_id, duration } => {
                    println!("  + {} (new) {:.2}s", crate_id, duration.as_secs_f64());
                }
                CrateChange::Removed { crate_id, duration } => {
                    println!("  - {} (gone) {:.2}s", crate_id, duration.as_secs_f64());
                }
                CrateChange::Changed { crate_id, change } => {
                    let arrow = if change.abs_delta_ms > 0 {
                        "▲"
                    } else {
                        "▼"
                    };
                    println!(
                        "  {} {} {:.2}s → {:.2}s ({:+.2}s, {:+.1}%)",
                        arrow,
                        crate_id,
                        change.before.as_secs_f64(),
                        change.after.as_secs_f64(),
                        change.abs_delta_ms as f64 / 1000.0,
                        change.pct_delta,
                    );
                }
                CrateChange::Unchanged { .. } => {}
            }
        }
    }

    if unchanged_count > 0 {
        println!("  … {} crates unchanged", unchanged_count);
    }

    println!();
    print_critical_path_diff(&diff.critical_path_before, &diff.critical_path_after);
}

/// Print a side-by-side comparison of the two critical paths.
///
/// The full path is always printed — it is the load-bearing metric for build
/// performance (longest dependency chain), and the actual bottleneck may sit
/// deep in the path. Pipe to `less` if the output is long.
///
/// Layout:
/// ```text
/// Critical path: 14 → 11 nodes (-3)
///
///    #   before              after
///   ─── ─────────────────── ───────────────────
///    1  cfg_if              memchr
///    2  equivalent          bytes
///    ...
///    5  foldhash            foldhash             ✓ same position
///    ...
///   12  version_check       —
///
///   removed: scopeguard, version_check, ryu
///   added:   (none)
/// ```
fn print_critical_path_diff(before: &[String], after: &[String]) {
    let len_b = before.len();
    let len_a = after.len();
    let delta = len_a as i64 - len_b as i64;
    let delta_str = match delta.cmp(&0) {
        std::cmp::Ordering::Greater => format!(" (+{})", delta),
        std::cmp::Ordering::Less => format!(" ({})", delta),
        std::cmp::Ordering::Equal => String::new(),
    };
    println!("Critical path: {} → {} nodes{}", len_b, len_a, delta_str);

    if len_b == 0 && len_a == 0 {
        println!("  (empty)");
        return;
    }

    let max_len = len_b.max(len_a);

    // Column width = longest name across the full path, capped at 28 to keep
    // total under ~80 cols.
    let col_width = before
        .iter()
        .chain(after.iter())
        .map(|s| s.chars().count())
        .max()
        .unwrap_or(20)
        .clamp(6, 28);

    // Index column width grows with the path length so 100+ nodes still align.
    let idx_width = max_len.to_string().len().max(3);

    println!();
    println!(
        "  {:>idx$}  {:<width$}  {:<width$}",
        "#",
        "before",
        "after",
        idx = idx_width,
        width = col_width,
    );
    println!(
        "  {}  {}  {}",
        "─".repeat(idx_width),
        "─".repeat(col_width),
        "─".repeat(col_width),
    );

    for i in 0..max_len {
        let b = before.get(i).map(|s| s.as_str()).unwrap_or("—");
        let a = after.get(i).map(|s| s.as_str()).unwrap_or("—");
        let marker = if before.get(i).is_some() && before.get(i) == after.get(i) {
            "  ✓"
        } else {
            ""
        };
        println!(
            "  {:>idx$}  {:<width$}  {:<width$}{}",
            i + 1,
            truncate(b, col_width),
            truncate(a, col_width),
            marker,
            idx = idx_width,
            width = col_width,
        );
    }

    // Set diff: which crate names are unique to each path.
    let before_set: std::collections::HashSet<&str> = before.iter().map(|s| s.as_str()).collect();
    let after_set: std::collections::HashSet<&str> = after.iter().map(|s| s.as_str()).collect();
    let removed: Vec<&str> = before
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !after_set.contains(s))
        .collect();
    let added: Vec<&str> = after
        .iter()
        .map(|s| s.as_str())
        .filter(|s| !before_set.contains(s))
        .collect();

    println!();
    if !removed.is_empty() {
        println!("  removed from path: {}", removed.join(", "));
    }
    if !added.is_empty() {
        println!("  added to path:     {}", added.join(", "));
    }
    if removed.is_empty() && added.is_empty() && len_b > 0 && len_a > 0 {
        println!("  same crate set, possibly reordered");
    }
}

fn truncate(s: &str, width: usize) -> String {
    let count = s.chars().count();
    if count <= width {
        s.to_string()
    } else if width <= 1 {
        "…".to_string()
    } else {
        let mut out: String = s.chars().take(width - 1).collect();
        out.push('…');
        out
    }
}

fn render_duration_change(label: &str, change: &DurationChange) {
    println!(
        "  {}: {:.1}s → {:.1}s ({:+.1}s, {:+.1}%)",
        label,
        change.before.as_secs_f64(),
        change.after.as_secs_f64(),
        change.abs_delta_ms as f64 / 1000.0,
        change.pct_delta,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Build, BuildDiff, BuildId, CrateChange, CrateId, DurationChange};
    use std::time::Duration;

    // ---- Cli::parse tests (pass with current implementation) -------------

    #[test]
    fn parse_ls_defaults_last_to_10() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "ls"]).unwrap();
        match cli.command {
            Command::Ls { last, format } => {
                assert_eq!(last, 10);
                assert_eq!(format, Format::Text);
            }
            other => panic!("expected Ls, got {:?}", other),
        }
    }

    #[test]
    fn parse_ls_with_explicit_last() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "ls", "--last", "5"]).unwrap();
        match cli.command {
            Command::Ls { last, format } => {
                assert_eq!(last, 5);
                assert_eq!(format, Format::Text);
            }
            _ => panic!("expected Ls"),
        }
    }

    #[test]
    fn parse_ls_with_json_format() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "ls", "--format", "json"]).unwrap();
        match cli.command {
            Command::Ls { format, .. } => assert_eq!(format, Format::Json),
            _ => panic!("expected Ls"),
        }
    }

    #[test]
    fn parse_ls_rejects_unknown_format() {
        let result = Cli::try_parse_from(["cargo-chronoscope", "ls", "--format", "yaml"]);
        assert!(result.is_err());
    }

    #[test]
    fn parse_record_forwards_cargo_args() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "record", "--release", "-p", "demo"])
            .unwrap();
        match cli.command {
            Command::Record { cargo_args } => {
                assert_eq!(cargo_args, vec!["--release", "-p", "demo"]);
            }
            _ => panic!("expected Record"),
        }
    }

    #[test]
    fn parse_watch_forwards_cargo_args() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "watch", "--release"]).unwrap();
        match cli.command {
            Command::Watch { cargo_args } => {
                assert_eq!(cargo_args, vec!["--release"]);
            }
            _ => panic!("expected Watch"),
        }
    }

    #[test]
    fn parse_diff_requires_two_ids() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "diff", "3", "7"]).unwrap();
        match cli.command {
            Command::Diff {
                before,
                after,
                format,
            } => {
                assert_eq!(before, 3);
                assert_eq!(after, 7);
                assert_eq!(format, Format::Text);
            }
            _ => panic!("expected Diff"),
        }
    }

    #[test]
    fn parse_diff_with_json_format() {
        let cli = Cli::try_parse_from(["cargo-chronoscope", "diff", "1", "2", "--format", "json"])
            .unwrap();
        match cli.command {
            Command::Diff { format, .. } => assert_eq!(format, Format::Json),
            _ => panic!("expected Diff"),
        }
    }

    #[test]
    fn parse_diff_missing_args_errors() {
        let result = Cli::try_parse_from(["cargo-chronoscope", "diff", "1"]);
        assert!(result.is_err());
    }

    // ---- render smoke tests (pass with current implementation) -----------

    #[test]
    fn render_ls_empty_does_not_panic() {
        render_ls(&[]);
    }

    #[test]
    fn render_ls_with_rows_does_not_panic() {
        let builds = vec![Build {
            id: BuildId(1),
            started_at: "2025-04-19T12:00:00Z".to_string(),
            finished_at: Some("2025-04-19T12:00:05Z".to_string()),
            commit_hash: Some("abc123".to_string()),
            cargo_args: "[\"build\"]".to_string(),
            profile: "dev".to_string(),
            success: Some(true),
            total_duration: Some(Duration::from_millis(5000)),
        }];
        render_ls(&builds);
    }

    #[test]
    fn render_diff_does_not_panic() {
        let diff = BuildDiff {
            before: BuildId(1),
            after: BuildId(2),
            total_change: DurationChange {
                before: Duration::from_secs(10),
                after: Duration::from_secs(12),
                abs_delta_ms: 2000,
                pct_delta: 20.0,
            },
            crate_changes: vec![
                CrateChange::Added {
                    crate_id: CrateId {
                        name: "new-crate".into(),
                        version: None,
                    },
                    duration: Duration::from_millis(500),
                },
                CrateChange::Changed {
                    crate_id: CrateId {
                        name: "foo".into(),
                        version: None,
                    },
                    change: DurationChange {
                        before: Duration::from_secs(1),
                        after: Duration::from_secs(2),
                        abs_delta_ms: 1000,
                        pct_delta: 100.0,
                    },
                },
            ],
            critical_path_before: vec!["a".into(), "b".into()],
            critical_path_after: vec!["a".into(), "b".into(), "c".into()],
        };
        render_diff(&diff);
    }

    #[test]
    fn render_diff_empty_crate_changes_does_not_panic() {
        let diff = BuildDiff {
            before: BuildId(1),
            after: BuildId(2),
            total_change: DurationChange {
                before: Duration::from_secs(1),
                after: Duration::from_secs(1),
                abs_delta_ms: 0,
                pct_delta: 0.0,
            },
            crate_changes: vec![],
            critical_path_before: vec![],
            critical_path_after: vec![],
        };
        render_diff(&diff);
    }
}
