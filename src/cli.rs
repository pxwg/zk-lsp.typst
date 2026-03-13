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
    New,
    /// Delete a note and remove it from link.typ
    Remove {
        /// The 10-digit note ID (YYMMDDHHMM)
        id: String,
    },
    /// Format a note: read from stdin, write formatted content to stdout
    Format,
    /// Migrate legacy comment-format notes to TOML schema v1
    Migrate,
    /// Reconcile cross-file checkbox states across the whole wiki
    Reconcile {
        /// Show what would change without writing any files
        #[arg(long, default_value_t = false)]
        dry_run: bool,
    },
    /// Export a BFS context document from an entry note (for AI consumption)
    Export {
        /// Entry note ID (10-digit YYMMDDHHMM)
        id: String,
        /// BFS traversal depth
        #[arg(long, short, default_value_t = 2)]
        depth: usize,
        /// Traverse inbound links instead of outgoing; output ancestors first, entry last
        #[arg(long, default_value_t = false)]
        inverse: bool,
    },
    /// Check graph integrity: dead links and orphan notes
    Check {
        /// Only report dead links (skip orphan check)
        #[arg(long)]
        no_orphans: bool,
        /// Only report orphans (skip dead link check)
        #[arg(long)]
        no_dead_links: bool,
    },
    /// Initialise a new wiki in the current directory (or --wiki-root)
    Init,
    /// Output a single note's metadata as JSON
    NoteInfo {
        /// The 10-digit note ID (YYMMDDHHMM)
        id: String,
    },
}
