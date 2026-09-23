use gpui::prelude::*;
use gpui::{App, Pixels, px};

use crate::theme::ActiveTheme as _;

/// How much of the popover colour a glass control keeps over what it blurs.
pub const GLASS_FILL: f32 = 0.2;
/// How hard a glass control blurs what is behind it.
pub const GLASS_BLUR: Pixels = px(8.);

/// Whether a surface that only frosts for the look of it should frost at all. A backdrop blur
/// is the priciest thing the renderer does per frame, so the setting turns those off and each
/// one falls back to a flat fill. The settings header and the fullscreen chrome frost whatever
/// it answers: their haze is what keeps text readable over the content passing under it.
pub fn blurring(cx: &App) -> bool {
    cx.theme().blur
}

/// Styles an element as frosted glass: a faint popover fill over a hard blur of
/// whatever it floats on. Over a flat colour the blur shows nothing, so the fill is all
/// that separates the control from the page there. A site the blur setting reaches asks
/// `blurring` before calling this, since the thin fill needs the wash to read.
pub fn glass<E: Styled>(element: E, cx: &App) -> E {
    let theme = cx.theme();

    element
        .bg(theme.popover.opacity(GLASS_FILL))
        .backdrop_blur(GLASS_BLUR)
}

/// Blurs what is behind an element by the glass width and nothing more, for a hover fill
/// that is already translucent. Only hovers that float over real content go through it;
/// on flat paint the blur shows nothing, so those stay plain fills. The caller asks
/// `blurring` first, since this one has no theme to read.
pub fn frost<E: Styled>(element: E) -> E {
    element.backdrop_blur(GLASS_BLUR)
}
