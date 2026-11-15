use std::{rc::Rc, time::Duration};

use gpui::{
    AnyElement, AnyView, App, Bounds, Context, DispatchPhase, Div, ElementId, InteractiveElement,
    IntoElement, MouseMoveEvent, ParentElement, Pixels, Point, Render, RenderOnce, Role, Stateful,
    StatefulInteractiveElement, Styled, Subscription, Task, Window, canvas, deferred, div,
    prelude::FluentBuilder as _, px,
};

use crate::{Placement, Positioner};

const TOOLTIP_PRIORITY: usize = 200;
const WINDOW_MARGIN: Pixels = px(4.);
const GRACE_PERIOD: Duration = Duration::from_millis(300);
const SHOW_DELAY: Duration = Duration::from_millis(500);

type TooltipBuilder = Rc<dyn Fn(&mut Window, &mut App) -> AnyView>;
type TooltipRenderer = Rc<dyn Fn(AnyView, TooltipTransition, &mut Window, &mut App) -> AnyElement>;

/// An unstyled tooltip popup.
///
/// This corresponds to Base UI's `Tooltip.Popup`: it owns the accessible
/// tooltip role and accepts application-owned content and presentation.
#[derive(IntoElement)]
pub struct Tooltip {
    base: Stateful<Div>,
}

impl Tooltip {
    pub fn new(id: impl Into<ElementId>) -> Self {
        Self {
            base: div().id(id).role(Role::Tooltip),
        }
    }
}

impl Styled for Tooltip {
    fn style(&mut self) -> &mut gpui::StyleRefinement {
        self.base.style()
    }
}

impl ParentElement for Tooltip {
    fn extend(&mut self, children: impl IntoIterator<Item = AnyElement>) {
        self.base.extend(children);
    }
}

impl RenderOnce for Tooltip {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        self.base
    }
}

/// Content requested by a tooltip trigger.
#[derive(Clone)]
pub struct TooltipRequest {
    build: TooltipBuilder,
    trigger_bounds: Bounds<Pixels>,
    preferred_placement: Option<Placement>,
}

impl TooltipRequest {
    pub fn new(
        trigger_bounds: Bounds<Pixels>,
        build: impl Fn(&mut Window, &mut App) -> AnyView + 'static,
    ) -> Self {
        Self {
            build: Rc::new(build),
            trigger_bounds,
            preferred_placement: None,
        }
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.preferred_placement = Some(placement);
        self
    }
}

/// Presentation transition requested by the Base tooltip lifecycle.
#[derive(Clone, Copy, Debug)]
pub enum TooltipTransition {
    Enter {
        epoch: usize,
    },
    Switch {
        epoch: usize,
        previous: Bounds<Pixels>,
        current: Bounds<Pixels>,
    },
}

/// Per-window tooltip provider and overlay.
///
/// Show requests are ignored on iOS and Android, where touch input must not
/// open hover tooltips. This does not control GPUI's native `.tooltip()` API.
pub struct TooltipOverlay {
    enabled: bool,
    content: Option<TooltipRequest>,
    /// (den) What `show_task` will show: the overlay watches the mouse for it
    /// too (see `render`).
    pending: Option<Bounds<Pixels>>,
    previous_bounds: Option<Bounds<Pixels>>,
    epoch: usize,
    had_recent_tooltip: bool,
    animation_epoch: usize,
    is_switching: bool,
    show_task: Option<Task<()>>,
    hide_task: Option<Task<()>>,
    /// (den) Hides the tooltip on any key in its window (see `request_show`).
    keys: Option<Subscription>,
    renderer: TooltipRenderer,
}

impl TooltipOverlay {
    pub fn new() -> Self {
        Self {
            enabled: !crate::is_mobile(),
            content: None,
            pending: None,
            previous_bounds: None,
            epoch: 0,
            had_recent_tooltip: false,
            animation_epoch: 0,
            is_switching: false,
            show_task: None,
            hide_task: None,
            keys: None,
            renderer: Rc::new(|view, _, _, _| div().child(view).into_any_element()),
        }
    }

