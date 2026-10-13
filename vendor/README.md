# vendor

Every change from the crates.io releases is marked `(den)` in the source
(for `search.rs` in both crates, which were reworked, once at the top of the
file) and described below. `patches/` holds all of them as unified diffs
against the releases, so an update can reapply them.

## gpui-base 0.7.0

A copy of the crates.io release (Apache-2.0, see `gpui-base/LICENSE-APACHE`),
used through `[patch.crates-io]` in the root `Cargo.toml`. Its benches and
tests were left out, with their `[[bench]]`/`[[test]]` entries in
`Cargo.toml`, and the `aho-corasick` dependency, which the search no longer
uses (below).

Changes marked `(den)` in `src/input/base/state.rs`: public
`selections`, `set_selections`, `reveal_offset` and `edit` on the editor
state, which `crates/ui/src/editing.rs` needs for Cmd-D and moving and
duplicating lines with several cursors (the crate keeps its cursors private).

Also `(den)`, for side-by-side diffs: `LineStyle` and `set_line_styles`
(a background across the whole line, a hatched gap and a label of its own in
the gutter, painted in `src/input/base/element.rs`), `target_scroll_offset`,
and a notification when the scrollbar moves the offset, so the other side can
follow it. And `ScrollbarMark` and `set_scrollbar_marks`: the changes'
marks in the scrollbar's track, painted under the thumb by `EditorScrollbar`
(which keeps the line height in its snapshot for them).

Also `(den)`, for the debugger: `set_gutter_column` reserves a column
before the line numbers (`GUTTER_COLUMN_WIDTH` in `src/input/base/element.rs`),
`set_gutter_marks` paints a breakpoint's dot or ring in it, `set_execution_line`
paints the line stopped at and an arrow, and `on_gutter_click` gets a mouse
down on the gutter instead of the cursor moving.

And a hover fix marked `(den)` in `on_mouse_move` (`src/input/base/state.rs`):
it tested the pointer against `last_bounds`, which move up with the scroll, so
in a scrolled editor every move was taken for leaving it and hover (and its
language server popover) only worked at the top of a file. It tests the
visible `input_bounds`; `test_hover_in_a_scrolled_editor` covers it.

Markdown previews use `resolve_image_source` in `src/text/text_view.rs` and
`src/text/node.rs` to load file images through the workspace's agent, while
keeping the default handling of HTTP and embedded data URLs (the resolver
returns an `Option`; `None` keeps the default). The component facade exposes
this hook in `src/text/compat.rs`.

And `(den)` for Cmd-click: in `src/text/inline.rs`, a Cmd-click on plain text
in a text view (Markdown) hands the word under the pointer to the link handler
as a `word:` link (`word_at`), for the app to open a `path:line`. In
`src/input/editor/lsp/definitions.rs`, `DefinitionProvider` gets
`hover_definitions`, what's underlined while Cmd is held (by default the
definitions; the app answers there only what it knows at once),
and a Cmd-click where nothing is underlined asks for the definitions, as Go to
Definition does.

And one fix marked `(den)` in `src/input/editor/lsp/completions.rs`: the
completion menu forgot where it had opened only on hiding its own copy, so
after the first completion typing before that point asked for nothing; and it
stayed open, stale, on text that isn't a trigger. And `resolve_completion`
on `CompletionProvider`, which the menu calls for the selected item.

And one fix marked `(den)` in `src/resizable/panel.rs` and `mod.rs`: a
hidden panel kept its last size (or the 100 px placeholder) in the sizes a
drag redistributes, so dragging beside it went over the container and the
dragged panel jumped back to its minimum on every move. Hidden, it now
counts as 0.

Also `(den)`, for a find like VS Code's: `SearchOptions` in
`src/input/editor/search.rs` (whole words, regular expressions with `$1` in
the replacement, and replacements that keep the case), matched with `regex`
instead of `aho-corasick`. `set_search_query` takes `SearchOptions` instead of
a case flag (the tests in `state.rs` follow), and `src/input/mod.rs` exports
it.

