//! Cmd-click in the editor: a `path[:line[:column]]` or a URL in the text
//! opens it, as in a terminal; anything else goes to its definition, from the
//! task's language server. Held Cmd underlines the paths and URLs only: the
//! server is asked on the click, not as the pointer moves.

use std::path::{Path, PathBuf};

use anyhow::Result;
use gpui_kit::component::input::{DefinitionProvider, EditorState, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{LocationLink, Position, Range, ShowDocumentParams, Uri};
use proto::{LspOp, Request, Response};
use ui_term::links::Link;

use crate::workspace::Workspace;

pub struct Definitions {
    pub workspace: WeakEntity<Workspace>,
    pub editor: WeakEntity<EditorState>,
}

impl Definitions {
    /// The path or URL at `offset`, as where it goes.
    fn link(&self, text: &Rope, offset: usize, cx: &App) -> Option<LocationLink> {
        let (workspace, editor) = (self.workspace.upgrade()?, self.editor.upgrade()?);
        let at = text.offset_to_position(offset);
        let line = text.slice_line(at.line as usize).to_string();
        let (link, chars) = workspace.read(cx).link_at(&editor, &line, at.character as usize)?;
        let origin = Range::new(Position::new(at.line, chars.start as u32), Position::new(at.line, chars.end as u32));
        let (target_uri, target) = match link {
            Link::Url(url) => (url.parse().ok()?, Position::default()),
            Link::Path { path, line, column } => (
                file_uri(&path)?,
                Position::new(line.unwrap_or(1).saturating_sub(1), column.unwrap_or(1).saturating_sub(1)),
            ),
        };
        let target = Range::new(target, target);
        Some(LocationLink { origin_selection_range: Some(origin), target_uri, target_range: target, target_selection_range: target })
    }
}

impl DefinitionProvider for Definitions {
    fn definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Vec<LocationLink>>> {
        if let Some(link) = self.link(text, offset, cx) {
            return Task::ready(Ok(vec![link]));
        }
        let none = || Task::ready(Ok(Vec::new()));
        let (Some(workspace), Some(editor)) = (self.workspace.upgrade(), self.editor.upgrade()) else {
            return none();
        };
        let Some((client, root, path)) = workspace.read(cx).completion_target(&editor) else {
            return none();
        };
        let at = text.offset_to_position(offset);
        let request = Request::Lsp { root, path, text: text.to_string(), line: at.line, column: at.character, op: LspOp::Definition };
        cx.spawn(async move |cx| {
            let response = client.request(request).await;
            workspace.update(cx, |workspace, cx| workspace.report_lsp(&editor, &response, cx));
            let Response::Lsp { locations, .. } = response? else {
                return Ok(Vec::new());
            };
            Ok(locations
                .into_iter()
                .filter_map(|location| {
                    let start = Position::new(location.line, location.column);
                    let range = Range::new(start, Position::new(location.line, location.column + location.length));
                    Some(LocationLink {
                        origin_selection_range: None,
                        target_uri: file_uri(&location.path)?,
                        target_range: range,
                        target_selection_range: range,
                    })
                })
                .collect())
        })
    }

    /// Only what's known at once: asking the server as the pointer moves
    /// would send it the whole file at every word.
    fn hover_definitions(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Vec<LocationLink>>> {
        Task::ready(Ok(self.link(text, offset, cx).into_iter().collect()))
    }
}

/// Opens a file the editor goes to (a path clicked or a definition) in its
/// tab, at its place. URLs are left to the editor, which opens them.
pub fn show_document(workspace: WeakEntity<Workspace>, params: &ShowDocumentParams, window: &mut Window, cx: &mut App) -> bool {
    let Some(path) = path_of(&params.uri) else {
        return false;
    };
    let at = params.selection.map(|range| range.start).unwrap_or_default();
    let goto = gpui_kit::component::input::Position::new(at.line, at.character);
    workspace.update(cx, |workspace, cx| workspace.open_at_place(path, goto, window, cx)).is_ok()
}

/// `path` as a `file://` URI.
fn file_uri(path: &Path) -> Option<Uri> {
    let mut uri = String::from("file://");
    for byte in path.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => uri.push(byte as char),
            _ => uri.push_str(&format!("%{byte:02X}")),
        }
    }
    uri.parse().ok()
}

/// The path of a `file://` URI.
fn path_of(uri: &Uri) -> Option<PathBuf> {
    let uri = uri.as_str().strip_prefix("file://")?;
    let mut bytes = Vec::with_capacity(uri.len());
    let mut rest = uri.as_bytes();
    while let Some((&byte, after)) = rest.split_first() {
        match (byte, after.get(..2).and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok())) {
            (b'%', Some(decoded)) => {
                bytes.push(decoded);
                rest = &after[2..];
            }
            _ => {
                bytes.push(byte);
                rest = after;
            }
        }
    }
    Some(PathBuf::from(String::from_utf8(bytes).ok()?))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{file_uri, path_of};

    #[test]
    fn paths_go_through_uris_unchanged() {
        for path in ["/a/b.rs", "/with space/ñ.md", "/odd#name?.txt"] {
            assert_eq!(path_of(&file_uri(Path::new(path)).unwrap()).unwrap(), Path::new(path));
        }
    }
}
