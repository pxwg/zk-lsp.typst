use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use dashmap::DashMap;
use tokio::fs;

use crate::config::WikiConfig;
use crate::parser;

#[derive(Debug, Clone)]
pub struct NoteInfo {
    pub id: String,
    pub title: String,
    pub archived: bool,
    pub legacy: bool,
    pub alt_id: Option<String>,
    pub evo_id: Option<String>,
    pub aliases: Vec<String>,
    pub keywords: Vec<String>,
    pub abstract_text: Option<String>,
    pub path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct BacklinkLocation {
    pub file: PathBuf,
    pub line: u32,
    pub start_char: u32,
    pub end_char: u32,
}

pub struct NoteIndex {
    pub notes: Arc<DashMap<String, NoteInfo>>,
    pub backlinks: Arc<DashMap<String, Vec<BacklinkLocation>>>,
    pub config: Arc<WikiConfig>,
}

impl NoteIndex {
    pub fn new(config: Arc<WikiConfig>) -> Self {
        NoteIndex {
            notes: Arc::new(DashMap::new()),
            backlinks: Arc::new(DashMap::new()),
            config,
        }
    }

    /// Rebuild the full index by scanning all notes in note_dir.
    pub async fn rebuild_full(&self) -> Result<usize> {
        self.notes.clear();
        self.backlinks.clear();

        let mut entries = fs::read_dir(&self.config.note_dir).await?;
        let mut paths = Vec::new();
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("typ") {
                if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                    if stem.len() == 10 && stem.chars().all(|c| c.is_ascii_digit()) {
                        paths.push(path);
                    }
                }
            }
        }

        for path in &paths {
            let _ = self.index_file(path).await;
        }

        Ok(self.notes.len())
    }

    /// Update a single file in the index.
    pub async fn update_file(&self, path: &Path) -> Result<()> {
        // Remove old backlinks contributed by this file
        self.remove_backlinks_from(path);
        self.index_file(path).await
    }

    /// Remove a note from the index by its path.
    pub fn remove_by_path(&self, path: &Path) {
        if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
            self.notes.remove(stem);
        }
        self.remove_backlinks_from(path);
    }

    pub fn get(&self, id: &str) -> Option<NoteInfo> {
        self.notes.get(id).map(|r| r.clone())
    }

    /// Simple fuzzy search over title, aliases, keywords.
    pub fn search(&self, query: &str) -> Vec<NoteInfo> {
        let q = query.to_lowercase();
        self.notes
            .iter()
            .filter(|entry| {
                let n = entry.value();
                n.title.to_lowercase().contains(&q)
                    || n.id.contains(&q)
                    || n.aliases.iter().any(|a| a.to_lowercase().contains(&q))
                    || n.keywords.iter().any(|k| k.to_lowercase().contains(&q))
                    || n.abstract_text
                        .as_deref()
                        .map(|a| a.to_lowercase().contains(&q))
                        .unwrap_or(false)
            })
            .map(|entry| entry.value().clone())
            .collect()
    }

    /// Get all backlink locations for an ID.
    pub fn get_backlinks(&self, id: &str) -> Vec<BacklinkLocation> {
        self.backlinks
            .get(id)
            .map(|r| r.clone())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Private helpers
    // -----------------------------------------------------------------------

    async fn index_file(&self, path: &Path) -> Result<()> {
        let content = fs::read_to_string(path).await?;
        if let Some(header) = parser::parse_header(&content) {
            let info = NoteInfo {
                id: header.id.clone(),
                title: header.title.clone(),
                archived: header.archived,
                legacy: header.legacy,
                alt_id: header.alt_id.clone(),
                evo_id: header.evo_id.clone(),
                aliases: header.aliases.clone(),
                keywords: header.keywords.clone(),
                abstract_text: header.abstract_text.clone(),
                path: path.to_path_buf(),
            };
            self.notes.insert(header.id.clone(), info);
        }

        // Update backlinks from this file.
        // Convert byte offsets to UTF-16 code-unit offsets (required by LSP) here,
        // while the line text is available.
        let lines: Vec<&str> = content.lines().collect();
        let refs = parser::find_all_refs(&content);
        for r in refs {
            let line_text = lines.get(r.line as usize).copied().unwrap_or("");
            let loc = BacklinkLocation {
                file: path.to_path_buf(),
                line: r.line,
                start_char: parser::byte_to_utf16(line_text, r.start_char as usize),
                end_char: parser::byte_to_utf16(line_text, r.end_char as usize),
            };
            self.backlinks.entry(r.id).or_default().push(loc);
        }
        Ok(())
    }

    fn remove_backlinks_from(&self, path: &Path) {
        for mut entry in self.backlinks.iter_mut() {
            entry.value_mut().retain(|loc| loc.file != path);
        }
        // Remove empty entries
        self.backlinks.retain(|_, v| !v.is_empty());
    }
}
