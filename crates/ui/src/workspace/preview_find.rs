//! Finding text in a Markdown preview, with the find bar of `find_bar`.
//! The matches are of the text as shown, the one Copy gives, and are
//! painted behind it; F3 and Shift-F3 also go from one to the next.

use std::ops::Range;

use gpui_base::{RangeHighlight, RenderedText};
use gpui_kit::component::input::{InputEvent, InputState};

use super::*;
use crate::find_bar;

/// The find bar of a preview, while it's open.
pub(super) struct PreviewFind {
    /// The preview it finds in.
    pub(super) text: Entity<TextViewState>,
    input: Entity<InputState>,
    invalid: bool,
    pub(super) matches: Vec<Range<usize>>,
    pub(super) active: Option<usize>,
    /// The text last searched: a change of it (the file saved) searches again.
    searched: Option<RenderedText>,
    _subscriptions: [Subscription; 2],
}

impl Workspace {
    /// Opens the find bar of the preview shown, or focuses it, with the
    /// selection of a line as the query.
    pub(super) fn open_preview_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.active.and_then(|ix| self.tabs[ix].rendered().cloned()) else {
            return;
        };
        if self.preview_find.as_ref().is_none_or(|find| find.text != text) {
            self.close_preview_find(window, cx);
            let input = find_bar::new_input(window, cx);
            let changed = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if let InputEvent::Change = event {
                    this.find_in_preview(true, cx);
                }
            });
            // Its text changed (the file saved): the matches again, the
            // highlights' own notify aside.
            let observed = cx.observe(&text, |this, text, cx| {
                let current = text.read(cx).rendered_text();
                if this.preview_find.as_ref().is_some_and(|find| find.searched.as_ref() != Some(&current)) {
                    this.find_in_preview(false, cx);
                }
            });
            self.preview_find = Some(PreviewFind {
                text: text.clone(),
                input,
                invalid: false,
                matches: Vec::new(),
                active: None,
                searched: None,
                _subscriptions: [changed, observed],
            });
        }
        let selected = text.read(cx).selected_text();
        let Some(find) = &self.preview_find else { return };
        find_bar::focus_input(&find.input.clone(), &selected, window, cx);
        self.find_in_preview(true, cx);
    }

    /// Closes the find bar, its highlights with it; the focus goes back to the workspace.
    pub(super) fn close_preview_find(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(find) = self.preview_find.take() else { return };
        let focused = find.input.read(cx).focus_handle(cx).contains_focused(window, cx);
        find.text.update(cx, |text, cx| text.clear_range_highlights(cx));
        if focused {
            self.focus_handle.focus(window, cx);
        }
        cx.notify();
    }

    /// The matches of the query in the preview's text. `from_start`: the
    /// query changed, the first match is active; else the text did, and the
    /// active one stays where it was.
    fn find_in_preview(&mut self, from_start: bool, cx: &mut Context<Self>) {
        let options = find_bar::options(cx);
        let Some(find) = &mut self.preview_find else { return };
        let query = find.input.read(cx).value().to_string();
        let rendered = find.text.read(cx).rendered_text();
        let previous = find.active.and_then(|ix| find.matches.get(ix)).map(|found| found.start);
        (find.matches, find.invalid) = match find_bar::pattern(&query, &options) {
            Ok(Some(pattern)) => (find_bar::find_in(rendered.as_str(), &pattern, find_bar::MAX_MATCHES).collect(), false),
            Ok(None) => (Vec::new(), false),
            Err(()) => (Vec::new(), true),
        };
        find.active = match previous.filter(|_| !from_start) {
            Some(start) => (!find.matches.is_empty())
                .then(|| find.matches.partition_point(|found| found.start < start).min(find.matches.len() - 1)),
            None => (!find.matches.is_empty()).then_some(0),
        };
        find.searched = Some(rendered);
        self.show_preview_matches(from_start, cx);
    }

    /// Paints the matches, the active one stronger; and scrolls to the
    /// active one when `reveal`.
    fn show_preview_matches(&mut self, reveal: bool, cx: &mut Context<Self>) {
        let Some(find) = &self.preview_find else { return };
        let (others, active) = find_bar::colors(cx);
        let highlights: Vec<RangeHighlight> = find
            .matches
            .iter()
            .enumerate()
            .map(|(ix, found)| RangeHighlight::new(found.clone(), if find.active == Some(ix) { active } else { others }))
            .collect();
        let target = find.active.map(|ix| find.matches[ix].clone()).filter(|_| reveal);
        find.text.update(cx, |text, cx| {
            // Ranges of the text just searched: they can't be invalid.
            text.set_range_highlights(highlights, cx).ok();
            if let Some(range) = target {
                text.reveal_range(range, cx).ok();
            }
        });
        cx.notify();
    }

    /// The next match or the previous one. False with no find bar open on
    /// the preview shown.
    pub(super) fn step_preview_find(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        let shown = self.active.and_then(|ix| self.tabs[ix].rendered().cloned());
        let Some(find) = self.preview_find.as_mut().filter(|find| Some(&find.text) == shown.as_ref()) else {
            return false;
        };
        if let Some(ix) = find_bar::step(find.active, find.matches.len(), forward) {
            find.active = Some(ix);
            self.show_preview_matches(true, cx);
        }
        true
    }

    /// The find bar over the preview of `text`, if it's open on it.
    pub(super) fn render_preview_find(&self, text: &Entity<TextViewState>, cx: &mut Context<Self>) -> Option<AnyElement> {
        let find = self.preview_find.as_ref().filter(|find| &find.text == text)?;
        let status = find_bar::Status { invalid: find.invalid, count: find.matches.len(), active: find.active };
        let handlers = find_bar::Handlers {
            step: |this: &mut Self, forward, cx| {
                this.step_preview_find(forward, cx);
            },
            close: Self::close_preview_find,
            toggled: |this: &mut Self, cx| this.find_in_preview(true, cx),
        };
        Some(find_bar::render(&find.input, status, handlers, cx))
    }
}
