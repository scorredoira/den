# vendor

## gpui-base 0.7.0

A copy of the crates.io release (Apache-2.0, see `gpui-base/LICENSE-APACHE`),
used through `[patch.crates-io]` in the root `Cargo.toml`. Its benches and
tests were left out.

Changes marked `(den)` in `src/input/base/state.rs`: public
`selections`, `set_selections`, `reveal_offset` and `edit` on the editor
state, which `crates/ui/src/editing.rs` needs for Cmd-D and moving and
duplicating lines with several cursors (the crate keeps its cursors private).

Also `(den)`, for side-by-side diffs: `LineStyle` and `set_line_styles`
(a background across the whole line, a hatched gap and a label of its own in
the gutter, painted in `src/input/base/element.rs`), `target_scroll_offset`,
and a notification when the scrollbar moves the offset, so the other side can
follow it. And `ScrollbarMark` and `set_scrollbar_marks`: the changes'
marks in the scrollbar's track, painted under the thumb by `EditorScrollbar`.

Also `(den)`, for the debugger: `set_gutter_column` reserves a column
before the line numbers (`GUTTER_COLUMN_WIDTH` in `src/input/base/element.rs`),
`set_gutter_marks` paints a breakpoint's dot or ring in it, `set_execution_line`
paints the line stopped at and an arrow, and `on_gutter_click` gets a mouse
down on the gutter instead of the cursor moving.

Markdown previews use `resolve_image_source` in `src/text/text_view.rs` and
`src/text/node.rs` to load file images through the workspace's agent, while
keeping the default handling of HTTP and embedded data URLs. The component
facade exposes this hook in `src/text/compat.rs`.

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

## gpui-component 0.7.0

The crates.io release the same way (its `tests/` and `[[test]]` entries left
out). Changes, all marked `(den)` in `src/input/popovers/completion_menu.rs`:
the completion menu drawn like VS Code's, with an icon per kind, the letters
that match what's typed highlighted, the detail at the right of the selected
item only, and that item resolved for its detail and documentation.

And one fix marked `(den)` in `src/highlighter/input_adapter.rs`: the fold
ranges were collected by recursing over the syntax tree, which overflowed a
background thread's stack on a deeply nested file and killed the app. They
are now walked with a tree cursor.

And `(den)` for syntax colors like VS Code's: `SyntaxColors` in
`src/highlighter/registry.rs` (and `wasm_stub.rs`) takes `constant.builtin`,
`keyword.control`, `namespace` and `variable.parameter`; the TypeScript,
JavaScript, Go and Rust `highlights.scm` capture control keywords as
`keyword.control` and parameters as `variable.parameter` (Rust's numbers and
booleans as `number` and `boolean`); and `src/highlighter/highlighter.rs`
skips the `local.*` captures, which shadowed the highlights of the same node. Brackets are colored by nesting depth like VS Code's bracket pair colors
(`bracket_depth` there, the `punctuation.bracket.1`–`3` colors, and Go's
`highlights.scm` capturing its brackets).

Updating: copy the new release over it, drop `benches/`, `tests/` and their
`[[bench]]`/`[[test]]` entries, and reapply the `(den)` functions.
