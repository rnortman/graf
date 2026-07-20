use std::io::IsTerminal;

use anyhow::Result;
use clap::Args;

use graf::todo;

#[derive(Args)]
pub struct ReindexArgs {
    /// Repo slug or path (required when manifest is active)
    #[arg(long)]
    pub repo: Option<String>,

    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: ReindexArgs) -> Result<()> {
    let resolved = super::resolve_single_repo(args.repo.as_deref())?;
    let result = todo::reindex(&resolved.path)?;
    let json = args.json || !std::io::stdout().is_terminal();

    if json {
        let errors: Vec<serde_json::Value> = result
            .errors
            .iter()
            .map(|e| {
                serde_json::json!({
                    "path": e.path.display().to_string(),
                    "message": e.message,
                })
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&serde_json::json!({
                "files_indexed": result.files_indexed,
                "errors": errors,
            }))?
        );
    } else {
        for err in &result.errors {
            eprintln!("warning: {}: {}", err.path.display(), err.message);
        }

        println!(
            "{} file{} indexed, {} error{}",
            result.files_indexed,
            if result.files_indexed == 1 { "" } else { "s" },
            result.errors.len(),
            if result.errors.len() == 1 { "" } else { "s" },
        );
    }

    if result.files_indexed == 0 {
        std::process::exit(1);
    }

    Ok(())
}
