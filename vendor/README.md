# vendor

## gpui-base 0.7.0

A copy of the crates.io release (Apache-2.0, see `gpui-base/LICENSE-APACHE`),
used through `[patch.crates-io]` in the root `Cargo.toml`. Its benches and
tests were left out.

Changes marked `(sik)` in `src/input/base/state.rs`: public
`selections`, `set_selections`, `reveal_offset` and `edit` on the editor
state, which `crates/ui/src/editing.rs` needs for Cmd-D and moving and
duplicating lines with several cursors (the crate keeps its cursors private).

Also `(sik)`, for side-by-side diffs: `LineStyle` and `set_line_styles`
(a background across the whole line, a hatched gap and a label of its own in
the gutter, painted in `src/input/base/element.rs`), `target_scroll_offset`,
and a notification when the scrollbar moves the offset, so the other side can
follow it.

And one fix marked `(sik)` in `src/input/editor/lsp/completions.rs`: the
completion menu forgot where it had opened only on hiding its own copy, so
after the first completion typing before that point asked for nothing; and it
stayed open, stale, on text that isn't a trigger. And `resolve_completion`
on `CompletionProvider`, which the menu calls for the selected item.

## gpui-component 0.7.0

The crates.io release the same way (its `tests/` and `[[test]]` entries left
out). Changes, all marked `(sik)` in `src/input/popovers/completion_menu.rs`:
the completion menu drawn like VS Code's, with an icon per kind, the letters
that match what's typed highlighted, the detail at the right of the selected
item only, and that item resolved for its detail and documentation.

Updating: copy the new release over it, drop `benches/`, `tests/` and their
`[[bench]]`/`[[test]]` entries, and reapply the `(sik)` functions.
