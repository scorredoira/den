//! Search panel: searches the task's files (on the agent) as you type, and
//! groups the results by file, and replaces them. The References panel is the
//! same, without the boxes: its results (F12, Shift-F12) come from outside.

use std::{path::PathBuf, sync::Arc, time::Duration};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use proto::{Request, Response, SearchHit};

use crate::{config::UiText, menu};

/// Wait after the last keystroke before searching.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// Cap on matches per search.
const MAX_HITS: usize = 5_000;

pub enum SearchEvent {
    Open { file: String, line: u32, column: u32, pin: bool },
    /// Replace All was confirmed for these files: the workspace leaves out
    /// those with unsaved changes and calls `replace`.
    Replace { files: Vec<String> },
}

enum Row {
    File { path: String, count: usize },
    Hit(usize),
}

pub struct SearchPanel {
    client: Option<Arc<Client>>,
    root: PathBuf,
    input: Entity<InputState>,
    replacement: Entity<InputState>,
    regex: bool,
    case_sensitive: bool,
    /// Each replacement takes the case of what it replaces.
    preserve_case: bool,
    /// What the last Replace All did.
    replaced: Option<SharedString>,
    hits: Vec<SearchHit>,
    truncated: bool,
    selected: Option<usize>,
    searching: bool,
    error: Option<SharedString>,
    search: Option<Task<()>>,
    /// References panel: what is shown instead of the search box.
    title: Option<SharedString>,
    _subscription: Subscription,
}

impl EventEmitter<SearchEvent> for SearchPanel {}