    pub fn render_with(
        mut self,
        renderer: impl Fn(AnyView, TooltipTransition, &mut Window, &mut App) -> AnyElement + 'static,
    ) -> Self {
        self.renderer = Rc::new(renderer);
        self
    }

    fn next_epoch(&mut self) -> usize {
        self.epoch += 1;
        self.epoch
    }

    pub fn request_show(
        &mut self,
        content: TooltipRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // Gate both delayed display and the immediate grace-period switch.
        // Keep this in Base so every managed component shares the policy.
        if !self.enabled {
            return;
        }
        // (den) A key hides it, as GPUI's own tooltips: Escape may close what
        // the mouse is on, and nothing would say it was left. Keys go down the
        // focus path, which the overlay is not on, so it observes them all.
        if self.keys.is_none() {
            let own = window.window_handle();
            self.keys = Some(cx.observe_keystrokes(move |this, _, window, cx| {
                if window.window_handle() == own {
                    this.hide(cx);
                }
            }));
        }
        self.hide_task = None;
        let was_visible = self.content.is_some();
        if was_visible || self.had_recent_tooltip {
            self.previous_bounds = self.content.as_ref().map(|content| content.trigger_bounds);
            self.content = Some(content);
            self.show_task = None;
            self.pending = None;
            self.is_switching = was_visible;
            self.animation_epoch += 1;
            cx.notify();
            return;
        }

        let epoch = self.next_epoch();
        self.pending = Some(content.trigger_bounds);
        // (den) Drawn again so the overlay watches the mouse while it waits.
        cx.notify();
        self.show_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(SHOW_DELAY).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.epoch == epoch {
                    this.pending = None;
                    this.content = Some(content);
                    this.previous_bounds = None;
                    this.is_switching = false;
                    this.animation_epoch += 1;
                    cx.notify();
                }
            });
        }));
    }

    pub fn request_hide(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.show_task = None;
        if self.pending.take().is_some() {
            cx.notify();
        }
        if self.content.is_none() {
            return;
        }
        let epoch = self.next_epoch();
        self.had_recent_tooltip = true;
        self.hide_task = Some(cx.spawn_in(window, async move |this, cx| {
            cx.background_executor().timer(GRACE_PERIOD).await;
            let _ = this.update_in(cx, |this, _, cx| {
                if this.epoch == epoch {
                    this.content = None;
                    this.previous_bounds = None;
                    this.had_recent_tooltip = false;
                    cx.notify();
                }
            });
        }));
    }

    pub fn hide(&mut self, cx: &mut Context<Self>) {
        let changed = self.content.is_some()
            || self.pending.is_some()
            || self.previous_bounds.is_some()
            || self.had_recent_tooltip
            || self.show_task.is_some()
            || self.hide_task.is_some();
        self.content = None;
        self.pending = None;
        self.previous_bounds = None;
        self.had_recent_tooltip = false;
        self.is_switching = false;
        self.show_task = None;
        self.hide_task = None;
        if changed {
            cx.notify();
        }
    }

    /// (den) The mouse moved to `position`: off the trigger, the tooltip goes
    /// as if the trigger had been left. A trigger that went away under the
    /// mouse (a bar closed with Escape) never says it was left, and its
    /// tooltip stayed on screen for good.
    fn mouse_moved(
        &mut self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let trigger = self
            .content
            .as_ref()
            .map(|content| content.trigger_bounds)
            .or(self.pending);
        let hiding = self.content.is_some() && self.hide_task.is_some();
        if trigger.is_some_and(|bounds| !bounds.contains(&position)) && !hiding {
            self.request_hide(window, cx);
        }
    }
}

impl Default for TooltipOverlay {
    fn default() -> Self {
        Self::new()
    }
}

