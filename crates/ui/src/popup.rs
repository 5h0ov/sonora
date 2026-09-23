use std::rc::Rc;

use gpui::prelude::*;
use gpui::{App, MouseButton, Pixels, Point, StyleRefinement, Window, anchored, point, px};

use crate::menu::{self, Menu};

const MARGIN: Pixels = px(8.);
/// How far the panel sits from the pointer. Opening it right under the cursor puts an item
/// beneath the button that is still down, so letting go without moving would pick it.
const NUDGE: Pixels = px(4.);

type Close = Box<dyn Fn(&(), &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Popup {
    at: Point<Pixels>,
    menu: Menu,
    close: Option<Close>,
}

impl Popup {
    pub fn new(at: Point<Pixels>, menu: Menu) -> Self {
        Self {
            at,
            menu,
            close: None,
        }
    }

    pub fn on_close(mut self, handler: impl Fn(&(), &mut Window, &mut App) + 'static) -> Self {
        self.close = Some(Box::new(handler));
        self
    }
}

impl Styled for Popup {
    fn style(&mut self) -> &mut StyleRefinement {
        self.menu.style()
    }
}

impl RenderOnce for Popup {
    fn render(self, _: &mut Window, _: &mut App) -> impl IntoElement {
        let Self { at, menu, close } = self;
        let menu = menu.pressed_at(at);
        let menu = match close {
            None => menu,
            Some(close) => {
                let close = Rc::new(close);
                let outside = close.clone();
                let toggled = close.clone();
                let searches = menu.searches();

                menu.on_dismiss(move |_, window, cx| outside(&(), window, cx))
                    .on_action(move |_, window, cx| close(&(), window, cx))
                    .on_mouse_down(MouseButton::Right, move |_, window, cx| {
                        cx.stop_propagation();
                        menu::release(&searches, window, cx);
                        toggled(&(), window, cx);
                    })
            }
        };

        anchored()
            .position(at + point(NUDGE, NUDGE))
            .snap_to_window_with_margin(MARGIN)
            .child(menu)
    }
}
