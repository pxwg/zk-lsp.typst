use std::path::PathBuf;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "zk-lsp", about = "Zettelkasten LSP server and CLI tools")]
pub struct Cli {
    /// Path to wiki root (overrides WIKI_ROOT env and ~/wiki default)
    #[arg(long, global = true)]
    pub wiki_root: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Subcommand)]
pub enum Command {
    /// Start the LSP server on stdin/stdout (default when no subcommand given)
    Lsp,
    /// Regenerate link.typ from the note directory
    Generate,
    /// Create a new note and print its path to stdout
    New {
        /// Include metadata block in the new note
        #[arg(long)]
        metadata: bool,
    },
    /// Delete a note and remove it from link.typ
    Remove {
        /// The 10-digit note ID (YYMMDDHHMM)
        id: String,
    },
    /// Format a note: read from stdin, write formatted content to stdout
    Format,
}