impl Render for TooltipOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // (den) While a tooltip is shown or about to be, the mouse leaving its
        // trigger hides it, whether or not the trigger is still drawn (see
        // `mouse_moved`).
        let watch = (self.content.is_some() || self.pending.is_some()).then(|| {
            let this = cx.weak_entity();
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                        if phase == DispatchPhase::Capture {
                            let _ = this.update(cx, |this, cx| {
                                this.mouse_moved(event.position, window, cx)
                            });
                        }
                    });
                },
            )
        });
        let Some(content) = self.content.as_ref() else {
            return div().children(watch).into_any_element();
        };
        let view = (content.build)(window, cx);
        let transition = match (self.is_switching, self.previous_bounds) {
            (true, Some(previous)) => TooltipTransition::Switch {
                epoch: self.animation_epoch,
                previous,
                current: content.trigger_bounds,
            },
            _ => TooltipTransition::Enter {
                epoch: self.animation_epoch,
            },
        };
        let rendered = (self.renderer)(view, transition, window, cx);
        div()
            .children(watch)
            .child(
                deferred(
                    TooltipPositioner::new(content.trigger_bounds)
                        .when_some(content.preferred_placement, |this, placement| {
                            this.placement(placement)
                        })
                        .child(rendered),
                )
                .with_priority(TOOLTIP_PRIORITY),
            )
            .into_any_element()
    }
}

/// An unstyled tooltip positioner with viewport-aware flipping and clamping.
///
/// This is a tooltip-named view of [`crate::Positioner`]'s side placement. It
/// adds no element of its own; the shared positioner is what gets rendered.
pub struct TooltipPositioner(Positioner);

impl TooltipPositioner {
    pub fn new(trigger_bounds: Bounds<Pixels>) -> Self {
        Self(Positioner::side(trigger_bounds).margin(WINDOW_MARGIN))
    }

    pub fn placement(mut self, placement: Placement) -> Self {
        self.0 = self.0.placement(placement);
        self
    }
}

impl ParentElement for TooltipPositioner {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.0.extend(elements);
    }
}

impl IntoElement for TooltipPositioner {
    type Element = Positioner;

    fn into_element(self) -> Self::Element {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, point, size};

