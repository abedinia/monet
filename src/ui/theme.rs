//! The palette. Every color has a truecolor value and a 16-color code; the
//! 256-color value is derived. Meaning never rests on color alone: arrows,
//! labels and structure carry it when color is off.
use super::canvas::{Color, Style};

/// Canvas behind everything.
pub const BG: Color = Color::new(5, 7, 14, 30);
/// Panel glass.
pub const SURFACE: Color = Color::new(10, 14, 27, 30);
/// Chips, selection and key caps.
pub const RAISED: Color = Color::new(28, 36, 64, 90);
pub const LINE: Color = Color::new(44, 54, 90, 90);
pub const MUTED: Color = Color::new(118, 128, 166, 37);
pub const TEXT: Color = Color::new(220, 226, 242, 97);
pub const WHITE: Color = Color::new(255, 255, 255, 97);

/// Download and inbound.
pub const CYAN: Color = Color::new(34, 211, 238, 96);
/// Upload and outbound.
pub const MAGENTA: Color = Color::new(236, 112, 246, 95);
/// Sockets and structure.
pub const VIOLET: Color = Color::new(150, 128, 255, 94);
pub const LIME: Color = Color::new(74, 222, 128, 92);
pub const AMBER: Color = Color::new(251, 191, 36, 93);
pub const RED: Color = Color::new(255, 99, 124, 91);

pub const fn ink(color: Color) -> Style {
    Style::fg(color)
}

/// Sink a color into the panel surface; 0 keeps it, 1 is the surface.
pub fn fade(color: Color, amount: f32) -> Color {
    color.mix(SURFACE, amount)
}

/// Lift a color toward white for crests and highlights.
pub fn glow(color: Color) -> Color {
    color.mix(WHITE, 0.4)
}

/// The signature cyan → violet → magenta sweep, wrapping back to cyan.
pub fn spectrum(t: f32) -> Color {
    let t = t.rem_euclid(1.0) * 3.0;
    match t {
        t if t < 1.0 => CYAN.mix(VIOLET, t),
        t if t < 2.0 => VIOLET.mix(MAGENTA, t - 1.0),
        t => MAGENTA.mix(CYAN, t - 2.0),
    }
}

/// Green for healthy, amber for busy, red for saturated.
pub fn severity(fraction: f64) -> Color {
    if fraction >= 0.8 { RED } else if fraction >= 0.5 { AMBER } else { LIME }
}
