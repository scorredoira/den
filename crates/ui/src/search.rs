//! Search panel: searches the task's files (on the agent) as you type, and
//! groups the results by file, and replaces them. The References panel is the
//! same, without the boxes: its results (F12, Shift-F12) come from outside.

use std::{collections::BTreeSet, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use client::Client;
use gpui_kit::component::{
    ActiveTheme as _, h_flex,
    input::{Input, InputEvent, InputState},
    menu::ContextMenuExt as _,
    tooltip::Tooltip,
    v_flex,
};
use gpui_kit::{prelude::FluentBuilder as _, *};
use crate::menu::PanelItems as _;
use proto::{Request, Response, SearchHit, SearchQuery};

use crate::{
    config::{Config, UiText},
    menu,
};

/// Wait after the last keystroke before searching.
const DEBOUNCE: Duration = Duration::from_millis(200);
/// Cap on matches per search.
const MAX_HITS: usize = 5_000;

pub enum SearchEvent {
    Open { file: String, line: u32, column: u32, pin: bool },
    /// Replace All was confirmed for these files: the workspace leaves out
    /// those with unsaved changes and calls `replace`.
    Replace { files: Vec<String> },
    /// Select `file` in the Files panel.
    Reveal { file: String },
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
    /// The globs of the files searched, and of those left out.
    include: Entity<InputState>,
    exclude: Entity<InputState>,
    regex: bool,
    case_sensitive: bool,
    whole_word: bool,
    /// Each replacement takes the case of what it replaces.
    preserve_case: bool,
    /// The replace box shows (the chevron on the left).
    show_replace: bool,
    /// The files to include and exclude show (the ⋯ under the search box).
    show_files: bool,
    /// What the last Replace All did.
    replaced: Option<SharedString>,
    /// Changed only through `set_hits`, which works out the rows from them.
    hits: Rc<Vec<SearchHit>>,
    /// The hits under their files, and how many files.
    rows: Rc<Vec<Row>>,
    files: usize,
    truncated: bool,
    /// Find in Folder: the folder searched (relative to `root`), not all of it.
    scope: Option<String>,
    /// Files with some lines dismissed: Replace All still replaces those.
    dismissed: BTreeSet<String>,
    selected: Option<usize>,
    searching: bool,
    error: Option<SharedString>,
    search: Option<Task<()>>,
    /// References panel: what is shown instead of the search box.
    title: Option<SharedString>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<SearchEvent> for SearchPanel {}

impl SearchPanel {
    pub fn new(root: PathBuf, client: Option<Arc<Client>>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx).placeholder("Search (↑↓ for history)"));
        let replacement = cx.new(|cx| InputState::new(window, cx).placeholder("Replace"));
        let include = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. *.ts, src/**/include"));
        let exclude = cx.new(|cx| InputState::new(window, cx).placeholder("e.g. *.ts, src/**/exclude"));
        let globs_changed = |this: &mut Self, _: Entity<InputState>, event: &InputEvent, cx: &mut Context<Self>| {
            if let InputEvent::Change = event {
                this.schedule(DEBOUNCE, cx)
            }
        };
        let subscriptions = vec![
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Change => this.schedule(DEBOUNCE, cx),
                InputEvent::PressEnter { .. } => this.step(1, cx),
                _ => {}
            }),
            cx.subscribe(&include, globs_changed),
            cx.subscribe(&exclude, globs_changed),
        ];
        let saved = &Config::get(cx).search;
        Self {
            client,
            root,
            input,
            replacement,
            include,
            exclude,
            regex: saved.regex,
            case_sensitive: saved.case_sensitive,
            whole_word: saved.whole_word,
            preserve_case: saved.preserve_case,
            show_replace: false,
            show_files: false,
            replaced: None,
            hits: Rc::default(),
            rows: Rc::default(),
            files: 0,
            truncated: false,
            scope: None,
            dismissed: BTreeSet::new(),
            selected: None,
            searching: false,
            error: None,
            search: None,
            title: None,
            _subscriptions: subscriptions,
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
        // The last results stay until the new ones come: no empty flash.
        self.title = Some(title.into());
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
        self.dismissed.clear();
        match result {
            Ok(hits) => {
                self.set_hits(hits);
                self.error = None;
            }
            Err(error) => {
                self.set_hits(Vec::new());
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

    /// Searches only under `scope` (relative to the task's folder), or all of
    /// the task with `None`.
    pub fn set_scope(&mut self, scope: Option<String>, cx: &mut Context<Self>) {
        if self.scope != scope {
            self.scope = scope;
            self.schedule(Duration::ZERO, cx);
        }
    }

    fn schedule(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.replaced = None;
        let query = self.input.read(cx).value().to_string();
        if query.is_empty() {
            self.search = None;
            self.set_hits(Vec::new());
            self.selected = None;
            self.searching = false;
            self.error = None;
            return cx.notify();
        }
        let Some(client) = self.client.clone() else {
            self.error = Some("No agent".into());
            return cx.notify();
        };
        // The agent searches the folder, and the paths it finds are relative to it.
        let scope = self.scope.clone();
        let request = Request::Search {
            path: scope.as_ref().map_or_else(|| self.root.clone(), |scope| self.root.join(scope)),
            query: self.query(query),
            include: globs(&self.include.read(cx).value()),
            exclude: globs(&self.exclude.read(cx).value()),
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
                    Ok(Response::SearchResults { mut hits, truncated }) => {
                        if let Some(scope) = &scope {
                            for hit in &mut hits {
                                hit.path = format!("{scope}/{}", hit.path);
                            }
                        }
                        this.set_hits(hits);
                        this.dismissed.clear();
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

    fn query(&self, text: String) -> SearchQuery {
        SearchQuery { text, regex: self.regex, case_sensitive: self.case_sensitive, whole_word: self.whole_word }
    }

    /// A toggle flipped: kept for the next time, and searched again.
    fn toggle(&mut self, change: impl FnOnce(&mut Self), cx: &mut Context<Self>) {
        change(self);
        let (regex, case_sensitive, whole_word, preserve_case) =
            (self.regex, self.case_sensitive, self.whole_word, self.preserve_case);
        Config::update(cx, |config| {
            let saved = &mut config.search;
            (saved.regex, saved.case_sensitive, saved.whole_word, saved.preserve_case) =
                (regex, case_sensitive, whole_word, preserve_case);
        });
        self.schedule(Duration::ZERO, cx);
    }

    /// The query searched goes last in the history.
    fn remember(&self, cx: &mut App) {
        let query = self.input.read(cx).value().to_string();
        Config::update_quietly(cx, |config| config.search.remember(&query));
    }

    /// ↑ and ↓ in the search box: the query before or after in the
    /// history. What was typed and not searched yet is kept in it first.
    fn browse_history(&mut self, back: bool, window: &mut Window, cx: &mut Context<Self>) {
        let current = self.input.read(cx).value().to_string();
        if !Config::get(cx).search.history.contains(&current) {
            self.remember(cx);
        }
        let history = &Config::get(cx).search.history;
        let at = history.iter().rposition(|query| *query == current).unwrap_or(history.len());
        let to = if back { at.checked_sub(1) } else { (at + 1 < history.len()).then_some(at + 1) };
        let Some(query) = to.and_then(|ix| history.get(ix)).cloned() else {
            return;
        };
        self.input.update(cx, |input, cx| {
            input.set_value(query, window, cx);
            input.select_all(window, cx);
        });
        self.schedule(Duration::ZERO, cx);
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
        let dismissed = files.iter().filter(|file| self.dismissed.contains(*file)).count();
        let detail = match dismissed {
            0 => "Files with unsaved changes are left out. This can't be undone from here.".to_string(),
            n => format!(
                "Files with unsaved changes are left out. The lines dismissed in {} still listed are replaced too. \
                 This can't be undone from here.",
                count(n, "file")
            ),
        };
        let answer = window.prompt(PromptLevel::Warning, &message, Some(&detail), &[PromptButton::new("Cancel"), PromptButton::new("Replace")], cx);
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
        self.remember(cx);
        let request = Request::Replace {
            path: self.root.clone(),
            files,
            query: self.query(self.input.read(cx).value().to_string()),
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
        if self.title.is_none() {
            self.remember(cx);
        }
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

    /// Dismiss on a match: takes it off the list.
    fn dismiss_hit(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(hit) = self.hits.get(ix) else {
            return;
        };
        let path = hit.path.clone();
        let mut hits = Rc::unwrap_or_clone(std::mem::take(&mut self.hits));
        self.selected = dismiss(&mut hits, self.selected, |at, _| at == ix);
        self.set_hits(hits);
        // The file's other lines are still listed (and replaced).
        if self.hits.iter().any(|hit| hit.path == path) {
            self.dismissed.insert(path);
        }
        cx.notify();
    }

    /// Dismiss on a file: takes its matches off the list.
    fn dismiss_file(&mut self, path: &str, cx: &mut Context<Self>) {
        let mut hits = Rc::unwrap_or_clone(std::mem::take(&mut self.hits));
        self.selected = dismiss(&mut hits, self.selected, |_, hit| hit.path == path);
        self.set_hits(hits);
        self.dismissed.remove(path);
        cx.notify();
    }

    /// New hits: the rows and the count of files are worked out once here,
    /// not each time it's drawn.
    fn set_hits(&mut self, hits: Vec<SearchHit>) {
        self.files = hits.iter().map(|hit| &hit.path).collect::<BTreeSet<_>>().len();
        self.hits = Rc::new(hits);
        self.rows = Rc::new(self.rows());
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

/// Removes the hits `remove` says (by index and hit) and returns where the
/// selected one is now, if it's still there.
fn dismiss(hits: &mut Vec<SearchHit>, selected: Option<usize>, remove: impl Fn(usize, &SearchHit) -> bool) -> Option<usize> {
    let mut kept = Vec::with_capacity(hits.len());
    let mut now = None;
    for (ix, hit) in std::mem::take(hits).into_iter().enumerate() {
        if remove(ix, &hit) {
            continue;
        }
        if selected == Some(ix) {
            now = Some(kept.len());
        }
        kept.push(hit);
    }
    *hits = kept;
    now
}

/// The globs written in a files box: separated by commas.
fn globs(text: &str) -> Vec<String> {
    text.split(',').map(str::trim).filter(|glob| !glob.is_empty()).map(String::from).collect()
}

/// "1 file", "3 files".
fn count(n: usize, what: &str) -> String {
    if n == 1 { format!("1 {what}") } else { format!("{n} {what}s") }
}

impl SearchPanel {
    fn render_search_box(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let can_replace = !self.hits.is_empty() && !self.searching;
        let label = |text: &'static str| div().text_ui_small(cx).text_color(theme.sidebar_foreground).child(text);
        h_flex()
            .items_start()
            .pl_1()
            .pr_2()
            .pt_2()
            .gap_0p5()
            .child(
                icon_button(
                    "search-replace-toggle",
                    if self.show_replace { "icons/tree-chevron-down.svg" } else { "icons/tree-chevron-right.svg" },
                    "Toggle Replace",
                    false,
                    cx,
                )
                .mt(px(5.))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.show_replace = !this.show_replace;
                    if this.show_replace {
                        this.replacement.update(cx, |input, cx| input.focus(window, cx));
                    }
                    cx.notify();
                })),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                                let keystroke = &event.keystroke;
                                if keystroke.modifiers.modified() {
                                    return;
                                }
                                let back = match keystroke.key.as_str() {
                                    "up" => true,
                                    "down" => false,
                                    _ => return,
                                };
                                cx.stop_propagation();
                                this.browse_history(back, window, cx);
                            }))
                            .child(
                                Input::new(&self.input).suffix(
                                    h_flex()
                                        .gap_0p5()
                                        .child(
                                            icon_button("search-case", "icons/case-sensitive.svg", "Match Case", self.case_sensitive, cx)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle(|this| this.case_sensitive = !this.case_sensitive, cx)
                                                })),
                                        )
                                        .child(
                                            icon_button("search-word", "icons/whole-word.svg", "Match Whole Word", self.whole_word, cx)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle(|this| this.whole_word = !this.whole_word, cx)
                                                })),
                                        )
                                        .child(
                                            icon_button("search-regex", "icons/regex.svg", "Use Regular Expression", self.regex, cx)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle(|this| this.regex = !this.regex, cx)
                                                })),
                                        ),
                                ),
                            ),
                    )
                    .when(self.show_replace, |el| {
                        el.child(
                            h_flex()
                                .gap_2()
                                .child(
                                    div().flex_1().min_w_0().child(
                                        Input::new(&self.replacement).suffix(
                                            icon_button("replace-case", "icons/case-upper.svg", "Preserve Case", self.preserve_case, cx)
                                                .on_click(cx.listener(|this, _, _, cx| {
                                                    this.toggle(|this| this.preserve_case = !this.preserve_case, cx)
                                                })),
                                        ),
                                    ),
                                )
                                .child(
                                    icon_button("replace-all", "icons/replace-all.svg", "Replace All", false, cx)
                                        .when(!can_replace, |el| el.opacity(0.5))
                                        .when(can_replace, |el| {
                                            el.on_click(cx.listener(|this, _, window, cx| this.confirm_replace(window, cx)))
                                        }),
                                ),
                        )
                    })
                    .child(
                        h_flex().justify_end().child(
                            icon_button("search-files-toggle", "icons/ellipsis.svg", "Toggle Search Details", self.show_files, cx)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.show_files = !this.show_files;
                                    cx.notify();
                                })),
                        ),
                    )
                    .when(self.show_files, |el| {
                        el.child(label("files to include"))
                            .child(Input::new(&self.include))
                            .child(label("files to exclude"))
                            .child(Input::new(&self.exclude))
                    })
                    // Find in Folder: where it searches, with × to search everywhere again.
                    .children(self.scope.clone().map(|scope| {
                        h_flex()
                            .gap_1()
                            .text_ui_small(cx)
                            .text_color(theme.muted_foreground)
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .whitespace_nowrap()
                                    .text_ellipsis()
                                    .child(format!("In {scope}")),
                            )
                            .child(
                                div()
                                    .id("search-scope-clear")
                                    .px_1()
                                    .rounded(theme.radius)
                                    .hover(|style| style.text_color(theme.sidebar_foreground))
                                    .child("×")
                                    .on_click(cx.listener(|this, _, _, cx| this.set_scope(None, cx))),
                            )
                    })),
            )
    }
}

