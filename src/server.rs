use std::collections::HashMap;
use std::sync::Arc;

use serde_json::Value;
use tokio::sync::RwLock;
use tower_lsp::jsonrpc::Result as LspResult;
use tower_lsp::lsp_types::*;
use tower_lsp::{Client, LanguageServer};
use tracing::{error, info};

use crate::config::WikiConfig;
use crate::handlers::{
    code_actions, completion, definition, diagnostics, hover, inlay_hints, references,
};
use crate::index::NoteIndex;
use crate::note_ops::MetaOverrides;
use crate::{link_gen, metadata, note_ops, parser, reconcile, watcher};

pub struct ZkLspServer {
    client: Client,
    index: Arc<NoteIndex>,
    config: Arc<RwLock<WikiConfig>>,
    cli_root: Option<std::path::PathBuf>,
    open_documents: Arc<RwLock<HashMap<Url, String>>>,
}

impl ZkLspServer {
    pub fn new(
        client: Client,
        config: Arc<RwLock<WikiConfig>>,
        cli_root: Option<std::path::PathBuf>,
    ) -> Self {
        let index = Arc::new(NoteIndex::new(Arc::clone(&config)));
        ZkLspServer {
            client,
            index,
            config,
            cli_root,
            open_documents: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    async fn current_config(&self) -> WikiConfig {
        self.config.read().await.clone()
    }

    async fn document_in_scope(&self, uri: &Url) -> bool {
        let Ok(path) = uri.to_file_path() else {
            return false;
        };
        let config = self.current_config().await;
        is_workspace_document_path(&config, &path)
    }

    async fn publish_diagnostics(&self, uri: Url, content: &str) {
        let file_path = uri.to_file_path().unwrap_or_default();
        let config = self.current_config().await;
        if !is_workspace_document_path(&config, &file_path) {
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
            return;
        }
        if is_metadata_path(&config, &file_path) {
            self.client
                .publish_diagnostics(
                    uri,
                    diagnostics::get_metadata_index_diagnostics_with_context(
                        content,
                        &config.zk_config.metadata.fields,
                        Some(&config.note_dir),
                    ),
                    None,
                )
                .await;
            return;
        }

        let mut diags = diagnostics::get_diagnostics(content, &self.index, uri.path());
        diags.extend(diagnostics::get_schema_diagnostics_for_note(
            content,
            path_note_id(&file_path),
        ));
        diags.extend(get_note_metadata_diagnostics(content, &config).await);
        if let Ok(reconcile_diags) =
            reconcile::collect_diagnostics(&config, Some((&file_path, content))).await
        {
            diags.extend(diagnostics::get_reconcile_diagnostics(
                content,
                &file_path,
                &reconcile_diags,
            ));
        }
        if let Some(d) = diagnostics::get_orphan_diagnostic(content, uri.path(), &self.index) {
            diags.push(d);
        }
        self.client.publish_diagnostics(uri, diags, None).await;
    }

    async fn read_document(&self, uri: &Url) -> Option<String> {
        let open_content = self.open_documents.read().await.get(uri).cloned();
        read_document_from_sources(open_content, uri)
    }

    async fn read_metadata_document(&self, config: &WikiConfig) -> Option<(Url, String)> {
        let path = metadata::metadata_path(&config.root);
        let uri = Url::from_file_path(&path).ok()?;
        let content = self.read_document(&uri).await?;
        Some((uri, content))
    }
}

#[tower_lsp::async_trait]
impl LanguageServer for ZkLspServer {
    async fn initialize(&self, params: InitializeParams) -> LspResult<InitializeResult> {
        let init_root = WikiConfig::lsp_root(&params);
        let resolved = WikiConfig::resolve(self.cli_root.clone(), init_root);
        let resolved_root = resolved.root.clone();
        *self.config.write().await = resolved;
        info!("initialize: resolved root to {}", resolved_root.display());

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
                definition_provider: Some(OneOf::Left(true)),
                references_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Simple(true)),
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(completion_trigger_characters()),
                    resolve_provider: Some(false),
                    ..Default::default()
                }),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
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
        let client = self.client.clone();
        let open_documents = Arc::clone(&self.open_documents);