    fn bounds(x: f32, y: f32, width: f32, height: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(x), px(y)), size(px(width), px(height)))
    }

    #[gpui::test]
    fn provider_owns_grace_switch_and_dismiss(cx: &mut gpui::TestAppContext) {
        let state = cx.update(|cx| cx.new(|_| TooltipOverlay::new()));
        let cx = cx.add_empty_window();
        cx.update(|window, cx| {
            state.update(cx, |tooltip, cx| {
                tooltip.had_recent_tooltip = true;
                tooltip.request_show(
                    TooltipRequest::new(bounds(0., 0., 20., 20.), |_, _| {
                        panic!("content is not rendered by this lifecycle test")
                    }),
                    window,
                    cx,
                );
            });
        });
        cx.update(|_, cx| assert!(state.read(cx).content.is_some()));

        cx.update(|_, cx| {
            state.update(cx, |tooltip, cx| tooltip.hide(cx));
        });
        cx.update(|_, cx| assert!(state.read(cx).content.is_none()));
    }

    /// (den) A trigger with a tooltip, as components wire it, that the test
    /// can take away while the mouse is on it.
    struct Bar {
        overlay: gpui::Entity<TooltipOverlay>,
        trigger: bool,
    }

    impl Render for Bar {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            let overlay = self.overlay.clone();
            div()
                .size_full()
                .when(self.trigger, |this| {
                    this.child(div().id("close").size(px(20.)).on_hover(
                        move |hovered, window, cx| {
                            overlay.update(cx, |overlay, cx| {
                                if *hovered {
                                    let request =
                                        TooltipRequest::new(bounds(0., 0., 20., 20.), |_, cx| {
                                            cx.new(|_| gpui::Empty).into()
                                        });
                                    overlay.request_show(request, window, cx);
                                } else {
                                    overlay.request_hide(window, cx);
                                }
                            })
                        },
                    ))
                })
                .child(self.overlay.clone())
        }
    }

    fn bar(cx: &mut gpui::TestAppContext) -> (gpui::Entity<Bar>, &mut gpui::VisualTestContext) {
        let overlay = cx.update(|cx| cx.new(|_| TooltipOverlay::new()));
        cx.add_window_view(|_, _| Bar {
            overlay,
            trigger: true,
        })
    }

    fn shown(bar: &gpui::Entity<Bar>, cx: &mut gpui::VisualTestContext) -> bool {
        cx.update(|_, cx| bar.read(cx).overlay.read(cx).content.is_some())
    }

    fn take_trigger_away(bar: &gpui::Entity<Bar>, cx: &mut gpui::VisualTestContext) {
        cx.update(|_, cx| {
            bar.update(cx, |bar, cx| {
                bar.trigger = false;
                cx.notify();
            })
        });
        cx.run_until_parked();
    }

    /// (den) A trigger gone from under the mouse never says it was left: its
    /// tooltip goes once the mouse moves off it, or with a key (below).
    #[gpui::test]
    fn tooltip_of_a_trigger_gone_goes_with_the_mouse(cx: &mut gpui::TestAppContext) {
        let none = gpui::Modifiers::default();
        let (bar, cx) = bar(cx);
        cx.run_until_parked();
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, none);
        cx.executor().advance_clock(SHOW_DELAY);
        cx.run_until_parked();
        assert!(shown(&bar, cx));
        take_trigger_away(&bar, cx);
        cx.simulate_mouse_move(point(px(12.), px(12.)), None, none);
        cx.run_until_parked();
        assert!(shown(&bar, cx), "still on where the trigger was");
        cx.simulate_mouse_move(point(px(100.), px(100.)), None, none);
        cx.executor().advance_clock(GRACE_PERIOD);
        cx.run_until_parked();
        assert!(!shown(&bar, cx));
    }

    #[gpui::test]
    fn tooltip_of_a_trigger_gone_goes_with_a_key(cx: &mut gpui::TestAppContext) {
        let (bar, cx) = bar(cx);
        cx.run_until_parked();
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, gpui::Modifiers::default());
        cx.executor().advance_clock(SHOW_DELAY);
        cx.run_until_parked();
        assert!(shown(&bar, cx));
        take_trigger_away(&bar, cx);
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
        assert!(!shown(&bar, cx));
    }

    /// (den) Taken away before its delay is up, with the mouse gone from where
    /// it was, it never shows.
    #[gpui::test]
    fn tooltip_waiting_for_a_trigger_gone_never_shows(cx: &mut gpui::TestAppContext) {
        let none = gpui::Modifiers::default();
        let (bar, cx) = bar(cx);
        cx.run_until_parked();
        cx.simulate_mouse_move(point(px(10.), px(10.)), None, none);
        cx.run_until_parked();
        take_trigger_away(&bar, cx);
        cx.simulate_mouse_move(point(px(100.), px(100.)), None, none);
        cx.executor().advance_clock(SHOW_DELAY);
        cx.run_until_parked();
        assert!(!shown(&bar, cx));
    }

    #[test]
    fn tooltip_priority_exceeds_popup_layer() {
        assert!(TOOLTIP_PRIORITY > crate::POPUP_PRIORITY);
    }

    #[gpui::test]
    fn disabled_provider_ignores_delayed_and_immediate_requests(cx: &mut gpui::TestAppContext) {
        let state = cx.update(|cx| {
            cx.new(|_| TooltipOverlay {
                enabled: false,
                ..TooltipOverlay::new()
            })
        });
        let cx = cx.add_empty_window();
        for had_recent_tooltip in [false, true] {
            cx.update(|window, cx| {
                state.update(cx, |tooltip, cx| {
                    tooltip.had_recent_tooltip = had_recent_tooltip;
                    tooltip.request_show(
                        TooltipRequest::new(bounds(0., 0., 20., 20.), |_, _| {
                            panic!("disabled tooltips must not build content")
                        }),
                        window,
                        cx,
                    );
                    assert!(tooltip.content.is_none());
                    assert!(tooltip.show_task.is_none());
                    assert!(tooltip.hide_task.is_none());
                    assert_eq!(tooltip.animation_epoch, 0);
                });
            });
        }
    }
}