/// A small icon that toggles, or acts: highlighted while `on`.
fn icon_button(id: &'static str, icon: &'static str, tip: &'static str, on: bool, cx: &App) -> Stateful<Div> {
    let theme = cx.theme();
    div()
        .id(id)
        .flex_none()
        .p(px(3.))
        .rounded(theme.radius)
        .when(on, |el| el.bg(theme.sidebar_accent))
        .hover(|style| style.bg(theme.sidebar_accent.opacity(0.5)))
        .child(svg().path(icon).size(px(14.)).text_color(if on { theme.sidebar_foreground } else { theme.muted_foreground }))
        .tooltip(move |window, cx| Tooltip::new(tip).build(window, cx))
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
        let files = self.files;
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
        let rows = self.rows.clone();
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
                                // The name, and its folder in grey, as VS Code.
                                .child({
                                    let (folder, name) = path.rsplit_once(['/', '\\']).unwrap_or(("", path));
                                    h_flex()
                                        .flex_1()
                                        .min_w_0()
                                        .gap_1p5()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .child(div().flex_none().text_color(theme.sidebar_foreground).child(name.to_string()))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .text_color(theme.muted_foreground)
                                                .child(folder.to_string()),
                                        )
                                })
                                .child(
                                    div()
                                        .flex_none()
                                        .min_w(px(18.))
                                        .px_1p5()
                                        .rounded_full()
                                        .bg(theme.primary)
                                        .text_color(theme.primary_foreground)
                                        .text_center()
                                        .child(count.to_string()),
                                )
                                .context_menu({
                                    let panel = view.downgrade();
                                    let path = path.clone();
                                    move |menu, window, cx| {
                                        let (open, copy, relative, reveal, dismiss) =
                                            (path.clone(), path.clone(), path.clone(), path.clone(), path.clone());
                                        menu.item(menu::item("Open File", &panel, move |_, _, cx| {
                                            cx.emit(SearchEvent::Open {
                                                file: open.clone(),
                                                line: 1,
                                                column: 0,
                                                pin: true,
                                            })
                                        }))
                                        .item(menu::item("Dismiss", &panel, move |this, _, cx| this.dismiss_file(&dismiss, cx)))
                                        .separator()
                                        // References outside the task are absolute already.
                                        .item(menu::item("Copy Path", &panel, move |this, _, cx| {
                                            let absolute = this.root.join(&copy).to_string_lossy().into_owned();
                                            cx.write_to_clipboard(ClipboardItem::new_string(absolute))
                                        }))
                                        .item(menu::item("Copy Relative Path", &panel, move |_, _, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(relative.clone()))
                                        }))
                                        .item(menu::item("Reveal in File Tree", &panel, move |_, _, cx| {
                                            cx.emit(SearchEvent::Reveal { file: reveal.clone() })
                                        }))
                                        .separator()
                                        .panel_items(menu::hide_panel(), window, cx)
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
                                let line = hit.line;
                                // The line's number only in its tooltip: the
                                // width goes to the text, as in VS Code.
                                h_flex()
                                    .id(ix)
                                    .h(px(22.))
                                    .pl(px(24.))
                                    .pr_2()
                                    .text_ui_small(cx)
                                    .when(is_selected, |el| el.bg(crate::app::selected_row(cx)))
                                    .when(!is_selected, |el| el.hover(|style| style.bg(theme.sidebar_accent.opacity(0.5))))
                                    .tooltip(move |window, cx| Tooltip::new(format!("Line {line}")).build(window, cx))
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
                                    .on_click({
                                        let view = view.clone();
                                        move |event, _, cx| {
                                            let pin = event.click_count() >= 2;
                                            view.update(cx, |this, cx| this.open(hit_ix, pin, cx));
                                        }
                                    })
                                    .context_menu({
                                        let panel = view.downgrade();
                                        let line = hit.text.clone();
                                        move |menu, window, cx| {
                                            let line = line.clone();
                                            menu.item(menu::item("Open", &panel, move |this, _, cx| this.open(hit_ix, true, cx)))
                                                .item(menu::item("Dismiss", &panel, move |this, _, cx| this.dismiss_hit(hit_ix, cx)))
                                                .item(menu::item("Copy Line", &panel, move |_, _, cx| {
                                                    cx.write_to_clipboard(ClipboardItem::new_string(line.trim().to_string()))
                                                }))
                                                .separator()
                                                .panel_items(menu::hide_panel(), window, cx)
                                        }
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

#[cfg(test)]
mod tests {
    use super::{SearchHit, dismiss};

    fn hit(path: &str, line: u32) -> SearchHit {
        SearchHit { path: path.into(), line, column: 0, length: 1, text: String::new() }
    }

    #[test]
    fn dismissing_keeps_the_selection_on_the_same_match() {
        let mut hits = vec![hit("a", 1), hit("a", 2), hit("b", 1), hit("c", 1)];
        assert_eq!(dismiss(&mut hits, Some(2), |ix, _| ix == 0), Some(1));
        assert_eq!(hits.len(), 3);
        assert_eq!(dismiss(&mut hits, Some(1), |_, hit| hit.path == "b"), None);
        assert_eq!(hits.iter().map(|hit| (hit.path.as_str(), hit.line)).collect::<Vec<_>>(), [("a", 2), ("c", 1)]);
        assert_eq!(dismiss(&mut hits, None, |_, hit| hit.path == "a"), None);
        assert_eq!(hits.len(), 1);
    }
}
