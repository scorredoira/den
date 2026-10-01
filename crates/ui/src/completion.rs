//! Completions as you type, from the task's language server: the agent asks
//! it once per word, and while the word grows the menu filters what came back
//! (unless the server said the list was incomplete).

use std::{cell::RefCell, rc::Rc};

use anyhow::Result;
use gpui_kit::component::input::{CompletionProvider, EditorState, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{
    CompletionContext, CompletionItem, CompletionResponse, CompletionTextEdit, Documentation, MarkupContent, MarkupKind,
    Position, Range, TextEdit,
};
use nucleo_matcher::{
    Config, Matcher, Utf32Str,
    pattern::{CaseMatching, Normalization, Pattern},
};
use proto::{LspCompletion, LspOp, Request, Response};

use crate::workspace::Workspace;

/// The menu shows at most this many.
const SHOWN: usize = 200;

pub struct Completions {
    workspace: WeakEntity<Workspace>,
    editor: WeakEntity<EditorState>,
    last: Rc<RefCell<Option<Answer>>>,
}

/// The server's complete answer (its `list`) for the word at `line`, `start`
/// when it was `prefix`, after `before` on its line.
struct Answer {
    list: u64,
    line: u32,
    start: usize,
    before: String,
    prefix: String,
    items: Vec<LspCompletion>,
}

impl Completions {
    pub fn new(workspace: WeakEntity<Workspace>, editor: WeakEntity<EditorState>) -> Self {
        Self { workspace, editor, last: Default::default() }
    }
}

impl CompletionProvider for Completions {
    fn completions(
        &self,
        text: &Rope,
        offset: usize,
        _: CompletionContext,
        _: &mut Window,
        cx: &mut App,
    ) -> Task<Result<CompletionResponse>> {
        let none = || Task::ready(Ok(CompletionResponse::Array(Vec::new())));
        let position = text.offset_to_position(offset);
        let (line, column) = (position.line, position.character);
        let chars: Vec<char> = text.slice_line(line as usize).chars().take(column as usize).collect();
        let start = chars.len() - chars.iter().rev().take_while(|ch| is_word(**ch)).count();
        let before: String = chars[..start].iter().collect();
        let prefix: String = chars[start..].iter().collect();
        let after_trigger = [".", "::", "->"].iter().any(|trigger| before.ends_with(trigger));
        if (prefix.is_empty() && !after_trigger) || prefix.starts_with(|ch: char| ch.is_ascii_digit()) {
            *self.last.borrow_mut() = None;
            return none();
        }
        if let Some(last) = self.last.borrow().as_ref()
            && (last.line, last.start, &last.before) == (line, start, &before)
            && prefix.starts_with(&last.prefix)
        {
            return Task::ready(Ok(CompletionResponse::Array(matching(last.list, &last.items, &prefix, line, column))));
        }
        let (Some(workspace), Some(editor)) = (self.workspace.upgrade(), self.editor.upgrade()) else {
            return none();
        };
        let Some((client, root, path)) = workspace.read(cx).completion_target(&editor) else {
            return none();
        };
        let request = Request::Lsp { root, path, text: text.to_string(), line, column, op: LspOp::Completion };
        let last = self.last.clone();
        cx.spawn(async move |cx| {
            let response = client.request(request).await;
            workspace.update(cx, |workspace, cx| workspace.report_lsp(&editor, &response, cx));
            let Response::Completions { server, list, items, incomplete } = response? else {
                return Ok(CompletionResponse::Array(Vec::new()));
            };
            let shown = matching(list, &items, &prefix, line, column);
            // A missing server isn't a cacheable empty completion list:
            // typing again must retry and allow the status to recover.
            *last.borrow_mut() = (server.is_some() && !incomplete).then_some(Answer { list, line, start, before, prefix, items });
            Ok(CompletionResponse::Array(shown))
        })
    }

    fn resolve_completion(&self, item: &CompletionItem, cx: &mut App) -> Task<Result<CompletionItem>> {
        let mut item = item.clone();
        let target = (self.workspace.upgrade(), self.editor.upgrade());
        let (Some(workspace), Some(editor)) = target else {
            return Task::ready(Ok(item));
        };
        let Some((client, root, path)) = workspace.read(cx).completion_target(&editor) else {
            return Task::ready(Ok(item));
        };
        let Some((list, ix)) = item.data.as_ref().and_then(|data| Some((data["list"].as_u64()?, data["item"].as_u64()?))) else {
            return Task::ready(Ok(item));
        };
        cx.spawn(async move |cx| {
            let request = Request::LspResolve { root, path, list, item: ix as u32 };
            let response = client.request(request).await;
            workspace.update(cx, |workspace, cx| workspace.report_lsp(&editor, &response, cx));
            if let Response::Resolved { detail, documentation } = response? {
                item.detail = detail;
                item.documentation = documentation.map(markdown);
            }
            Ok(item)
        })
    }

    /// One character typed or deleted: the menu follows the word. A paste
    /// closes it.
    fn is_completion_trigger(&self, _: usize, new_text: &str, _: &mut App) -> bool {
        new_text.chars().count() <= 1 && new_text != "\n"
    }
}

fn is_word(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_' || ch == '$'
}

/// Those that match what's typed, best first, as the menu's items: each one
/// replaces from where it starts up to the cursor.
fn matching(list: u64, items: &[LspCompletion], prefix: &str, line: u32, column: u32) -> Vec<CompletionItem> {
    let mut matcher = Matcher::new(Config::DEFAULT);
    let pattern = Pattern::parse(prefix, CaseMatching::Smart, Normalization::Smart);
    let mut buf = Vec::new();
    let mut scored: Vec<(u32, usize, &LspCompletion)> = items
        .iter()
        .enumerate()
        .filter_map(|(ix, item)| {
            let score = if prefix.is_empty() { 0 } else { pattern.score(Utf32Str::new(&item.filter, &mut buf), &mut matcher)? };
            Some((score, ix, item))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| (&a.2.sort, &a.2.label).cmp(&(&b.2.sort, &b.2.label))));
    scored
        .into_iter()
        .take(SHOWN)
        .map(|(_, ix, item)| CompletionItem {
            label: item.label.clone(),
            kind: item.kind.and_then(|kind| serde_json::from_value(kind.into()).ok()),
            detail: item.detail.clone(),
            documentation: item.documentation.clone().map(markdown),
            data: Some(serde_json::json!({ "list": list, "item": ix })),
            text_edit: Some(CompletionTextEdit::Edit(TextEdit {
                range: Range::new(Position::new(line, item.start.min(column)), Position::new(line, column)),
                new_text: item.text.clone(),
            })),
            ..Default::default()
        })
        .collect()
}

fn markdown(value: String) -> Documentation {
    Documentation::MarkupContent(MarkupContent { kind: MarkupKind::Markdown, value })
}

#[cfg(test)]
mod tests {
    use lsp_types::CompletionItem;
    use proto::LspCompletion;

    use super::matching;

    fn item(label: &str, sort: &str) -> LspCompletion {
        LspCompletion {
            label: label.into(),
            kind: None,
            detail: None,
            documentation: None,
            text: label.into(),
            start: 4,
            filter: label.into(),
            sort: sort.into(),
        }
    }

    #[test]
    fn best_first_then_by_sort_and_label() {
        let items = [item("save", "11"), item("addTable", "11"), item("MAX_SIZE", "11"), item("getDB", "10")];
        let labels = |shown: Vec<CompletionItem>| shown.into_iter().map(|item| item.label).collect::<Vec<_>>();
        assert_eq!(labels(matching(1, &items, "", 0, 4)), ["getDB", "MAX_SIZE", "addTable", "save"]);
        assert_eq!(labels(matching(1, &items, "gd", 0, 6)), ["getDB"]);
        let shown = matching(7, &items, "sa", 0, 6);
        assert_eq!(shown[0].data, Some(serde_json::json!({ "list": 7, "item": 0 })));
    }
}
