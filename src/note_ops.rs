use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;
use tokio::fs;

use crate::config::WikiConfig;
use crate::link_gen;

/// Create a new note with the current timestamp as ID.
/// Returns the path to the new file.
pub async fn create_note(config: &WikiConfig, with_metadata: bool) -> Result<PathBuf> {
    let id = Local::now().format("%y%m%d%H%M").to_string();
    fs::create_dir_all(&config.note_dir).await?;

    let path = config.note_dir.join(format!("{id}.typ"));
    if !path.exists() {
        let content = if with_metadata {
            format!(
                "/* Metadata:\nAliases: \nAbstract: \nKeyword: \nGenerated: true\n*/\n\
                 #import \"../include.typ\": *\n#show: zettel\n\n=  <{id}>\n#tag.\n\n"
            )
        } else {
            format!("#import \"../include.typ\": *\n#show: zettel\n\n=  <{id}>\n#tag.\n\n")
        };
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
