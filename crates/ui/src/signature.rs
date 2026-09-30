//! The signature of the call being typed, above the cursor, with the
//! parameter it's on in bold (VS Code's parameter hints). It's asked for on
//! `(` and `,`, and again on each change or move along the line while shown;
//! it closes outside the call, on another line and with Escape.

use gpui_kit::component::{ActiveTheme as _, ThemeStyled as _, input::{EditorState, Position}};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::LspSignature;

pub struct SignatureHint {
    pub editor: Entity<EditorState>,
    pub signature: LspSignature,
}

/// Whether typing just before `cursor` opens the hint.
pub fn opens(text: &str, cursor: Position) -> bool {
    let Some(line) = text.lines().nth(cursor.line as usize) else {
        return false;
    };
    let before = cursor.character.checked_sub(1).and_then(|ix| line.chars().nth(ix as usize));
    matches!(before, Some('(' | ','))
}

/// Above the cursor, in window coordinates like the editor's layout; nothing
/// if the cursor isn't in view.
pub fn render(hint: &SignatureHint, cx: &App) -> Option<AnyElement> {
    let state = hint.editor.read(cx);
    let cursor = state.cursor();
    let at = state.range_to_bounds(&(cursor..cursor))?;
    let area = state.input_bounds();
    if at.origin.y < area.top() || at.bottom() > area.bottom() {
        return None;
    }
    let theme = cx.theme();
    let signature = &hint.signature;
    let highlights = signature
        .active
        .map(|(start, end)| {
            let byte = |chars: u32| signature.label.char_indices().nth(chars as usize).map_or(signature.label.len(), |(ix, _)| ix);
            let style = HighlightStyle { font_weight: Some(FontWeight::BOLD), color: Some(theme.blue), ..Default::default() };
            vec![(byte(start)..byte(end), style)]
        })
        .unwrap_or_default();
    let popover = div()
        .occlude()
        .popover_style(cx)
        .max_w(px(640.))
        .px_2()
        .py_1()
        .font_family(theme.mono_font_family.clone())
        .text_xs()
        .child(StyledText::new(signature.label.clone()).with_highlights(highlights))
        .when_some(signature.documentation.clone(), |this, documentation| {
            this.child(div().pt_1().text_color(theme.muted_foreground).font_family(theme.font_family.clone()).child(documentation))
        });
    Some(
        anchored()
            .anchor(Anchor::BottomLeft)
            .position(point(at.origin.x - px(8.), at.origin.y - px(2.)))
            .snap_to_window()
            .child(popover)
            .into_any_element(),
    )
}

#[cfg(test)]
mod tests {
    use gpui_kit::component::input::Position;

    use super::opens;

    #[test]
    fn opens_after_paren_or_comma() {
        let text = "a\n  getDB(x, ñ)";
        assert!(opens(text, Position::new(1, 8)));
        assert!(opens(text, Position::new(1, 10)));
        assert!(!opens(text, Position::new(1, 11)));
        assert!(!opens(text, Position::new(1, 7)));
        assert!(!opens(text, Position::new(1, 0)));
        assert!(!opens(text, Position::new(5, 1)));
    }
}
