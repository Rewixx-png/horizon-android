//! Material Design 3 tokens for the Android launcher. Game screens stay
//! vanilla; this module only feeds the native launcher's shell and widgets.
//!
//! The palette is the official Material color-utilities dark scheme generated
//! from Horizon's violet seed `#7656FA`. Surface containers are the neutral
//! tonal roles (6/10/12/17/22), not opacity overlays.

use horizon_client::ui_theme::color::hex;
use std::time::{Duration, Instant};

// Primary / secondary / tertiary.
pub const PRIMARY: u32 = hex(0xcabeff, 255);
pub const ON_PRIMARY: u32 = hex(0x31009a, 255);
pub const PRIMARY_CONTAINER: u32 = hex(0x4715cb, 255);
pub const ON_PRIMARY_CONTAINER: u32 = hex(0xe6deff, 255);
pub const SECONDARY_CONTAINER: u32 = hex(0x484459, 255);
pub const ON_SECONDARY_CONTAINER: u32 = hex(0xe6dff9, 255);
pub const TERTIARY: u32 = hex(0xedb8cc, 255);

// Error.
pub const ERROR: u32 = hex(0xffb4ab, 255);

// Neutral surfaces: tone 4/6/10/12/17/22/24.
pub const SURFACE: u32 = hex(0x141316, 255);
pub const SURFACE_LOW: u32 = hex(0x1c1b1f, 255);
pub const SURFACE_CONTAINER: u32 = hex(0x201f23, 255);
pub const SURFACE_HIGH: u32 = hex(0x2b292d, 255);
pub const SURFACE_HIGHEST: u32 = hex(0x363438, 255);
pub const ON_SURFACE: u32 = hex(0xe6e1e6, 255);
pub const ON_SURFACE_VARIANT: u32 = hex(0xc9c4d0, 255);
pub const OUTLINE: u32 = hex(0x938f99, 255);
pub const OUTLINE_VARIANT: u32 = hex(0x48454e, 255);

// Legacy launcher names, mapped onto MD3 roles so the page code keeps one set
// of semantic names.
pub const BG: u32 = SURFACE;
pub const TEXT: u32 = ON_SURFACE;
pub const MUTED: u32 = ON_SURFACE_VARIANT;
pub const ACCENT: u32 = PRIMARY;
pub const RAISED: u32 = SURFACE_CONTAINER;
pub const BORDER: u32 = OUTLINE_VARIANT;
pub const DANGER: u32 = ERROR;
pub const SUCCESS: u32 = TERTIARY;

pub fn with_alpha(color: u32, alpha: f32) -> u32 {
    let base = (color >> 24) as f32;
    (color & 0x00ff_ffff) | ((base * alpha.clamp(0.0, 1.0)).round() as u32) << 24
}

// MD3 shape tokens.
pub const SHAPE_XS: f32 = 4.0;
pub const SHAPE_M: f32 = 12.0;
pub const SHAPE_XL: f32 = 28.0;

// MD3 type scale, in dp. The Ui draws line boxes separately where the MD3
// line height is not 1.45×size (all headlines and labels).
pub const HEADLINE: f32 = 28.0;
pub const TITLE: f32 = 22.0;
pub const TITLE_MEDIUM: f32 = 16.0;
pub const TITLE_SMALL: f32 = 14.0;
pub const BODY: f32 = 16.0;
pub const BODY_MEDIUM: f32 = 14.0;
pub const LABEL: f32 = 14.0;
pub const LABEL_SMALL: f32 = 11.0;
pub const HEADLINE_LINE: f32 = 36.0;
pub const TITLE_LINE: f32 = 28.0;
pub const TITLE_MEDIUM_LINE: f32 = 24.0;
pub const TITLE_SMALL_LINE: f32 = 20.0;
pub const BODY_LINE: f32 = 24.0;
pub const BODY_MEDIUM_LINE: f32 = 20.0;
pub const LABEL_LINE: f32 = 20.0;
pub const LABEL_SMALL_LINE: f32 = 16.0;

pub const APP_BAR: f32 = 64.0;
pub const TABS: f32 = 48.0;

// MD3 motion tokens.
pub const SHORT4: Duration = Duration::from_millis(200);
pub const MEDIUM2: Duration = Duration::from_millis(300);

fn axis(t: f32, a: f32, b: f32) -> f32 {
    let u = 1.0 - t;
    3.0 * u * u * t * a + 3.0 * u * t * t * b + t * t * t
}

