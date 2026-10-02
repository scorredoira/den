//! Hovering a name in the code while stopped shows its value: the whole
//! member chain up to the word under the mouse (`order.customer` on
//! `customer`), in a card that opens like the Variables panel.

use anyhow::Result;
use gpui_kit::component::input::{EditorState, HoverProvider, Rope, RopeExt as _};
use gpui_kit::*;
use lsp_types::Hover;

use super::{Debugger, expression_span};

pub struct DebugHover {
    pub debugger: WeakEntity<Debugger>,
    pub editor: WeakEntity<EditorState>,
}

impl HoverProvider for DebugHover {
    fn hover(&self, text: &Rope, offset: usize, _: &mut Window, cx: &mut App) -> Task<Result<Option<Hover>>> {
        let Some(debugger) = self.debugger.upgrade() else {
            return Task::ready(Ok(None));
        };
        let row = text.offset_to_point(offset).row;
        let start = text.line_start_offset(row);
        let line = text.slice_line(row).to_string();
        let span = expression_span(&line, offset - start)
            .filter(|_| debugger.read(cx).is_stopped())
            // keywords are names too, but have no value
            .filter(|span| {
                !matches!(&line[span.clone()], "let" | "const" | "var" | "function" | "return" | "if" | "else" | "for" | "while" | "new" | "export" | "import" | "true" | "false" | "null" | "undefined")
            });
        let Some(span) = span else {
            debugger.update(cx, |debugger, cx| debugger.clear_hover(cx));
            return Task::ready(Ok(None));
        };
        let expr = line[span.clone()].to_string();
        let range = start + span.start..start + span.end;
        let editor = self.editor.clone();
        // The editor is being updated while it asks: where the name is
        // drawn is read once it is done.
        cx.spawn(async move |cx| {
            let anchor = editor.read_with(cx, |editor, _| editor.range_to_bounds(&range)).ok().flatten();
            if let Some(anchor) = anchor {
                debugger.update(cx, |debugger, _| debugger.show_hover(expr, anchor));
            }
            Ok(None)
        })
    }
}

/// The hover's card, over everything: the Debugger draws it.
pub struct HoverCard {
    debugger: Entity<Debugger>,
    _observe: Subscription,
}

impl HoverCard {
    pub fn new(debugger: Entity<Debugger>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&debugger, |_, _, cx| cx.notify());
        Self { debugger, _observe: observe }
    }
}

impl Render for HoverCard {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.debugger.update(cx, |debugger, cx| debugger.render_hover(cx)) {
            Some(card) => card,
            None => Empty.into_any_element(),
        }
    }
}
