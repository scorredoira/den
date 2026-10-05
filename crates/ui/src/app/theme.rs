use gpui_kit::{component::{ActiveTheme as _, ThemeConfig}, rgb, App, Hsla};

/// The band behind the selected row of every panel, opaque. gpui-kit caps
/// `list_active` at 20% opacity, which leaves it unreadable on light surfaces.
const SELECTED_LIGHT: u32 = 0xc4dbfb;
const SELECTED_DARK: u32 = 0x04395e;

/// The background of the selected row in any panel.
pub(crate) fn selected_row(cx: &App) -> Hsla {
    rgb(if cx.theme().mode.is_dark() { SELECTED_DARK } else { SELECTED_LIGHT }).into()
}

/// VS Code's selection: a blue band, outlined in a stronger blue where the
/// list has the keyboard.
pub(super) fn selection(config: &mut ThemeConfig, dark: bool) {
    let colors = &mut config.colors;
    let (band, outline) = if dark { (SELECTED_DARK, "#0078d4") } else { (SELECTED_LIGHT, "#005fb8") };
    colors.list_active = Some(format!("#{band:06x}").into());
    colors.list_active_border = Some(outline.into());
}

/// Store the palette in the dark configuration so manual and system theme
/// changes also project these colors onto the editor and base components.
pub(super) fn dark_surfaces(config: &mut ThemeConfig) {
    let colors = &mut config.colors;
    // Content, surrounding chrome, and floating surfaces form three layers.
    colors.background = Some("#1e1e1e".into());
    colors.sidebar = Some("#252525".into());
    colors.tab_bar = Some("#292929".into());
    colors.tab_bar_segmented = Some("#292929".into());
    colors.tab_active = Some("#1e1e1e".into());
    colors.title_bar = Some("#292929".into());
    colors.status_bar = Some("#292929".into());
    colors.popover = Some("#2d2d2d".into());
    colors.accordion = Some("#252525".into());
    colors.group_box = Some("#252525".into());
    colors.list = Some("#2d2d2d".into());
    colors.list_head = Some("#333333".into());
    colors.list_even = Some("#303030".into());
    colors.list_hover = Some("#3b3b3b".into());
    colors.table = Some("#1e1e1e".into());
    colors.table_head = Some("#292929".into());
    colors.table_head_foreground = Some("#b8b8b8".into());
    colors.table_even = Some("#252525".into());
    colors.table_hover = Some("#333333".into());
    // Separators remain visible on each surface; inputs get a stronger edge.
    colors.border = Some("#4b4b4b".into());
    colors.sidebar_border = Some("#4b4b4b".into());
    colors.title_bar_border = Some("#4b4b4b".into());
    colors.status_bar_border = Some("#4b4b4b".into());
    colors.window_border = Some("#4b4b4b".into());
    colors.table_row_border = Some("#414141".into());
    colors.input = Some("#686868".into());
    colors.muted = Some("#333333".into());
    colors.muted_foreground = Some("#b8b8b8".into());
    colors.accent = Some("#414141".into());
    colors.sidebar_accent = Some("#414141".into());
    colors.secondary = Some("#383838".into());
    colors.secondary_hover = Some("#454545".into());
    colors.secondary_active = Some("#505050".into());
    colors.scrollbar_thumb = Some("#737373cc".into());
    colors.scrollbar_thumb_hover = Some("#929292".into());

    let highlight = config.highlight.get_or_insert_default();
    highlight.editor_background = Some(rgb(0x1e1e1e).into());
    highlight.editor_active_line = Some(rgb(0x2a2a2a).into());
}
