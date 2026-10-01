//! Shared tab drag previews and edge placement for editors and terminals.
use crate::{config::UiText, splits::Axis};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{prelude::FluentBuilder as _, *};

pub(crate) struct TabDragPreview(pub SharedString);

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .px_3()
            .py_1()
            .text_ui(cx)
            .rounded(cx.theme().radius)
            .bg(cx.theme().tab_active)
            .text_color(cx.theme().tab_active_foreground)
            .shadow_md()
            .child(self.0.clone())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DropPlacement {
    Center,
    Left,
    Right,
    Top,
    Bottom,
}

impl DropPlacement {
    pub(crate) fn at(bounds: Bounds<Pixels>, position: Point<Pixels>, can_split: bool) -> Option<Self> {
        if !bounds.contains(&position) || bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return None;
        }
        if !can_split {
            return Some(Self::Center);
        }
        let x = (position.x - bounds.left()) / bounds.size.width;
        let y = (position.y - bounds.top()) / bounds.size.height;
        // The nearest edge wins at corners; the middle half merges into the group.
        [(x, Self::Left), (1. - x, Self::Right), (y, Self::Top), (1. - y, Self::Bottom)]
            .into_iter()
            .filter(|(distance, _)| *distance < 0.25)
            .min_by(|(a, _), (b, _)| a.total_cmp(b))
            .map(|(_, placement)| placement)
            .or(Some(Self::Center))
    }

    pub(crate) fn split(self) -> Option<(Axis, usize)> {
        match self {
            Self::Center => None,
            Self::Left => Some((Axis::Row, 0)),
            Self::Right => Some((Axis::Row, 1)),
            Self::Top => Some((Axis::Column, 0)),
            Self::Bottom => Some((Axis::Column, 1)),
        }
    }

    pub fn indicator(self, cx: &App) -> Div {
        div()
            .absolute()
            .map(|el| match self {
                Self::Center => el.inset_0(),
                Self::Left => el.left_0().top_0().bottom_0().w(relative(0.5)),
                Self::Right => el.right_0().top_0().bottom_0().w(relative(0.5)),
                Self::Top => el.top_0().left_0().right_0().h(relative(0.5)),
                Self::Bottom => el.bottom_0().left_0().right_0().h(relative(0.5)),
            })
            .bg(cx.theme().primary.opacity(0.16))
            .border_1()
            .border_color(cx.theme().primary.opacity(0.65))
    }
}