        tokio::spawn(async move {
            match index.rebuild_full().await {
                Ok(n) => {
                    info!("index built: {n} notes");
                    for (uri, content) in open_documents.read().await.iter() {
                        let file_path = uri.to_file_path().unwrap_or_default();
                        let current_config = config.read().await.clone();
                        if !is_workspace_document_path(&current_config, &file_path) {
                            client
                                .publish_diagnostics(uri.clone(), Vec::new(), None)
                                .await;
                            continue;
                        }
                        if is_metadata_path(&current_config, &file_path) {
                            client
                                .publish_diagnostics(
                                    uri.clone(),
                                    diagnostics::get_metadata_index_diagnostics_with_context(
                                        content,
                                        &current_config.zk_config.metadata.fields,
                                        Some(&current_config.note_dir),
                                    ),
                                    None,
                                )
                                .await;
                            continue;
                        }
                        let mut diags = diagnostics::get_diagnostics(content, &index, uri.path());
                        diags.extend(diagnostics::get_schema_diagnostics_for_note(
                            content,
                            path_note_id(&file_path),
                        ));
                        diags.extend(get_note_metadata_diagnostics(content, &current_config).await);
                        if let Ok(reconcile_diags) = reconcile::collect_diagnostics(
                            &current_config,
                            Some((&file_path, content)),
                        )
                        .await
                        {
                            diags.extend(diagnostics::get_reconcile_diagnostics(
                                content,
                                &file_path,
                                &reconcile_diags,
                            ));
                        }
                        if let Some(d) =
                            diagnostics::get_orphan_diagnostic(content, uri.path(), &index)
                        {
                            diags.push(d);
                        }
                        client.publish_diagnostics(uri.clone(), diags, None).await;
                    }
                    // Tell the client to re-request inlay hints now that the index is ready.
                    // Ask the client to re-fetch all inlay hints now that the
                    // index is populated.
                    let _ = client.inlay_hint_refresh().await;
                }
                Err(e) => error!("index build failed: {e}"),
            }
            // Start filesystem watcher
            if let Err(e) = watcher::start_watcher(config, index).await {
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
        if !self.document_in_scope(&uri).await {
            self.open_documents.write().await.remove(&uri);
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
            return;
        }

        self.open_documents
            .write()
            .await
            .insert(uri.clone(), content.clone());
        // Update index for this file
        if let Ok(path) = uri.to_file_path() {
            let config = self.current_config().await;
            if !is_scoped_note_path(&config, &path) {
                self.publish_diagnostics(uri, &content).await;
                return;
            }
            let _ = self.index.update_file(&path).await;
        }
        self.publish_diagnostics(uri, &content).await;
        // Tell VS Code to re-request inlay hints for this newly opened file.
        // Without this, VS Code caches the empty first response and never retries.
        let _ = self.client.inlay_hint_refresh().await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        if !self.document_in_scope(&uri).await {
            self.open_documents.write().await.remove(&uri);
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
            return;
        }

        let initial_content = self.read_document(&uri).await.unwrap_or_default();
        let updated_content = {
            let mut documents = self.open_documents.write().await;
            let content = documents.entry(uri.clone()).or_insert(initial_content);
            apply_content_changes(content, &params.content_changes);
            content.clone()
        };

        self.publish_diagnostics(uri, &updated_content).await;
    }

    async fn did_save(&self, params: DidSaveTextDocumentParams) {
        let uri = params.text_document.uri.clone();
        if !self.document_in_scope(&uri).await {
            self.open_documents.write().await.remove(&uri);
            self.client.publish_diagnostics(uri, Vec::new(), None).await;
            return;
        }

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

        self.open_documents
            .write()
            .await
            .insert(uri.clone(), content.clone());

        // Update index
        if let Ok(path) = uri.to_file_path() {
            let config = self.current_config().await;
            if is_metadata_path(&config, &path) {
                self.publish_diagnostics(uri.clone(), &content).await;
                let _ = self.index.rebuild_full().await;
                let _ = self.client.inlay_hint_refresh().await;
                return;
            }
            if !is_scoped_note_path(&config, &path) {
                self.publish_diagnostics(uri.clone(), &content).await;
                return;
            }
            let _ = self.index.update_file(&path).await;
        }

        // Publish diagnostics for the saved file
        self.publish_diagnostics(uri.clone(), &content).await;
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        self.open_documents.write().await.remove(&uri);
        self.client.publish_diagnostics(uri, Vec::new(), None).await;
    }

    async fn did_change_watched_files(&self, params: DidChangeWatchedFilesParams) {
        for change in params.changes {
            let uri = change.uri.clone();
            if let Ok(path) = uri.to_file_path() {
                let config = self.current_config().await;
                if !is_workspace_document_path(&config, &path) {
                    self.client.publish_diagnostics(uri, Vec::new(), None).await;
                    continue;
                }
                if is_metadata_path(&config, &path) {
                    if let Ok(content) = tokio::fs::read_to_string(&path).await {
                        self.publish_diagnostics(uri, &content).await;
                    }
                    let _ = self.index.rebuild_full().await;
                    let _ = self.client.inlay_hint_refresh().await;
                    continue;
                }

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
    // Definition
    // -----------------------------------------------------------------------

    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> LspResult<Option<GotoDefinitionResponse>> {
        let uri = &params.text_document_position_params.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let position = params.text_document_position_params.position;
        let content = self.read_document(uri).await.unwrap_or_default();
        let config = self.current_config().await;
        let path = uri.to_file_path().unwrap_or_default();

        if is_metadata_path(&config, &path) {
            return Ok(
                definition::get_metadata_definition(&content, position, &self.index)
                    .map(GotoDefinitionResponse::Scalar),
            );
        }
        if !is_scoped_note_path(&config, &path) {
            return Ok(None);
        }

        if let Some(link) = definition::get_note_ref_definition(&content, position, &self.index) {
            return Ok(Some(GotoDefinitionResponse::Link(vec![link])));
        }

        let location = self.read_metadata_document(&config).await.and_then(
            |(metadata_uri, metadata_content)| {
                definition::get_note_definition(
                    &content,
                    position,
                    &metadata_uri,
                    &metadata_content,
                )
            },
        );
        Ok(location.map(GotoDefinitionResponse::Scalar))
    }

    // -----------------------------------------------------------------------
    // References
    // -----------------------------------------------------------------------

    async fn references(&self, params: ReferenceParams) -> LspResult<Option<Vec<Location>>> {
        let uri = &params.text_document_position.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let content = match self.read_document(uri).await {
            Some(c) => c,
            None => return Ok(None),
        };
        let config = self.current_config().await;
        let Some((metadata_uri, metadata_content)) = self.read_metadata_document(&config).await
        else {
            return Ok(Some(Vec::new()));
        };
        let locs = references::find_references(
            &self.index,
            uri,
            &content,
            params.text_document_position.position,
            &metadata_uri,
            &metadata_content,
            params.context.include_declaration,
        );
        Ok(Some(locs))
    }

    // -----------------------------------------------------------------------
    // Code actions
    // -----------------------------------------------------------------------

    async fn code_action(&self, params: CodeActionParams) -> LspResult<Option<CodeActionResponse>> {
        let uri = &params.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let content = self.read_document(uri).await.unwrap_or_default();
        let mut actions = code_actions::get_code_actions(uri, &params.context.diagnostics);
        if let Ok(path) = uri.to_file_path() {
            let config = self.current_config().await;
            if is_metadata_path(&config, &path) {
                actions.extend(code_actions::get_metadata_actions(
                    uri,
                    &content,
                    params.range,
                ));
            }
        }
        Ok(Some(actions))
    }

    // -----------------------------------------------------------------------
    // Completion
    // -----------------------------------------------------------------------

    async fn completion(&self, params: CompletionParams) -> LspResult<Option<CompletionResponse>> {
        let uri = &params.text_document_position.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let position = params.text_document_position.position;
        let content = self.read_document(uri).await.unwrap_or_default();
        let config = self.current_config().await;
        let path = uri.to_file_path().unwrap_or_default();
        if is_metadata_path(&config, &path) {
            let items = completion::get_metadata_completions(
                &content,
                position,
                &self.index,
                &config.zk_config.metadata.fields,
            );
            return Ok(if items.is_empty() {
                None
            } else {
                Some(CompletionResponse::Array(items))
            });
        }
        if is_scoped_note_path(&config, &path) {
            return Ok(
                completion::get_note_ref_completions(&content, position, &self.index)
                    .map(CompletionResponse::List),
            );
        }
        Ok(None)
    }

    // -----------------------------------------------------------------------
    // Hover
    // -----------------------------------------------------------------------

    async fn hover(&self, params: HoverParams) -> LspResult<Option<Hover>> {
        let uri = &params.text_document_position_params.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let position = params.text_document_position_params.position;
        let content = self.read_document(uri).await.unwrap_or_default();
        let config = self.current_config().await;
        let path = uri.to_file_path().unwrap_or_default();
        if is_metadata_path(&config, &path) {
            return Ok(hover::get_metadata_hover(&content, position, &self.index));
        }
        if !is_scoped_note_path(&config, &path) {
            return Ok(None);
        }

        let Some(target) = hover::resolve_note_ref_hover(&content, position, &self.index) else {
            return Ok(None);
        };
        let Some(target_content) = self.read_document(&target.uri).await else {
            return Ok(None);
        };
        Ok(Some(hover::build_note_ref_hover(&target, &target_content)))
    }

    // -----------------------------------------------------------------------
    // Inlay hints
    // -----------------------------------------------------------------------

    async fn inlay_hint(&self, params: InlayHintParams) -> LspResult<Option<Vec<InlayHint>>> {
        let uri = &params.text_document.uri;
        if !self.document_in_scope(uri).await {
            return Ok(None);
        }

        let content = match self.read_document(uri).await {
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
            "zk.generateLinkTyp" => {
                let config = self.current_config().await;
                match link_gen::generate_link_typ(&config).await {
                    Ok(()) => info!("link.typ regenerated"),
                    Err(e) => error!("generate_link_typ: {e}"),
                }
            }
            "zk.newNote" => {
                let config = self.current_config().await;
                if self.metadata_document_is_dirty(&config).await {
                    self.client
                        .show_message(
                            MessageType::WARNING,
                            "Save metadata.toml before running zk.newNote",
                        )
                        .await;
                    return Ok(None);
                }
                match note_ops::create_note(&config, None, &MetaOverrides::new(), None, None).await
                {
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
                    let config = self.current_config().await;
                    if self.metadata_document_is_dirty(&config).await {
                        self.client
                            .show_message(
                                MessageType::WARNING,
                                "Save metadata.toml before running zk.removeNote",
                            )
                            .await;
                        return Ok(None);
                    }
                    match note_ops::delete_note(id, &config).await {
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
                let inverse = params
                    .arguments
                    .get(2)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let simple = params
                    .arguments
                    .get(3)
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let config = self.current_config().await;
                match crate::context_export::export_context(&id, depth, inverse, simple, &config)
                    .await
                {
                    Ok(text) => return Ok(Some(Value::String(text))),
                    Err(e) => error!("exportContext: {e}"),
                }
            }
            cmd => info!("unhandled command: {cmd}"),
        }
        Ok(None)
    }
}

impl ZkLspServer {
    async fn metadata_document_is_dirty(&self, config: &WikiConfig) -> bool {
        let documents = self.open_documents.read().await;
        metadata_document_is_dirty(&documents, config)
    }
}

fn completion_trigger_characters() -> Vec<String> {
    vec!["\"".into(), "=".into(), "[".into(), "@".into()]
}

fn read_document_from_sources(open_content: Option<String>, uri: &Url) -> Option<String> {
    open_content.or_else(|| {
        uri.to_file_path()
            .ok()
            .and_then(|path| std::fs::read_to_string(path).ok())
    })
}

fn is_scoped_note_path(config: &WikiConfig, path: &std::path::Path) -> bool {
    let note_dir = absolute_path(&config.note_dir);
    let path = absolute_path(path);

    path.extension().and_then(|e| e.to_str()) == Some("typ") && path.starts_with(&note_dir)
}

fn is_metadata_path(config: &WikiConfig, path: &std::path::Path) -> bool {
    absolute_path(path) == absolute_path(&config.root.join("metadata.toml"))
}

fn is_workspace_document_path(config: &WikiConfig, path: &std::path::Path) -> bool {
    is_scoped_note_path(config, path) || is_metadata_path(config, path)
}

async fn get_note_metadata_diagnostics(content: &str, config: &WikiConfig) -> Vec<Diagnostic> {
    let Some(header) = parser::parse_header(content) else {
        return Vec::new();
    };
    match metadata::read_record_table(config, &header.id).await {
        Ok(table) => {
            diagnostics::note_metadata_record_message(&table, &config.zk_config.metadata.fields)
                .map(|message| note_metadata_diagnostic(content, &header, message))
                .into_iter()
                .collect()
        }
        Err(err) => {
            let message = if err.chain().any(|cause| {
                cause
                    .to_string()
                    .contains("Missing metadata for current note")
            }) {
                "Missing metadata for current note"
            } else {
                "Invalid metadata for current note"
            };
            vec![note_metadata_diagnostic(content, &header, message)]
        }
    }
}

fn note_metadata_diagnostic(
    content: &str,
    header: &parser::NoteHeader,
    message: &str,
) -> Diagnostic {
    let line_text = content.lines().nth(header.title_line_idx).unwrap_or("");
    Diagnostic {
        range: Range {
            start: Position {
                line: header.title_line_idx as u32,
                character: 0,
            },
            end: Position {
                line: header.title_line_idx as u32,
                character: line_text.len() as u32,
            },
        },
        severity: Some(DiagnosticSeverity::ERROR),
        source: Some("zk-lsp".into()),
        message: message.to_string(),
        ..Default::default()
    }
}

fn path_note_id(path: &std::path::Path) -> Option<&str> {
    path.file_stem()
        .and_then(|stem| stem.to_str())
        .filter(|id| metadata::is_note_id(id))
}

fn metadata_document_is_dirty(open_documents: &HashMap<Url, String>, config: &WikiConfig) -> bool {
    let path = metadata::metadata_path(&config.root);
    let Some(uri) = Url::from_file_path(&path).ok() else {
        return false;
    };
    let Some(open_content) = open_documents.get(&uri) else {
        return false;
    };
    match std::fs::read_to_string(path) {
        Ok(disk_content) => disk_content != *open_content,
        Err(_) => true,
    }
}

fn absolute_path(path: &std::path::Path) -> std::path::PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."))
            .join(path)
    }
}

fn apply_content_changes(content: &mut String, changes: &[TextDocumentContentChangeEvent]) {
    for change in changes {
        match change.range {
            Some(range) => {
                let start = position_to_byte_offset(content, range.start);
                let end = position_to_byte_offset(content, range.end);
                content.replace_range(start..end, &change.text);
            }
            None => {
                *content = change.text.clone();
            }
        }
    }
}

fn position_to_byte_offset(content: &str, position: Position) -> usize {
    let line_start = if position.line == 0 {
        0
    } else {
        match content
            .match_indices('\n')
            .nth(position.line.saturating_sub(1) as usize)
            .map(|(idx, _)| idx + 1)
        {
            Some(offset) => offset,
            None => content.len(),
        }
    };

    let line_end = content[line_start..]
        .find('\n')
        .map(|idx| line_start + idx)
        .unwrap_or(content.len());
    let line = &content[line_start..line_end];

    let mut utf16_units = 0u32;
    for (byte_idx, ch) in line.char_indices() {
        if utf16_units >= position.character {
            return line_start + byte_idx;
        }
        utf16_units += ch.len_utf16() as u32;
    }

    line_end
}

#[cfg(test)]
mod tests {
    use super::{
        apply_content_changes, completion_trigger_characters, is_scoped_note_path,
        metadata_document_is_dirty, position_to_byte_offset, read_document_from_sources,
    };
    use crate::config::WikiConfig;
    use crate::parser;
    use std::collections::HashMap;
    use std::path::PathBuf;
    use tower_lsp::lsp_types::{Position, Range, TextDocumentContentChangeEvent, Url};

    #[test]
    fn completion_triggers_preserve_metadata_characters_and_add_at() {
        assert_eq!(completion_trigger_characters(), vec!["\"", "=", "[", "@"]);
    }

    #[test]
    fn scoped_note_path_only_accepts_typst_files_under_note_dir() {
        let root = PathBuf::from("/tmp/zk-lsp-scope-test/wiki");
        let config = WikiConfig::from_root(root.clone());

        assert!(is_scoped_note_path(
            &config,
            &root.join("note/2605220949.typ")
        ));
        assert!(!is_scoped_note_path(&config, &root.join("test/test.typ")));
        assert!(!is_scoped_note_path(
            &config,
            &root.join("note/2605220949.md")
        ));
        assert!(!is_scoped_note_path(
            &config,
            &PathBuf::from("/tmp/zk-lsp-scope-test/wiki-notes/note/2605220949.typ")
        ));
    }

    #[test]
    fn metadata_document_dirty_detects_unsaved_open_buffer() {
        let root = std::env::temp_dir().join(format!("zk-lsp-server-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let metadata_path = root.join("metadata.toml");
        std::fs::write(&metadata_path, "format-version = 1\n").unwrap();
        let config = WikiConfig::from_root(root.clone());
        let uri = Url::from_file_path(&metadata_path).unwrap();
        let mut open = HashMap::new();

        open.insert(uri.clone(), "format-version = 1\n".to_string());
        assert!(!metadata_document_is_dirty(&open, &config));

        open.insert(uri, "format-version = 1\n# unsaved\n".to_string());
        assert!(metadata_document_is_dirty(&open, &config));

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn note_hover_reads_unsaved_open_target_before_disk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("2603110001.typ");
        std::fs::write(&path, "disk content\n").unwrap();
        let uri = Url::from_file_path(path).unwrap();
        let mut open = HashMap::new();
        open.insert(uri.clone(), "unsaved content\n".to_string());

        assert_eq!(
            read_document_from_sources(open.get(&uri).cloned(), &uri).as_deref(),
            Some("unsaved content\n")
        );
        open.clear();
        assert_eq!(
            read_document_from_sources(open.get(&uri).cloned(), &uri).as_deref(),
            Some("disk content\n")
        );
        let missing = Url::from_file_path(dir.path().join("missing.typ")).unwrap();
        assert!(read_document_from_sources(None, &missing).is_none());
    }

    #[test]
    fn apply_content_changes_updates_incremental_edits() {
        let mut content = "第一行\nhello @1234567890\n".to_string();
        apply_content_changes(
            &mut content,
            &[TextDocumentContentChangeEvent {
                range: Some(Range {
                    start: Position {
                        line: 1,
                        character: 6,
                    },
                    end: Position {
                        line: 1,
                        character: 17,
                    },
                }),
                range_length: None,
                text: "@0000000001".into(),
            }],
        );

        assert_eq!(content, "第一行\nhello @0000000001\n");
    }

    #[test]
    fn position_to_byte_offset_handles_utf16_columns() {
        let content = "第一行\nab界c\n";
        let offset = position_to_byte_offset(
            content,
            Position {
                line: 1,
                character: parser::byte_to_utf16("ab界c", "ab界".len()),
            },
        );

        assert_eq!(&content[offset..], "c\n");
    }
}
