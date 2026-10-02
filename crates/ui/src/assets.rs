use std::borrow::Cow;

use gpui_kit::{AssetSource, Result, SharedString};

/// The app's own icons; everything else is served by gpui-kit's assets.
const ICONS: &[(&str, &[u8])] = &[
    ("icons/files.svg", include_bytes!("../assets/icons/files.svg")),
    ("icons/git-branch.svg", include_bytes!("../assets/icons/git-branch.svg")),
    ("icons/text-search.svg", include_bytes!("../assets/icons/text-search.svg")),
    ("icons/references.svg", include_bytes!("../assets/icons/search.svg")),
    ("icons/tree-chevron-right.svg", include_bytes!("../assets/icons/chevron-right.svg")),
    ("icons/tree-chevron-down.svg", include_bytes!("../assets/icons/chevron-down.svg")),
    ("icons/tree-file.svg", include_bytes!("../assets/icons/file.svg")),
    ("icons/tree-folder.svg", include_bytes!("../assets/icons/folder.svg")),
    ("icons/tree-folder-open.svg", include_bytes!("../assets/icons/folder-open.svg")),
    ("icons/tab-close.svg", include_bytes!("../assets/icons/x.svg")),
    ("icons/tab-dirty.svg", include_bytes!("../assets/icons/circle.svg")),
    ("icons/settings.svg", include_bytes!("../assets/icons/settings.svg")),
    ("icons/server.svg", include_bytes!("../assets/icons/server.svg")),
    ("icons/monitor.svg", include_bytes!("../assets/icons/monitor.svg")),
    ("icons/folder.svg", include_bytes!("../assets/icons/folder.svg")),
    ("icons/panel-left.svg", include_bytes!("../assets/icons/panel-left.svg")),
    ("icons/panel-right.svg", include_bytes!("../assets/icons/panel-right.svg")),
    ("icons/plus.svg", include_bytes!("../assets/icons/plus.svg")),
    ("icons/sik-empty.svg", include_bytes!("../assets/icons/sik-empty.svg")),
    ("icons/play.svg", include_bytes!("../assets/icons/play.svg")),
    ("icons/pause.svg", include_bytes!("../assets/icons/pause.svg")),
    ("icons/square.svg", include_bytes!("../assets/icons/square.svg")),
    ("icons/rotate-ccw.svg", include_bytes!("../assets/icons/rotate-ccw.svg")),
    ("icons/redo-dot.svg", include_bytes!("../assets/icons/redo-dot.svg")),
    ("icons/arrow-down-to-dot.svg", include_bytes!("../assets/icons/arrow-down-to-dot.svg")),
    ("icons/arrow-up-from-dot.svg", include_bytes!("../assets/icons/arrow-up-from-dot.svg")),
    ("icons/bug.svg", include_bytes!("../assets/icons/bug.svg")),
    ("icons/terminal.svg", include_bytes!("../assets/icons/square-terminal.svg")),
    ("icons/layers.svg", include_bytes!("../assets/icons/layers.svg")),
    ("icons/history.svg", include_bytes!("../assets/icons/history.svg")),
    ("icons/code.svg", include_bytes!("../assets/icons/code.svg")),
    ("icons/symbol-function.svg", include_bytes!("../assets/icons/box.svg")),
    ("icons/symbol-field.svg", include_bytes!("../assets/icons/tag.svg")),
    ("icons/symbol-variable.svg", include_bytes!("../assets/icons/variable.svg")),
    ("icons/symbol-constant.svg", include_bytes!("../assets/icons/pi.svg")),
    ("icons/symbol-class.svg", include_bytes!("../assets/icons/shapes.svg")),
    ("icons/symbol-type.svg", include_bytes!("../assets/icons/type.svg")),
    ("icons/symbol-enum.svg", include_bytes!("../assets/icons/list.svg")),
    ("icons/symbol-module.svg", include_bytes!("../assets/icons/braces.svg")),
    ("icons/symbol-file.svg", include_bytes!("../assets/icons/file.svg")),
    ("icons/symbol-heading.svg", include_bytes!("../assets/icons/hash.svg")),
    ("icons/symbol-event.svg", include_bytes!("../assets/icons/zap.svg")),
];

pub struct Assets;

impl AssetSource for Assets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some((_, bytes)) = ICONS.iter().find(|(name, _)| *name == path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        let mut names: Vec<SharedString> = ICONS
            .iter()
            .filter(|(name, _)| name.starts_with(path))
            .map(|(name, _)| (*name).into())
            .collect();
        names.extend(gpui_kit::assets::Assets.list(path)?);
        Ok(names)
    }
}
