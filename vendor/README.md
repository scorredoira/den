# vendor

## gpui-base 0.7.0

A copy of the crates.io release (Apache-2.0, see `gpui-base/LICENSE-APACHE`),
used through `[patch.crates-io]` in the root `Cargo.toml`. Its benches and
tests were left out.

Changes, all marked `(sik)` in `src/input/base/state.rs`: public
`selections`, `set_selections`, `reveal_offset` and `edit` on the editor
state, which `crates/ui/src/editing.rs` needs for Cmd-D and moving and
duplicating lines with several cursors (the crate keeps its cursors private).

Updating: copy the new release over it, drop `benches/`, `tests/` and their
`[[bench]]`/`[[test]]` entries, and reapply the `(sik)` functions.
