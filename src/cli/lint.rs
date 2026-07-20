use anyhow::{Result, bail};
use clap::Args;
use std::path::PathBuf;

use graf::lint::{self, LintResult};
use graf::manifest::AppManifest;

#[derive(Args)]
pub struct LintArgs {
    /// Only check git-changed files (default: check all)
    #[arg(long)]
    pub changed: bool,

    /// Auto-fix what can be fixed (timestamps, etc.)
    #[arg(long)]
    pub fix: bool,

    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    pub json: bool,

    /// Repo slug or path (default: all manifest repos, or current directory)
    #[arg(long)]
    pub repo: Option<String>,

    /// Specific files to lint (relative to repo root)
    #[arg(value_name = "FILES")]
    pub files: Vec<PathBuf>,
}

pub fn run(args: LintArgs) -> Result<()> {
    if args.changed && !args.files.is_empty() {
        bail!("--changed and explicit file paths are mutually exclusive");
    }

    let manifest = AppManifest::load_default()?;
    let repos = if !args.files.is_empty() {
        vec![super::resolve_single_repo(
            args.repo.as_deref(),
            manifest.as_ref(),
        )?]
    } else {
        super::resolve_repos(args.repo.as_deref(), manifest.as_ref())?
    };

    let combined = super::multi_repo_lint(
        &repos,
        &super::MultiRepoLintOpts {
            fix: args.fix,
            changed: args.changed,
            files: args.files,
        },
    );

    output_and_exit(args.json, &combined)
}

/// Output lint results as JSON or human-readable text, then exit 1 on errors.
pub fn output_and_exit(json_flag: bool, result: &LintResult) -> ! {
    let use_json = json_flag || !std::io::IsTerminal::is_terminal(&std::io::stdout());
    if use_json {
        // unwrap: lint_result_to_json always produces valid JSON
        println!(
            "{}",
            serde_json::to_string(&lint::lint_result_to_json(result)).unwrap()
        );
    } else {
        print_results(result);
    }

    std::process::exit(if result.errors.is_empty() { 0 } else { 1 })
}

fn print_results(result: &LintResult) {
    for fixed in &result.files_fixed {
        println!("fixed: {}", fixed.display());
    }
    for err in &result.errors {
        eprintln!("error: {}: {}", err.path.display(), err.message);
    }
    if result.errors.is_empty() {
        println!(
            "ok: {} file{} checked, {} fixed",
            result.files_checked,
            if result.files_checked == 1 { "" } else { "s" },
            result.files_fixed.len()
        );
    }
}
