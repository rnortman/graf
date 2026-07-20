//! CLI commands for managing repo configs and the application manifest.
//!
//! `graf manifest init`  — initialize a repo's `.graf/config.toml`
//! `graf manifest add`   — add a repo to the application manifest
//! `graf manifest list`  — list all entries in the application manifest
//! `graf manifest check` — validate the manifest against repo configs

use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::{Args, Subcommand};

use graf::identity::{GlobalId, Slug};
use graf::manifest::{AppManifest, ManifestRepo, RepoConfig};

use super::resolve_repo;

#[derive(Args)]
pub struct ManifestArgs {
    #[command(subcommand)]
    pub command: ManifestCommand,
}

#[derive(Subcommand)]
pub enum ManifestCommand {
    /// Initialize a repo's .graf/config.toml
    Init(InitArgs),
    /// Add a repo to the application manifest
    Add(AddArgs),
    /// List all entries in the application manifest
    List(ListArgs),
    /// Validate the application manifest against repo configs
    Check(CheckArgs),
}

#[derive(Args)]
pub struct InitArgs {
    /// Global ID for this repo (e.g., example.com/life)
    #[arg(long)]
    pub id: String,
    /// Suggested slug (defaults to last segment of ID)
    #[arg(long)]
    pub slug: Option<String>,
    /// Sharing domain ID (e.g., example.com/household)
    #[arg(long)]
    pub domain: Option<String>,
    /// Repo directory (defaults to current directory)
    #[arg(long)]
    pub repo: Option<PathBuf>,
}

#[derive(Args)]
pub struct AddArgs {
    /// Filesystem path to the repo directory
    pub path: String,
    /// Slug for this repo in the manifest (defaults to repo's default_slug)
    pub slug: Option<String>,
}

#[derive(Args)]
pub struct ListArgs {
    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    pub json: bool,
}

#[derive(Args)]
pub struct CheckArgs {
    /// JSON output (default when stdout is not a TTY)
    #[arg(long)]
    pub json: bool,
}

pub fn run(args: ManifestArgs) -> Result<()> {
    match args.command {
        ManifestCommand::Init(a) => run_init(a),
        ManifestCommand::Add(a) => run_add(a),
        ManifestCommand::List(a) => run_list(a),
        ManifestCommand::Check(a) => run_check(a),
    }
}

fn run_init(args: InitArgs) -> Result<()> {
    let repo_dir = resolve_repo(args.repo)?;
    let id = GlobalId::new(&args.id)?;

    let slug = match args.slug {
        Some(s) => Slug::new(&s)?,
        None => {
            let last = id.as_str().rsplit('/').next().unwrap();
            Slug::new(last).with_context(|| {
                format!("cannot derive slug from ID {id} — use --slug to specify one")
            })?
        }
    };

    let domain = args.domain.map(|d| GlobalId::new(&d)).transpose()?;

    let config_path = RepoConfig::path_in_repo(&repo_dir);
    if config_path.exists() {
        bail!(
            ".graf/config.toml already exists at {}",
            config_path.display()
        );
    }

    let config = RepoConfig {
        id: id.clone(),
        default_slug: slug.clone(),
        domain,
        refs: Default::default(),
    };

    config.save(&repo_dir)?;
    eprintln!("Initialized {id} (slug: {slug}) at {}", repo_dir.display());
    Ok(())
}

fn run_add(args: AddArgs) -> Result<()> {
    let expanded_path = AppManifest::expand_path(&args.path)?;

    // Read repo config to get the ID (and default_slug if needed)
    let config = RepoConfig::load(&expanded_path)?.ok_or_else(|| {
        anyhow::anyhow!(
            "no .graf/config.toml found at {} — run `graf manifest init` first",
            expanded_path.display()
        )
    })?;

    let slug = match args.slug {
        Some(s) => Slug::new(&s)?,
        None => config.default_slug.clone(),
    };

    let mut manifest = AppManifest::load_default()?.unwrap_or(AppManifest {
        repo: Vec::new(),
        domain: Vec::new(),
    });

    if manifest.find_repo_by_slug(&slug).is_some() {
        bail!("repo slug {:?} already exists in manifest", slug.as_str());
    }
    if manifest.find_repo_by_id(&config.id).is_some() {
        bail!("repo ID {} already exists in manifest", config.id);
    }

    manifest.repo.push(ManifestRepo {
        slug: slug.clone(),
        path: args.path.clone(),
        id: config.id.clone(),
    });

    for warning in manifest.warnings() {
        eprintln!("warning: {warning}");
    }

    let manifest_path = manifest.save_default()?;
    eprintln!(
        "Added {} ({}) to {}",
        slug,
        config.id,
        manifest_path.display()
    );
    Ok(())
}

fn run_list(args: ListArgs) -> Result<()> {
    let manifest = AppManifest::load_default()?
        .ok_or_else(|| anyhow::anyhow!("no manifest found — run `graf manifest add` first"))?;

    let json = args.json || !std::io::stdout().is_terminal();

    if json {
        println!(
            "{}",
            serde_json::to_string(&super::manifest_list_json(&manifest))?
        );
    } else {
        if !manifest.repo.is_empty() {
            println!("Repos:");
            for repo in &manifest.repo {
                let expanded = AppManifest::expand_path(&repo.path)?;
                println!(
                    "  {:10} {} ({})",
                    repo.slug.as_str(),
                    expanded.display(),
                    repo.id
                );
            }
        }

        if !manifest.domain.is_empty() {
            if !manifest.repo.is_empty() {
                println!();
            }
            println!("Domains:");
            for domain in &manifest.domain {
                println!("  {:10} {}", domain.slug.as_str(), domain.id);
            }
        }

        if manifest.repo.is_empty() && manifest.domain.is_empty() {
            println!("Manifest is empty.");
        }
    }

    Ok(())
}

fn run_check(args: CheckArgs) -> Result<()> {
    let manifest = AppManifest::load_default()?
        .ok_or_else(|| anyhow::anyhow!("no manifest found — run `graf manifest add` first"))?;

    let json = args.json || !std::io::stdout().is_terminal();

    if json {
        let result = super::manifest_check_json(&manifest);
        let has_errors = !result["ok"].as_bool().unwrap_or(true);
        println!("{}", serde_json::to_string(&result)?);
        if has_errors {
            std::process::exit(1);
        }
    } else {
        let mut errors = manifest.validate();
        errors.extend(manifest.validate_repos());
        let warnings = manifest.warnings();
        for warning in &warnings {
            eprintln!("warning: {warning}");
        }

        if errors.is_empty() {
            eprintln!(
                "Manifest OK ({} repos, {} domains)",
                manifest.repo.len(),
                manifest.domain.len()
            );
        } else {
            for error in &errors {
                eprintln!("error: {error}");
            }
            bail!("{} validation error(s)", errors.len());
        }
    }

    Ok(())
}
