use anyhow::Result;
use clap::{Parser, Subcommand};

mod cli;

#[derive(Parser)]
#[command(name = "graf", about = "Personal knowledge base + todo CLI")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate frontmatter in markdown files
    Lint(cli::lint::LintArgs),

    /// Shorthand for `lint --changed --fix`
    Fix(cli::fix::FixArgs),

    /// Traverse the document graph from a starting point
    Traverse(cli::traverse::TraverseArgs),

    /// Run as an MCP server (JSON-RPC over stdio)
    Mcp(cli::mcp::McpArgs),

    /// Manage repo configs and the application manifest
    Manifest(cli::manifest::ManifestArgs),

    /// Rebuild the SQLite metadata cache from scratch
    Reindex(cli::reindex::ReindexArgs),

    /// Query and manage tasks
    Todo(cli::todo::TodoArgs),
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Lint(args) => cli::lint::run(args),
        Command::Fix(args) => cli::fix::run(args),
        Command::Traverse(args) => cli::traverse::run(args),
        Command::Mcp(args) => cli::mcp::run(args),
        Command::Manifest(args) => cli::manifest::run(args),
        Command::Reindex(args) => cli::reindex::run(args),
        Command::Todo(args) => cli::todo::run(args),
    }
}
