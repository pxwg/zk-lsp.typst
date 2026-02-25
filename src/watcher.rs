use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use notify_debouncer_mini::{new_debouncer, notify::RecursiveMode, DebouncedEvent};
use tokio::sync::mpsc;
use tracing::{error, info};

use crate::config::WikiConfig;
use crate::index::NoteIndex;
use crate::link_gen;

/// Start the filesystem watcher on note_dir.
/// Sends events (Create / Modify / Remove) to the returned receiver.
pub fn start_watcher(
    config: Arc<WikiConfig>,
    index: Arc<NoteIndex>,
) -> Result<tokio::task::JoinHandle<()>> {
    let (tx, mut rx) = mpsc::channel::<Vec<DebouncedEvent>>(64);

    let note_dir = config.note_dir.clone();

    // Spawn the blocking watcher thread
    std::thread::spawn(move || {
        let _rt = tokio::runtime::Handle::try_current();
        let (fs_tx, fs_rx) = std::sync::mpsc::channel();
        let mut debouncer = new_debouncer(Duration::from_millis(300), fs_tx).expect("debouncer");
        debouncer
            .watcher()
            .watch(&note_dir, RecursiveMode::NonRecursive)
            .expect("watch note_dir");

        for result in fs_rx {
            match result {
                Ok(events) => {
                    let _ = tx.blocking_send(events);
                }
                Err(e) => {
                    error!("watcher error: {e:?}");
                }
            }
        }
    });

    let handle = tokio::spawn(async move {
        while let Some(events) = rx.recv().await {
            for event in events {
                let path = event.path.clone();
                if !is_note_file(&path) {
                    continue;
                }
                if path.exists() {
                    info!("note changed/created: {}", path.display());
                    let _ = index.update_file(&path).await;
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let _ = link_gen::add_entry(&id, &config).await;
                } else {
                    info!("note removed: {}", path.display());
                    index.remove_by_path(&path);
                    let id = path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .unwrap_or("")
                        .to_string();
                    let _ = link_gen::remove_entry(&id, &config).await;
                }
            }
        }
    });

    Ok(handle)
}

fn is_note_file(path: &PathBuf) -> bool {
    if path.extension().and_then(|e| e.to_str()) != Some("typ") {
        return false;
    }
    path.file_stem()
        .and_then(|s| s.to_str())
        .map(|s| s.len() == 10 && s.chars().all(|c| c.is_ascii_digit()))
        .unwrap_or(false)
}
