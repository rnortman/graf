use anyhow::Result;
use clap::Args;

#[derive(Args)]
pub struct TraverseArgs {
    /// Path to the starting document
    pub path: String,

    /// Maximum traversal depth
    #[arg(long, default_value_t = 2)]
    pub depth: u32,
}

pub fn run(_args: TraverseArgs) -> Result<()> {
    todo!("traverse not yet implemented")
}