impl SearchPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search in Task"));
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let subscription = cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => this.schedule(DEBOUNCE, cx),
            InputEvent::PressEnter { .. } => this.step(1, cx),
            _ => {}
        });
        Self {
            client,
            root,
            input,
            replacement,
            regex: false,
            case_sensitive: false,
            preserve_case: false,
            replaced: None,
            hits: Vec::new(),
            truncated: false,
            selected: None,
            searching: false,
            error: None,
            search: None,
            title: None,
            _subscription: subscription,
        }
    }

    /// The References panel.
    pub fn references(root: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            title: Some("Press Shift-F12 on a symbol to show its references here.".into()),
            ..Self::new(root, None, window, cx)
        }
    }

    /// References panel: `title` is being searched.
    pub fn set_loading(&mut self, title: impl Into<SharedString>, cx: &mut Context<Self>) {
        self.title = Some(title.into());
        self.hits.clear();
        self.selected = None;
        self.searching = true;
        self.error = None;
        cx.notify();
    }

    /// References panel: the results for `title`, or why there are none.
    pub fn set_results(&mut self, title: impl Into<SharedString>, result: Result<Vec<SearchHit>, SharedString>, cx: &mut Context<Self>) {
        self.title = Some(title.into());
        self.selected = None;
        self.searching = false;
        self.truncated = false;
        match result {
            Ok(hits) => {
                self.hits = hits;
                self.error = None;
            }
            Err(error) => {
                self.hits.clear();
                self.error = Some(error);
            }
        }
        cx.notify();
    }

    /// Switches to a new connection with the agent.
    pub fn set_client(&mut self, client: Arc<Client>) {
        self.client = Some(client);
    }

    /// Focuses the search box, with its text selected.
    pub fn focus(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
    }

    /// Searches for `text` (the editor selection, for example).
    pub fn set_query(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |input, cx| input.set_value(text.to_string(), window, cx));
        self.schedule(Duration::ZERO, cx);
    }

    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.replaced = None;
        let query = self.input.read(cx).value().to_string();
        if query.is_empty() {
            self.search = None;
            self.hits.clear();
            self.selected = None;
            self.searching = false;
            self.error = None;
            return cx.notify();
        }
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return cx.notify();
        };
        let request = Request::Search {
            path: self.root.clone(),
            query,
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            max_hits: MAX_HITS,
        };
        self.searching = true;
        // Replacing the previous task cancels it: its result never arrives.
        self.search = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(delay).await;
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                this.searching = false;
                match response {
                    Ok(Response::SearchResults { hits, truncated }) => {
                        this.hits = hits;
                        this.truncated = truncated;
                        this.selected = None;
                        this.error = None;
                    }
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        }));
        cx.notify();
    }

    /// Asks before replacing every match shown.
    fn confirm_replace(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let files: Vec<String> = self
            .hits
            .iter()
            .map(|hit| hit.path.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let with = self.replacement.read(cx).value().to_string();
        let message = format!(
            "Replace the matches on {}{} in {} with \"{with}\"?",
            if self.truncated { "at least " } else { "" },
            count(self.hits.len(), "line"),
            count(files.len(), "file")
        );
        let detail = "Files with unsaved changes are left out. This can't be undone from here.";
        let answer = window.prompt(PromptLevel::Warning, &message, Some(detail), &[PromptButton::new("Cancel"), PromptButton::new("Replace")], cx);
        cx.spawn(async move |this, cx| {
            if matches!(answer.await, Ok(1)) {
                this.update(cx, |_, cx| cx.emit(SearchEvent::Replace { files })).ok();
            }
        })
        .detach();
    }

    /// Replaces the matches in `files` (the workspace already left out
    /// those with unsaved changes; `skipped` says how many) and searches again.
    pub fn replace(&mut self, files: Vec<String>, skipped: usize, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        let request = Request::Replace {
            path: self.root.clone(),
            files,
            query: self.input.read(cx).value().to_string(),
            regex: self.regex,
            case_sensitive: self.case_sensitive,
            replacement: self.replacement.read(cx).value().to_string(),
            preserve_case: self.preserve_case,
        };
        cx.spawn(async move |this, cx| {
            let response = client.request(request).await;
            this.update(cx, |this, cx| {
                this.schedule(Duration::ZERO, cx);
                let skipped = match skipped {
                    0 => String::new(),
                    n => format!("; {n} with unsaved changes left out"),
                };
                match response {
                    Ok(Response::Replaced { files, replacements }) => {
                        this.replaced =
                            Some(format!("Replaced {replacements} in {}{skipped}", count(files, "file")).into())
                    }
                    Ok(other) => this.error = Some(format!("Unexpected response: {other:?}").into()),
                    Err(err) => this.error = Some(format!("{err:#}").into()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// F4 / Shift-F4: next or previous match, opened in preview.
    pub fn step(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.hits.is_empty() {
            return;
        }
        let len = self.hits.len() as isize;
        let next = match self.selected {
            Some(ix) => (ix as isize + delta).rem_euclid(len),
            None if delta >= 0 => 0,
            None => len - 1,
        } as usize;
        self.open(next, false, cx);
    }

    fn open(&mut self, ix: usize, pin: bool, cx: &mut Context<Self>) {
        self.selected = Some(ix);
        let hit = &self.hits[ix];
        cx.emit(SearchEvent::Open {
            file: hit.path.clone(),
            line: hit.line,
            column: hit.column,
            pin,
        });
        cx.notify();
    }

    fn rows(&self) -> Vec<Row> {
        let mut rows = Vec::new();
        let mut ix = 0;
        while ix < self.hits.len() {
            let path = self.hits[ix].path.clone();
            let count = self.hits[ix..].iter().take_while(|hit| hit.path == path).count();
            rows.push(Row::File { path, count });
            rows.extend((ix..ix + count).map(Row::Hit));
            ix += count;
        }
        rows
    }
}

/// "1 file", "3 files".
fn count(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

impl SearchPanel {
    fn render_search_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let toggle = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .px_1p5()
                .py_0p5()
                .text_ui_small(cx)
                .rounded(theme.radius)
                .font_family(theme.mono_font_family.clone())
                .when(on, |el| el.bg(theme.sidebar_accent).text_color(theme.sidebar_foreground))
                .when(!on, |el| el.text_color(theme.muted_foreground))
                .hover(|style| style.text_color(theme.sidebar_foreground))
                .child(label)
        };
        let can_replace = !self.hits.is_empty() && !self.searching;
        v_flex()
            .px_2()
            .pt_2()
            .gap_1()
            .child(
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&self.input)))
                    .child(toggle("search-case", "Aa", self.case_sensitive).on_click(cx.listener(|this, _, _, cx| {
                        this.case_sensitive = !this.case_sensitive;
                        this.schedule(Duration::ZERO, cx);
                    })))
                    .child(toggle("search-regex", ".*", self.regex).on_click(cx.listener(|this, _, _, cx| {
                        this.regex = !this.regex;
                        this.schedule(Duration::ZERO, cx);
                    }))),
            )
            .child(
                h_flex()
                    .gap_1()
                    .child(div().flex_1().child(Input::new(&self.replacement)))
                    .child(toggle("replace-case", "AB", self.preserve_case).on_click(cx.listener(|this, _, _, cx| {
                        this.preserve_case = !this.preserve_case;
                        cx.notify();
                    })))
                    .child(
                        toggle("replace-all", "All", false)
                            .when(!can_replace, |el| el.opacity(0.5))
                            .when(can_replace, |el| {
                                el.on_click(cx.listener(|this, _, window, cx| this.confirm_replace(window, cx)))
                            }),
                    ),
            )
    }
}

impl Render for SearchPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let header = match &self.title {
            Some(title) => div()
                .px_3()
                .pt_2()
                .text_ui(cx)
                .text_color(cx.theme().sidebar_foreground)
                .whitespace_normal()
                .child(title.clone())
                .into_any_element(),
            None => self.render_search_box(cx).into_any_element(),
        };
        let theme = cx.theme();
        let files = self.hits.iter().map(|hit| &hit.path).collect::<std::collections::BTreeSet<_>>().len();
        let summary = if let Some(replaced) = &self.replaced {
            replaced.to_string()
        } else if self.searching {
            "Searching…".to_string()
        } else if self.hits.is_empty() {
            String::new()
        } else {
            format!(
                "{}{} in {} files",
                self.hits.len(),
                if self.truncated { "+" } else { "" },
                files
            )
        };
        let rows = self.rows();
        let hits = self.hits.clone();
        let selected = self.selected;
        let view = cx.entity();
        let mono = theme.mono_font_family.clone();
        v_flex()
            .size_full()
            .text_ui(cx)
            .child(header)
            .child(
                div()
                    .px_3()
                    .py_1()
                    .text_ui_small(cx)
                    .text_color(theme.muted_foreground)
                    .child(summary),
            )
            .children(self.error.clone().map(|error| {
                div()
                    .px_3()
                    .text_ui_small(cx)
                    .text_color(theme.danger)
                    .whitespace_normal()
                    .child(error)
            }))
            .child(
                uniform_list("search-results", rows.len(), move |range, _, cx| {
                    let theme = cx.theme();
                    range
                        .map(|ix| match &rows[ix] {
                            Row::File { path, count } => h_flex()
                                .id(ix)
                                .h(px(22.))
                                .px_3()
                                .gap_2()
                                .text_ui_small(cx)
                                .child(
                                    div()
                                        .flex_1()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .text_color(theme.sidebar_foreground)
                                        .child(path.clone()),
                                )
                                .child(div().text_color(theme.muted_foreground).child(count.to_string()))
                                .context_menu({
                                    let panel = view.downgrade();
                                    let path = path.clone();
                                    move |menu, _, _| {
                                        let (open, copy) = (path.clone(), path.clone());
                                        menu.item(menu::item("Open File", &panel, move |_, _, cx| {
                                            cx.emit(SearchEvent::Open {
                                                file: open.clone(),
                                                line: 1,
                                                column: 0,
                                                pin: true,
                                            })
                                        }))
                                        .item(menu::item("Copy Relative Path", &panel, move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(copy.clone()))
                                        }))
                                    }
                                })
                                .into_any_element(),
                            Row::Hit(hit_ix) => {
                                let hit = &hits[*hit_ix];
                                let is_selected = selected == Some(*hit_ix);
                                let chars: Vec<char> = hit.text.chars().collect();
                                let start = (hit.column as usize).min(chars.len());
                                let end = (start + hit.length as usize).min(chars.len());
                                // Trim the front so the match stays visible.
                                let from = start.saturating_sub(24);
                                let before: String = chars[from..start].iter().collect();
                                let matched: String = chars[start..end].iter().collect();
                                let after: String = chars[end..].iter().collect();
                                let hit_ix = *hit_ix;
                                let view = view.clone();
                                h_flex()
                                    .id(ix)
                                    .h(px(22.))
                                    .pl(px(20.))
                                    .pr_2()
                                    .gap_2()
                                    .text_ui_small(cx)
                                    .when(is_selected, |el| el.bg(theme.sidebar_accent))
                                    .when(!is_selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                                    .child(
                                        div()
                                            .w(px(32.))
                                            .flex_none()
                                            .text_color(theme.muted_foreground)
                                            .child(hit.line.to_string()),
                                    )
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .overflow_hidden()
                                            .whitespace_nowrap()
                                            .font_family(mono.clone())
                                            .text_color(theme.sidebar_foreground)
                                            .child(format!("{}{}", if from > 0 { "…" } else { "" }, before.trim_start()))
                                            .child(div().bg(theme.warning.opacity(0.35)).child(matched))
                                            .child(after),
                                    )
                                    .on_click(move |event, _, cx| {
                                        let pin = event.click_count() >= 2;
                                        view.update(cx, |this, cx| this.open(hit_ix, pin, cx));
                                    })
                                    .into_any_element()
                            }
                        })
                        .collect()
                })
                .flex_1()
                .min_h_0(),
            )
    }
}