## gpui-component 0.7.0

The crates.io release the same way (its `tests/` and `[[test]]` entries left
out). Changes, all marked `(den)` in `src/input/popovers/completion_menu.rs`:
the completion menu drawn like VS Code's, with an icon per kind, the letters
that match what's typed highlighted (the word before the cursor, read from the
editor the menu now holds weakly), the detail at the right of the selected
item only, and that item resolved for its detail and documentation.

And one fix marked `(den)` in `src/highlighter/input_adapter.rs`: the fold
ranges were collected by recursing over the syntax tree, which overflowed a
background thread's stack on a deeply nested file and killed the app. They
are now walked with a tree cursor.

And `(den)` for syntax colors like VS Code's: `SyntaxColors` in
`src/highlighter/registry.rs` (and `wasm_stub.rs`) takes `constant.builtin`,
`keyword.control`, `namespace` and `variable.parameter`, and for XML and JSON
`attribute.value`, `tag.delimiter`, `string.key` and `string.value` (these
four falling back to the color they had before); the TypeScript, JavaScript, Go and
Rust `highlights.scm` capture control keywords as `keyword.control` and
parameters as `variable.parameter` (TypeScript's namespaces as `namespace`,
Rust's numbers and booleans as `number` and `boolean`); JSON's captures keys
and string values as `string.key` and `string.value`; and
`src/highlighter/highlighter.rs` skips the `local.*` captures, which shadowed
the highlights of the same node. Brackets are colored by nesting depth like
VS Code's bracket pair colors (`bracket_depth` there, the
`punctuation.bracket.1`–`3` colors, and Go's `highlights.scm` capturing its
brackets).

And `(den)` for the find bar (Cmd-F) as VS Code's: `src/input/search.rs`
floats at the editor's top right (placed in `render_editor` in
`src/input/input.rs`), its left edge drags its width, and ↑↓ go through the
queries searched; `FindBarMemory` (exported from `src/input/mod.rs`, with
`SearchOptions`) holds the width, the history and the toggles for the app to
save. Its icons not in the default set (`whole-word`, `regex`, `replace-all`,
`case-upper`) come from the app's assets.

And `(den)` so a tooltip never shows over an open menu: an open context menu
holds a `DeferredPopover` in `src/menu/context_menu.rs`, as an open popover
does, and `Tooltip` in `src/tooltip.rs` renders nothing while one is held.
A right click also opens only the innermost context menu, not those of the
elements around it.

And `(den)` for shortcuts in context menus: `NativeMenuItem::Item` has a `key`,
which `NativeMenu::show` fills in (`resolve_keys` in `src/native_menu/mod.rs`)
with the single-keystroke binding of the item's action where the focus is.
macOS shows it as the item's key equivalent (`set_key_equivalent` in
`macos.rs`), Windows after a tab in the label (`windows.rs`); `fallback.rs`
ignores it.

## Updating

Copy the new release over each crate; drop `benches/`, `tests/` and their
`[[bench]]`/`[[test]]` entries (and `aho-corasick` from gpui-base); then
reapply the changes from `patches/`, from the crate's folder:

```sh
patch -p1 -d vendor/gpui-base < vendor/patches/gpui-base.patch
patch -p1 -d vendor/gpui-component < vendor/patches/gpui-component.patch
```

Hunks that no longer apply are redone by hand, with the descriptions above.
After changing a vendored crate, regenerate its patch against the release
(downloaded from `https://static.crates.io/crates/<name>/<name>-<version>.crate`,
a gzipped tarball):

```sh
diff -ruN -x benches -x tests -x Cargo.lock -x Cargo.toml.orig -x .cargo_vcs_info.json -x .cargo-ok \
  gpui-base-0.7.0 vendor/gpui-base | sed -e 's|^--- gpui-base-0.7.0/|--- a/|' -e 's|^+++ vendor/gpui-base/|+++ b/|' \
  > vendor/patches/gpui-base.patch
```
