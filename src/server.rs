use std::sync::Arc;

use serde_json::Value;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{error, info};

use crate::config::WikiConfig;
use crate::handlers::{code_actions, completion, diagnostics, inlay_hints, references};
use crate::index::NoteIndex;
use crate::{cycle, dependency_graph, link_gen, note_ops, watcher};

pub struct ZkLspServer {
    client: Client,
    index: Arc<NoteIndex>,
    config: Arc<WikiConfig>,
}

impl ZkLspServer {
    pub fn new(client: Client, config: Arc<WikiConfig>) -> Self {
        let index = Arc::new(NoteIndex::new(Arc::clone(&config)));
        ZkLspServer {
            client,
            index,
            config,
        }
    }

    async fn workspace_cycles(&self) -> Vec<cycle::DependencyCycle> {
        let Ok(mut rd) = tokio::fs::read_dir(&self.config.note_dir).await else {
            return vec![];
        };
        let mut notes = std::collections::HashMap::new();
        while let Some(entry) = rd.next_entry().await.ok().flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("typ") {
                continue;
            }
            let stem = match path.file_stem().and_then(|s| s.to_str()) {
                Some(s) if s.len() == 10 && s.chars().all(|c| c.is_ascii_digit()) => {
                    s.to_string()
                }
                _ => continue,
            };
            let Ok(content) = tokio::fs::read_to_string(&path).await else {
                continue;
            };
            notes.insert(stem, (path, content));
        }
        let graph = dependency_graph::build_dependency_graph(&notes);
        cycle::detect_cycles(&graph)
    }

    async fn publish_diagnostics(&self, uri: Url, content: &str) {
        let cycles = self.workspace_cycles().await;
        let file_path = uri.to_file_path().unwrap_or_default();
        let mut diags = diagnostics::get_diagnostics(content, &self.index, uri.path());
        diags.extend(diagnostics::get_cycle_diagnostics(content, &file_path, &cycles));
        diags.extend(diagnostics::get_schema_diagnostics(content, &self.index));
        if let Some(d) = diagnostics::get_orphan_diagnostic(content, uri.path(), &self.index) {
            diags.push(d);
        }
        self.client.publish_diagnostics(uri, diags, None).await;
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for ZkLspServer {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        // Honour initializationOptions.wikiRoot if the config wasn't set by CLI/env
        // (config is already resolved at server construction; this is just informational)
        info!("initialize: {:?}", params.root_uri);

        Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Options(
                    TextDocumentSyncOptions {
                        open_close: Some(true),
                        change: Some(TextDocumentSyncKind::INCREMENTAL),
                        save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions {
                            include_text: Some(true),
                        })),
                        will_save: None,
                        will_save_wait_until: None,
                    },
                )),
                references_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec!["\"".into(), "=".into(), "[".into()]),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                inlay_hint_provider: Some(OneOf::Left(true)),
                workspace_symbol_provider: Some(OneOf::Left(true)),
                execute_command_provider: Some(ExecuteCommandOptions {
                    commands: vec![
                        "zk.newNote".into(),
                        "zk.removeNote".into(),
                        "zk.generateLinkTyp".into(),
                        "zk.exportContext".into(),
                    ],
                    work_done_progress_options: Default::default(),
                }),
                ..Default::default()
            },
            server_info: Some(ServerInfo {
                name: "zk-lsp".into(),
                version: Some(env!("CARGO_PKG_VERSION").into()),
            }),
        })
    }

    async fn initialized(&self, _params: InitializedParams) {
        info!("server initialized, building index…");
        let index = Arc::clone(&self.index);
        let config = Arc::clone(&self.config);
        let _client = self.client.clone();

        tokio::spawn(async move {
            match index.rebuild_full().await {
                Ok(n) => info!("index built: {n} notes"),
                Err(e) => error!("index build failed: {e}"),
            }
            // Start filesystem watcher
            if let Err(e) = watcher::start_watcher(config, index) {
                error!("watcher start failed: {e}");
            }
        });
    }

    async fn shutdown(&self) -> LspResult<()> {
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Text document lifecycle
    // -----------------------------------------------------------------------

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let content = params.text_document.text;
        // Update index for this file
        if let Ok(path) = uri.to_file_path() {
            let _ = self.index.update_file(&path).await;
        }
        self.publish_diagnostics(uri, &content).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        let content = match params.text {
            Some(t) => t,
            None => match uri
                .to_file_path()
                .ok()
                .and_then(|p| std::fs::read_to_string(p).ok())
            {
                Some(t) => t,
                None => return,
            },
        };

        // Update index
        if let Ok(path) = uri.to_file_path() {
            let _ = self.index.update_file(&path).await;
        }

        // Publish diagnostics for the saved file
        self.publish_diagnostics(uri.clone(), &content).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            let uri = change.uri.clone();
            if let Ok(path) = uri.to_file_path() {
                match change.typ {
                    FileChangeType::CREATED | FileChangeType::CHANGED => {
                        let _ = self.index.update_file(&path).await;
                        if let Ok(content) = tokio::fs::read_to_string(&path).await {
                            self.publish_diagnostics(uri, &content).await;
                        }
                    }
                    FileChangeType::DELETED => {
                        self.index.remove_by_path(&path);
                    }
                    _ => {}
                }
            }
        }
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let row = params.text_document_position.position.line as usize;
        let content = match uri
            .to_file_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
        {
            Some(c) => c,
            None => return Ok(None),
        };
        let line = content.lines().nth(row).unwrap_or("");
        let locs = references::find_references(&self.index, uri, line);
        Ok(Some(locs))
    }

    // -----------------------------------------------------------------------
    // Code actions
    // -----------------------------------------------------------------------

    async fn code_action(&self, params: CodeActionParams) -> LspResult<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        let content = uri
            .to_file_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let mut actions = code_actions::get_code_actions(uri, &params.context.diagnostics);
        actions.extend(code_actions::get_metadata_actions(uri, &content, params.range));
        Ok(Some(actions))
    }

    // -----------------------------------------------------------------------
    // Completion
    // -----------------------------------------------------------------------

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let content = uri
            .to_file_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .unwrap_or_default();
        let items = completion::get_completions(&content, position, &self.index);
        Ok(if items.is_empty() { None } else { Some(CompletionResponse::Array(items)) })
    }

    // -----------------------------------------------------------------------
    // Inlay hints
    // -----------------------------------------------------------------------

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let content = match uri
            .to_file_path()
            .ok()
            .and_then(|p| std::fs::read_to_string(p).ok())
        {
            Some(c) => c,
            None => return Ok(None),
        };
        let hints = inlay_hints::get_inlay_hints(&content, params.range, &self.index);
        Ok(Some(hints))
    }

    // -----------------------------------------------------------------------
    // Workspace symbols
    // -----------------------------------------------------------------------

    async fn symbol(
        &self,
        params: WorkspaceSymbolParams,
    ) -> LspResult<Option<Vec<SymbolInformation>>> {
        #[allow(deprecated)]
        let symbols = self
            .index
            .search(&params.query)
            .into_iter()
            .map(|info| {
                let uri = Url::from_file_path(&info.path)
                    .unwrap_or_else(|_| Url::parse("file:///unknown").unwrap());
                SymbolInformation {
                    name: format!("[{}] {}", info.id, info.title),
                    kind: SymbolKind::FILE,
                    location: Location {
                        uri,
                        range: Range::default(),
                    },
                    tags: None,
                    deprecated: None,
                    container_name: None,
                }
            })
            .collect();
        Ok(Some(symbols))
    }

    // -----------------------------------------------------------------------
    // Execute command
    // -----------------------------------------------------------------------

    async fn execute_command(&self, params: ExecuteCommandParams) -> LspResult<Option<Value>> {
        match params.command.as_str() {
            "zk.generateLinkTyp" => match link_gen::generate_link_typ(&self.config).await {
                Ok(()) => info!("link.typ regenerated"),
                Err(e) => error!("generate_link_typ: {e}"),
            },
            "zk.newNote" => {
                match note_ops::create_note(&self.config).await {
                    Ok(path) => {
                        info!("created note: {}", path.display());
                        let uri = Url::from_file_path(&path).ok();
                        if let Some(uri) = uri {
                            self.client
                                .show_message(MessageType::INFO, format!("Created: {uri}"))
                                .await;
                        }
                    }
                    Err(e) => error!("create_note: {e}"),
                }
            }
            "zk.removeNote" => {
                if let Some(id) = params.arguments.first().and_then(|v| v.as_str()) {
                    match note_ops::delete_note(id, &self.config).await {
                        Ok(()) => info!("deleted note {id}"),
                        Err(e) => error!("delete_note: {e}"),
                    }
                }
            }
            "zk.exportContext" => {
                let id = params
                    .arguments
                    .first()
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let depth = params
                    .arguments
                    .get(1)
                    .and_then(|v| v.as_u64())
                    .unwrap_or(2) as usize;
                match crate::context_export::export_context(&id, depth, &self.config).await {
                    Ok(text) => return Ok(Some(Value::String(text))),
                    Err(e) => error!("exportContext: {e}"),
                }
            }
            cmd => info!("unhandled command: {cmd}"),
        }
        Ok(None)
    }
}
