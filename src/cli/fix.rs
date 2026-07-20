use anyhow::Result;
use clap::Args;

use graf::lint::{self, LintOptions};
use graf::manifest::AppManifest;

#[derive(Args)]
pub struct FixArgs {
    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    pub json: bool,

    /// Repo slug or path (default: current directory)
    #[arg(long)]
    pub repo: Option<String>,
}

pub fn run(args: FixArgs) -> Result<()> {
    let manifest = AppManifest::load_default()?;
    let resolved = super::resolve_single_repo(args.repo.as_deref(), manifest.as_ref())?;
    let options = LintOptions {
        fix: true,
        repo_dir: resolved.path,
    };
    let result = lint::lint_changed(&options)?;
    super::lint::output_and_exit(args.json, &result)
}
