use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;
use tokio::fs;

use crate::config::WikiConfig;
use crate::link_gen;

const DEFAULT_METADATA_BLOCK: &str = "#let zk-metadata = toml(bytes(\n \
  ```toml\n \
  schema-version = 1\n \
  aliases = []\n \
  abstract = \"\"\n \
  keywords = []\n \
  generated = true\n \
  checklist-status = \"none\"\n \
  relation = \"active\"\n \
  relation-target = []\n \
  ```.text,\n\
))";

async fn build_note_content(id: &str, wiki_root: &std::path::Path) -> String {
    if let Ok(raw) = tokio::fs::read_to_string(wiki_root.join("zk-lsp.toml")).await {
        if let Ok(table) = raw.parse::<toml::Table>() {
            if let Some(tmpl) = table
                .get("new_note")
                .and_then(|v| v.get("template"))
                .and_then(|v| v.as_str())
            {
                return tmpl
                    .replace("{{id}}", id)
                    .replace("{{metadata}}", DEFAULT_METADATA_BLOCK);
            }
        }
    }
    format!(
        "#import \"../include.typ\": *\n\
         {DEFAULT_METADATA_BLOCK}\n\
         #show: zettel.with(metadata: zk-metadata)\n\
         \n\
         =  <{id}>\n"
    )
}

/// Create a new note with the current timestamp as ID.
/// Returns the path to the new file.
pub async fn create_note(config: &WikiConfig) -> Result<PathBuf> {
    let id = Local::now().format("%y%m%d%H%M").to_string();
    fs::create_dir_all(&config.note_dir).await?;

    let path = config.note_dir.join(format!("{id}.typ"));
    if !path.exists() {
        let content = build_note_content(&id, &config.root).await;
        fs::write(&path, &content)
            .await
            .with_context(|| format!("writing note {}", path.display()))?;
    }

    link_gen::add_entry(&id, config).await?;
    Ok(path)
}

/// Delete a note and remove its entry from link.typ.
pub async fn delete_note(id: &str, config: &WikiConfig) -> Result<()> {
    let path = config.note_dir.join(format!("{id}.typ"));
    if path.exists() {
        fs::remove_file(&path)
            .await
            .with_context(|| format!("deleting note {}", path.display()))?;
    }
    link_gen::remove_entry(id, config).await?;
    Ok(())
}
