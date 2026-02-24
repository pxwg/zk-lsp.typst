use std::sync::Arc;

use tower_lsp::lsp_types::*;

use crate::index::NoteIndex;

/// Find all references to the note whose ID appears at the cursor position.
pub fn find_references(
    index: &Arc<NoteIndex>,
    uri: &Url,
    line_text: &str,
) -> Vec<Location> {
    // Extract the ID from the title label `<ID>` on the cursor line, or from any `@ID`
    let id = extract_id_from_line(line_text);
    let id = match id {
        Some(id) => id,
        None => return vec![],
    };

    index
        .get_backlinks(&id)
        .into_iter()
        .map(|loc| Location {
            uri: Url::from_file_path(&loc.file).unwrap_or_else(|_| uri.clone()),
            range: Range {
                start: Position { line: loc.line, character: loc.start_char },
                end: Position { line: loc.line, character: loc.end_char },
            },
        })
        .collect()
}

fn extract_id_from_line(line: &str) -> Option<String> {
    // Try `<ID>` first (title line format)
    if let Some(id) = extract_angle_id(line) {
        return Some(id);
    }
    // Try `@ID` at any position (the first one on the line)
    extract_at_id(line)
}

fn extract_angle_id(line: &str) -> Option<String> {
    let start = line.rfind('<')?;
    let end = line[start..].find('>')? + start;
    let candidate = &line[start + 1..end];
    if candidate.len() == 10 && candidate.chars().all(|c| c.is_ascii_digit()) {
        Some(candidate.to_string())
    } else {
        None
    }
}

fn extract_at_id(line: &str) -> Option<String> {
    let at = line.find('@')?;
    let rest = &line[at + 1..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    let candidate = &rest[..end];
    if candidate.len() == 10 {
        Some(candidate.to_string())
    } else {
        None
    }
}
