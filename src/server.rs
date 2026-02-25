use std::sync::Arc;

use serde_json::Value;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{error, info};

use crate::config::WikiConfig;
use crate::handlers::{code_actions, diagnostics, formatting, inlay_hints, references};
use crate::index::NoteIndex;
use crate::parser::StatusTag;
use crate::{link_gen, note_ops, watcher};

pub struct ZkLspServer {
    client: Client,
    index: Arc<NoteIndex>,
    config: Arc<WikiConfig>,
}

impl ZkLspServer {
    pub fn new(client: Client, config: Arc<WikiConfig>) -> Self {
        let index = Arc::new(NoteIndex::new(Arc::clone(&config)));
        ZkLspServer { client, index, config }
    }

    async fn publish_diagnostics(&self, uri: Url, content: &str) {
        let diags = diagnostics::get_diagnostics(content, &self.index, uri.path());
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
                        will_save: Some(true),
                        will_save_wait_until: Some(true),
                    },
                )),
                references_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
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
            None => match uri.to_file_path().ok().and_then(|p| {
                std::fs::read_to_string(p).ok()
            }) {
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

        // Cross-file tag propagation if the note's tag changed to Done/Wip
        if uri.path().contains("/note/") {
            if let Some(header) = crate::parser::parse_header(&content) {
                let todos = crate::parser::count_todos(&content);
                if let Some(new_tag) = crate::parser::compute_status_tag(&todos, header.archived)
                {
                    if new_tag == StatusTag::Done || new_tag == StatusTag::Wip {
                        match formatting::propagate_tag_change(
                            &header.id,
                            &new_tag,
                            &self.index,
                        )
                        .await
                        {
                            Ok(edit) => {
                                if edit.changes.as_ref().map(|c| !c.is_empty()).unwrap_or(false) {
                                    let _ = self.client.apply_edit(edit).await;
                                }
                            }
                            Err(e) => error!("propagate_tag_change: {e}"),
                        }
                    }
                }
            }
        }
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
    // willSaveWaitUntil — return tag edit before save is applied
    // -----------------------------------------------------------------------

    async fn will_save_wait_until(
        &self,
        params: WillSaveTextDocumentParams,
    ) -> LspResult<Option<Vec<TextEdit>>> {
        let uri = &params.text_document.uri;
        if !uri.path().contains("/note/") {
            return Ok(None);
        }
        let content = match uri.to_file_path().ok().and_then(|p| {
            std::fs::read_to_string(p).ok()
        }) {
            Some(c) => c,
            None => return Ok(None),
        };
        let edit = formatting::compute_tag_edit(&content);
        Ok(edit.map(|e| vec![e]))
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        let row = params.text_document_position.position.line as usize;
        let content = match uri.to_file_path().ok().and_then(|p| {
            std::fs::read_to_string(p).ok()
        }) {
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

    async fn code_action(
        &self,
        params: CodeActionParams,
    ) -> LspResult<Option<CodeActionResponse>> {
        let actions = code_actions::get_code_actions(
            &params.text_document.uri,
            &params.context.diagnostics,
        );
        Ok(Some(actions))
    }

    // -----------------------------------------------------------------------
    // Inlay hints
    // -----------------------------------------------------------------------

    async fn inlay_hint(
        &self,
        params: InlayHintParams,
    ) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        let content = match uri.to_file_path().ok().and_then(|p| {
            std::fs::read_to_string(p).ok()
        }) {
            Some(c) => c,
            None => return Ok(None),
        };
        let hints =
            inlay_hints::get_inlay_hints(&content, params.range, &self.index);
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

    async fn execute_command(
        &self,
        params: ExecuteCommandParams,
    ) -> LspResult<Option<Value>> {
        match params.command.as_str() {
            "zk.generateLinkTyp" => {
                match link_gen::generate_link_typ(&self.config).await {
                    Ok(()) => info!("link.typ regenerated"),
                    Err(e) => error!("generate_link_typ: {e}"),
                }
            }
            "zk.newNote" => {
                let with_meta = params
                    .arguments
                    .first()
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                match note_ops::create_note(&self.config, with_meta).await {
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
            cmd => info!("unhandled command: {cmd}"),
        }
        Ok(None)
    }
}
