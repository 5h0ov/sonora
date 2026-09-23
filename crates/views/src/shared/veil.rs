use gpui::prelude::*;
use gpui::{Div, Hsla, Pixels, Window, div};
use ui::snapped;

use crate::shared::effects;

/// How many strips the blur fades in through. They cost nothing beyond a quad each, so more of
/// them only make the crossfade smoother.
const STRIPS: usize = 64;
/// The curve of the crossfade: the exponent on a strip's distance from the solid edge. One is
/// linear, above one starts slower, below one starts faster. Kept well below one so the haze
/// reaches working strength quickly and only the far edge reads as clear.
const HAZE: f32 = 0.4;

/// Which side of the band holds the full blur. The other one is clear.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Edge {
    Top,
    Bottom,
}

/// A band that blurs what passes under it, at full strength along `edge` and fading to nothing
/// across `height`, so chrome floating over content stays readable without a hard line where
/// the treatment stops. It fills its parent, which has to be `height` tall and `relative`.
///
/// The renderer blurs a run of consecutive backdrops once, by the widest radius among them, and
/// honours each one's opacity, so the strips cost one blur pass a frame. Interleaving anything
/// between them splits that into a pass each, which lags. With effects off there is no blur to
/// fade and the band is the plain colour instead. The Blur UI setting never reaches it: a band
/// is there to keep what floats over it readable, not for the look of it.
pub(crate) fn veil(
    edge: Edge,
    height: Pixels,
    blur: Pixels,
    background: Hsla,
    window: &Window,
) -> Div {
    let edges: Vec<Pixels> = (0..=STRIPS)
        .map(|slice| snapped(height * (slice as f32 / STRIPS as f32), window))
        .collect();
    let strips = edges
        .windows(2)
        .enumerate()
        .filter_map(move |(slice, span)| {
            let cut = span[1] - span[0];
            let across = (slice as f32 + 0.5) / STRIPS as f32;
            let held = match edge {
                Edge::Top => 1. - across,
                Edge::Bottom => across,
            };
            (cut > Pixels::ZERO).then(|| {
                div()
                    .flex_none()
                    .w_full()
                    .h(cut)
                    .opacity(held.powf(HAZE))
                    .backdrop_blur(blur)
            })
        });

    match effects() {
        true => div()
            .absolute()
            .inset_0()
            .flex()
            .flex_col()
            .children(strips),
        false => div().absolute().inset_0().bg(background),
    }
}
