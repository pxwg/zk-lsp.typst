use tower_lsp::lsp_types::*;

use super::diagnostics::DiagnosticData;

/// Build code actions from diagnostics with source "zk-lsp".
pub fn get_code_actions(uri: &Url, diagnostics: &[Diagnostic]) -> Vec<CodeActionOrCommand> {
    let mut actions = Vec::new();

    for diag in diagnostics {
        if diag.source.as_deref() != Some("zk-lsp") {
            continue;
        }
        let data: DiagnosticData = match diag
            .data
            .as_ref()
            .and_then(|d| serde_json::from_value(d.clone()).ok())
        {
            Some(d) => d,
            None => continue,
        };
        let Some(ref new_id) = data.new_id else {
            continue;
        };

        let old_text = format!("@{}", data.old_id);
        let new_text = format!("@{new_id}");

        // Action 1: Replace @old with @new
        actions.push(make_replace_action(
            uri,
            diag,
            format!("Fix: Replace {old_text} with {new_text}"),
            new_text.clone(),
        ));

        // Action 2 (legacy only): Append @old @new
        if data.kind == "legacy" {
            let append_text = format!("{old_text} {new_text}");
            actions.push(make_replace_action(
                uri,
                diag,
                format!("Fix: Append new insight ({old_text} {new_text})"),
                append_text,
            ));
        }
    }

    actions
}

fn make_replace_action(
    uri: &Url,
    diag: &Diagnostic,
    title: String,
    new_text: String,
) -> CodeActionOrCommand {
    let edit = WorkspaceEdit {
        changes: Some(
            [(
                uri.clone(),
                vec![TextEdit {
                    range: diag.range,
                    new_text,
                }],
            )]
            .into_iter()
            .collect(),
        ),
        ..Default::default()
    };
    CodeActionOrCommand::CodeAction(CodeAction {
        title,
        kind: Some(CodeActionKind::QUICKFIX),
        diagnostics: Some(vec![diag.clone()]),
        edit: Some(edit),
        ..Default::default()
    })
}