/// CSS cubic-bezier(x1, y1, x2, y2) evaluated at x = `x`.
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, x: f32) -> f32 {
    let x = x.clamp(0.0, 1.0);
    let mut t = x;
    for _ in 0..6 {
        let current = axis(t, x1, x2);
        let delta = current - x;
        if delta.abs() < 1e-5 {
            break;
        }
        let slope = 3.0 * (1.0 - t).powi(2) * x1
            + 6.0 * (1.0 - t) * t * (x2 - x1)
            + 3.0 * t * t * (1.0 - x2);
        if slope.abs() < 1e-5 {
            break;
        }
        t = (t - delta / slope).clamp(0.0, 1.0);
    }
    axis(t, y1, y2)
}

pub fn emphasized_decelerate(t: f32) -> f32 {
    cubic_bezier(0.05, 0.7, 0.1, 1.0, t)
}

/// Launcher motion state: tab indicator, page fade-through and press ripple.
#[derive(Default)]
pub struct Motion {
    tab: f32,
    tab_target: f32,
    tab_alpha: f32,
    tab_alpha_target: f32,
    page: Option<Instant>,
    ripple: Option<(Instant, [f32; 4])>,
    last_tick: Option<Instant>,
}

impl Motion {
    pub fn new(tab: Option<usize>) -> Self {
        let value = tab.unwrap_or(0) as f32;
        let alpha = if tab.is_some() { 1.0 } else { 0.0 };
        Self {
            tab: value,
            tab_target: value,
            tab_alpha: alpha,
            tab_alpha_target: alpha,
            page: None,
            ripple: None,
            last_tick: None,
        }
    }

    /// A page was selected. `tab` is None for Settings, which lives in the app
    /// bar rather than the primary tabs.
    pub fn page_changed(&mut self, tab: Option<usize>) {
        if let Some(tab) = tab {
            self.tab_target = tab as f32;
            self.tab_alpha_target = 1.0;
        } else {
            self.tab_alpha_target = 0.0;
        }
        self.page = Some(Instant::now());
    }

    pub fn press(&mut self, rect: [f32; 4]) {
        self.ripple = Some((Instant::now(), rect));
    }

    /// Advance animated values; returns true while a frame is still needed.
    pub fn tick(&mut self, now: Instant) -> bool {
        let dt = self.last_tick.map_or(1.0 / 60.0, |last| {
            now.duration_since(last).as_secs_f32().min(0.1)
        });
        self.last_tick = Some(now);
        let k = 1.0 - (-dt / 0.12).exp();
        self.tab += (self.tab_target - self.tab) * k;
        self.tab_alpha += (self.tab_alpha_target - self.tab_alpha) * k;
        if (self.tab_target - self.tab).abs() < 0.005 {
            self.tab = self.tab_target;
        }
        if (self.tab_alpha_target - self.tab_alpha).abs() < 0.005 {
            self.tab_alpha = self.tab_alpha_target;
        }
        if self
            .ripple
            .is_some_and(|(start, _)| now.duration_since(start) >= SHORT4)
        {
            self.ripple = None;
        }
        self.animating(now)
    }

    pub fn animating(&self, now: Instant) -> bool {
        (self.tab_target - self.tab).abs() > 0.005
            || (self.tab_alpha_target - self.tab_alpha).abs() > 0.005
            || self
                .page
                .is_some_and(|start| now.duration_since(start) < MEDIUM2)
            || self.ripple.is_some()
    }

    pub fn tab(&self) -> f32 {
        self.tab
    }

    pub fn tab_alpha(&self) -> f32 {
        self.tab_alpha
    }

    /// A surface veil for the fade-through page transition.
    pub fn page_alpha(&self, now: Instant) -> f32 {
        let Some(start) = self.page else {
            return 0.0;
        };
        let elapsed = now.duration_since(start).as_secs_f32() / MEDIUM2.as_secs_f32();
        (1.0 - emphasized_decelerate(elapsed.clamp(0.0, 1.0))) * 0.92
    }

    pub fn ripple_alpha(&self, now: Instant) -> Option<([f32; 4], f32)> {
        let (start, rect) = self.ripple?;
        let elapsed = now.duration_since(start).as_secs_f32() / SHORT4.as_secs_f32();
        Some((rect, (1.0 - elapsed.clamp(0.0, 1.0)) * 0.12))
    }
}
