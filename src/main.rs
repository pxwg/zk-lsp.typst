mod cli;
mod config;
mod context_export;
mod cycle;
mod dependency_graph;
mod graph_check;
mod handlers;
mod index;
mod link_gen;
mod migrate;
mod note_ops;
mod parser;
mod reconcile;
mod server;
mod watcher;

use clap::Parser;
use tower_lsp::{LspService, Server};
use tracing_subscriber::{fmt, EnvFilter};

use cli::{Cli, Command};
use config::WikiConfig;
use server::ZkLspServer;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Tracing writes to stderr (stdout reserved for JSON-RPC)
    fmt()
        .with_env_filter(
            EnvFilter::try_from_env("ZK_LSP_LOG").unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let config = std::sync::Arc::new(WikiConfig::resolve(cli.wiki_root, None));

    match cli.command.unwrap_or(Command::Lsp) {
        Command::Lsp => {
            run_lsp(config).await?;
        }
        Command::Generate => {
            link_gen::generate_link_typ(&config).await?;
            eprintln!("link.typ regenerated at {}", config.link_file.display());
        }
        Command::New => {
            let path = note_ops::create_note(&config).await?;
            println!("{}", path.display());
        }
        Command::Remove { id } => {
            note_ops::delete_note(&id, &config).await?;
            eprintln!("Note {id} removed.");
        }
        Command::Format => {
            use std::io::Read;
            let mut content = String::new();
            std::io::stdin().read_to_string(&mut content)?;
            let formatted =
                handlers::formatting::format_content(&content, &config.note_dir).await;
            print!("{formatted}");
        }
        Command::Migrate => {
            eprintln!("Migrating legacy notes in {} …", config.note_dir.display());
            let stats = migrate::migrate_wiki(&config).await?;
            eprintln!(
                "Done: {} migrated, {} already current, {} skipped.",
                stats.migrated, stats.already_current, stats.skipped
            );
        }
        Command::Reconcile { dry_run } => {
            let stats = reconcile::run_reconcile(&config, dry_run).await?;
            eprintln!("Reconcile: {} file(s) changed", stats.files_changed);
        }
        Command::Export { id, depth, inverse } => {
            let out = context_export::export_context(&id, depth, inverse, &config).await?;
            print!("{out}");
        }
        Command::Check { no_orphans, no_dead_links } => {
            let mut report = graph_check::check_graph(&config).await?;
            let has_dead_links = !report.dead_links.is_empty();
            if no_dead_links {
                report.dead_links.clear();
            }
            if no_orphans {
                report.orphans.clear();
            }
            let rendered = graph_check::render_check_report(&report);
            print!("{rendered}");
            if has_dead_links && !no_dead_links {
                std::process::exit(1);
            }
        }
    }
    Ok(())
}

async fn run_lsp(config: std::sync::Arc<WikiConfig>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| ZkLspServer::new(client, config));
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}
