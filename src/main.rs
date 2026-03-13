mod cli;
mod config;
mod context_export;
#[allow(dead_code)]
mod hooks;
mod cycle;
mod dependency_graph;
mod graph_check;
mod handlers;
mod index;
mod init;
mod link_gen;
mod migrate;
mod note_ops;
mod parser;
mod reconcile;
mod server;
mod watcher;

use anyhow::Context;
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

    // `init` defaults to $PWD, not ~/wiki, so resolve its config before the
    // shared config (which defaults to ~/wiki for everything else).
    if matches!(cli.command, Some(Command::Init)) {
        let root = cli.wiki_root.clone().unwrap_or_else(|| {
            std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
        });
        let config = WikiConfig::from_root(root);
        return init::init_wiki(&config).await;
    }

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
                handlers::formatting::format_content(&content, &config).await;
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
        Command::Init => unreachable!("handled above"),
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
        Command::NoteInfo { id } => {
            let path = config.note_dir.join(format!("{id}.typ"));
            if !path.exists() {
                eprintln!("Note {id} not found at {}", path.display());
                std::process::exit(1);
            }
            let content = tokio::fs::read_to_string(&path)
                .await
                .with_context(|| format!("reading {}", path.display()))?;
            let header = parser::parse_header(&content)
                .ok_or_else(|| anyhow::anyhow!("Failed to parse note {id} (may be legacy format; run zk-lsp migrate first)"))?;
            let parsed_toml = parser::find_toml_metadata_block(&content)
                .and_then(|b| parser::parse_toml_metadata(&b.toml_content))
                .unwrap_or_default();
            let json = build_note_info_json(&id, &path, &header, &parsed_toml)?;
            println!("{json}");
        }
    }
    Ok(())
}

fn toml_value_to_json(v: &toml::Value) -> serde_json::Value {
    match v {
        toml::Value::String(s) => serde_json::Value::String(s.clone()),
        toml::Value::Integer(n) => serde_json::Value::Number((*n).into()),
        toml::Value::Float(f) => {
            serde_json::Number::from_f64(*f)
                .map(serde_json::Value::Number)
                .unwrap_or(serde_json::Value::Null)
        }
        toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
        toml::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(t) => {
            let map: serde_json::Map<String, serde_json::Value> =
                t.iter().map(|(k, v)| (k.clone(), toml_value_to_json(v))).collect();
            serde_json::Value::Object(map)
        }
        toml::Value::Datetime(dt) => serde_json::Value::String(dt.to_string()),
    }
}

fn build_note_info_json(
    id: &str,
    path: &std::path::Path,
    header: &parser::NoteHeader,
    parsed: &parser::ParsedToml,
) -> anyhow::Result<String> {
    use serde_json::{json, Map, Value};

    let checklist_status_str = match parsed.checklist_status {
        parser::ChecklistStatus::None => "none",
        parser::ChecklistStatus::Todo => "todo",
        parser::ChecklistStatus::Wip => "wip",
        parser::ChecklistStatus::Done => "done",
    };
    let relation_str = match parsed.relation {
        parser::Relation::Active => "active",
        parser::Relation::Archived => "archived",
        parser::Relation::Legacy => "legacy",
    };

    let mut metadata: Map<String, Value> = Map::new();
    metadata.insert("schema-version".into(), json!(parsed.schema_version));
    metadata.insert("aliases".into(), json!(parsed.aliases));
    metadata.insert(
        "abstract".into(),
        json!(parsed.abstract_text.as_deref().unwrap_or("")),
    );
    metadata.insert("keywords".into(), json!(parsed.keywords));
    metadata.insert("generated".into(), json!(parsed.generated));
    metadata.insert("checklist-status".into(), json!(checklist_status_str));
    metadata.insert("relation".into(), json!(relation_str));
    metadata.insert("relation-target".into(), json!(parsed.relation_target));

    // Merge extra (non-core) fields
    for (k, v) in &parsed.extra {
        metadata.insert(k.clone(), toml_value_to_json(v));
    }

    let output = json!({
        "id": id,
        "path": path.to_string_lossy().as_ref(),
        "title": header.title,
        "metadata": Value::Object(metadata),
    });

    Ok(serde_json::to_string_pretty(&output)?)
}

async fn run_lsp(config: std::sync::Arc<WikiConfig>) -> anyhow::Result<()> {
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let (service, socket) = LspService::new(|client| ZkLspServer::new(client, config));
    Server::new(stdin, stdout, socket).serve(service).await;
    Ok(())
}
