//! Hovering a name in the code while stopped shows its value: the whole
//! member chain up to the word under the mouse (`order.customer` on
//! `customer`).

use anyhow::Result;
use gpui_kit::component::input::{HoverProvider, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::{Hover, HoverContents, MarkupContent, MarkupKind};

use super::{Debugger, expression_at};

pub struct DebugHover {
    pub debugger: WeakEntity<Debugger>,
}

impl HoverProvider for DebugHover {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Option<Hover>>> {
        let Some(debugger) = self.debugger.upgrade().filter(|debugger| debugger.read(cx).is_stopped()) else {
            return Task::ready(Ok(None));
        };
        let row = text.offset_to_point(offset).row;
        let start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let Some(expr) = expression_at(&line, offset - start) else {
            return Task::ready(Ok(None));
        };
        // keywords are names too, but have no value
        if matches!(expr.as_str(), "let" | "const" | "var" | "function" | "return" | "if" | "else" | "for" | "while" | "new" | "export" | "import" | "true" | "false" | "null" | "undefined") {
            return Task::ready(Ok(None));
        }

        let (tx, rx) = smol::channel::bounded(1);
        debugger.update(cx, |debugger, _| {
            debugger.evaluate(expr.clone(), move |_, result, _| {
                let _ = tx.try_send(result);
            })
        });
        cx.spawn(async move |_| {
            let Ok(Ok(var)) = rx.recv().await else {
                return Ok(None);
            };
            let mut value = format!("{expr} = {}", var.value);
            if !var.kind.is_empty() {
                value.push_str(&format!("\n// {}", var.kind));
                if var.count > 0 {
                    value.push_str(&format!(", {} entries", var.count));
                }
            }
            Ok(Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: format!("```ts\n{value}\n```"),
                }),
                range: None,
            }))
        })
    }
}
