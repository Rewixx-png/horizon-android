use super::*;
use horizon_client::{
    launcher_i18n::{Language, tr as translate},
    osk::Output,
    tr,
    ui_theme::color,
};
use winit::{
    event::{KeyEvent, Touch},
    keyboard::{KeyCode, PhysicalKey},
};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Action {
    Page(Page),
    Play,
    Search,
    ShaderSource(crate::shader_catalog::Origin),
    ModSource(crate::shader_catalog::Origin),
    Field(&'static str),
    FinishNameEdit,
    Install(usize),
    Import(horizon_client::content_files::Kind),
    OpenFolder(horizon_client::content_files::Kind),
    ToggleSection(usize),
    ToggleLocal(usize),
    DeleteLocal(usize),
    ConfirmDeleteLocal,
    Next,
    Previous,
    Profile,
    Logs,
    RecordPerformance,
    ExportPerformance,
    ExportDiagnostics,
    Account,
    SignIn,
    DeviceSignIn,
    CopyLoginCode,
    CancelLogin,
    ReviewApp,
    Repository,
    SignOut,
    LoginLink,
    CloseLogin,
    Renderer,
    RenderScale,
    ChooseRenderScale(crate::display::RenderScale),
    Orientation,
    InputHost,
    ChooseInputHost(crate::catalog::InputHost),
    ChooseOrientation(crate::catalog::Orientation),
    ChooseRenderer(crate::catalog::RendererMode),
    ChooseProfile(u32),
    Dismiss,
    Immersive,
    Cutout,
    Aspect,
    ChooseAspect(crate::display::Aspect),
    DisplayHz,
    ChooseDisplayHz(u16),
    PerformanceHints,
    GpuProfile,
    Language,
    ChooseLanguage(Language),
    DownloadUpdate,
    RetryInstall,
}

/// The phone gutter from the design system: 16 dp, 32 dp on tablets.
pub(super) fn gutter(width: f32) -> f32 {
    if width < 700.0 { 16.0 } else { 32.0 }
}

/// An action control. Pills are the launcher's default shape; the text is
/// centred and fitted, so a translated label never runs outside the target.
pub(super) fn button(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    label: &str,
    action: Action,
    accent: bool,
) {
    let [x, y, w, h] = rect;
    let radius = (h / 2.0).min(24.0);
    ui.rounded(
        x,
        y,
        w,
        h,
        radius,
        if accent { color::ACCENT } else { color::RAISED },
    );
    let padding = 14.0_f32.min(w * 0.12);
    let mut size = if h >= 48.0 { 16.0 } else { 15.0 };
    let mut label = ui.fit_text(Face::Medium, size, label, w - 2.0 * padding);
    while size > 11.0 && ui.measure(Face::Medium, size, &label) > w - 2.0 * padding {
        size -= 1.0;
        label = ui.fit_text(Face::Medium, size, &label, w - 2.0 * padding);
    }
    let text_w = ui.measure(Face::Medium, size, &label);
    ui.text(
        Face::Medium,
        size,
        x + (w - text_w) / 2.0,
        y + (h - size * 1.45) / 2.0,
        if accent {
            color::ON_ACCENT
        } else {
            color::TEXT
        },
        &label,
    );
    targets.push((rect, action));
}

/// A text field: left-aligned, sunk into a soft plate, with an accent focus
/// ring instead of a permanent border.
pub(super) fn field(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    text: &str,
    action: Action,
    focused: bool,
) {
    let [x, y, w, h] = rect;
    ui.rounded(x, y, w, h, 8.0, color::RAISED);
    if focused {
        ui.border(x, y, w, h, 8.0, color::ACCENT);
    }
    let text = ui.fit_text(Face::Medium, 15.0, text, w - 24.0);
    ui.text(
        Face::Medium,
        15.0,
        x + 12.0,
        y + (h - 15.0 * 1.45) / 2.0,
        color::TEXT,
        &text,
    );
    targets.push((rect, action));
}

fn toggle(
    ui: &mut Ui,
    targets: &mut Vec<([f32; 4], Action)>,
    rect: [f32; 4],
    on: bool,
    action: Action,
    language: Language,
) {
    let [x, y, w, h] = rect;
    if w >= 80.0 {
        ui.text(
            Face::Regular,
            14.0,
            x,
            y + (h - 14.0 * 1.45) / 2.0,
            color::MUTED,
            translate(language, if on { "On" } else { "Off" }),
        );
    }
    let track = 44.0_f32.min(w);
    let knob = 18.0;
    ui.rounded(
        x + w - track,
        y + (h - 24.0) / 2.0,
        track,
        24.0,
        12.0,
        if on { color::ACCENT } else { color::HOVER },
    );
    ui.rounded(
        x + w - track + if on { track - knob - 3.0 } else { 3.0 },
        y + (h - knob) / 2.0,
        knob,
        knob,
        knob / 2.0,
        if on { color::ON_ACCENT } else { color::TEXT },
    );
    targets.push((rect, action));
}

pub(super) fn rule(ui: &mut Ui, x: f32, y: f32, w: f32) {
    ui.rounded(x, y, w, 1.0, 0.0, color::BORDER);
}

/// A page's display title and one-line description. Returns the content top.
pub(super) fn page_header_height(w: f32) -> f32 {
    let size = if w < 420.0 { 24.0 } else { 28.0 };
    size * 1.45 + 2.0 + 14.0 * 1.45 + 14.0
}

pub(super) fn page_header(ui: &mut Ui, title: &str, caption: &str, x: f32, y: f32, w: f32) -> f32 {
    let size = if w < 420.0 { 24.0 } else { 28.0 };
    let title = ui.fit_text(Face::Medium, size, title, w);
    let mut title_size = size;
    while title_size > 20.0 && ui.measure(Face::Medium, title_size, &title) > w {
        title_size -= 1.0;
    }
    ui.text(Face::Medium, title_size, x, y, color::TEXT, &title);
    let caption = ui.fit_text(Face::Regular, 14.0, caption, w);
    ui.text(
        Face::Regular,
        14.0,
        x,
        y + title_size * 1.45 + 2.0,
        color::MUTED,
        &caption,
    );
    y + title_size * 1.45 + 2.0 + 14.0 * 1.45 + 14.0
}

/// A fraction eased between frames: 0 is the frame the value changed, 1 settled.
/// `phase` is 0..1 of the way through `MILLIS`, so every bar moves at one pace.
pub(super) fn eased(phase: f32) -> f32 {
    // ease-out-cubic: quick to start, settling at the end.
    let t = 1.0 - phase.clamp(0.0, 1.0);
    1.0 - t * t * t
}

/// 0..1 that breathes once a second, for a dot that says "working".
pub(super) fn pulse(now: std::time::Instant, period_ms: u64) -> f32 {
    let ms = now.elapsed().as_millis() as u64 % period_ms;
    let t = ms as f32 / period_ms as f32;
    // sine, so it never jumps back to 0 hard.
    (t * std::f32::consts::TAU).sin() * 0.5 + 0.5
}

/// `color::ACCENT` at `alpha` 0..1, for the breathing dot.
pub(super) fn accent_at(alpha: f32) -> u32 {
    let a = (alpha.clamp(0.0, 1.0) * 255.0) as u8;
    (a as u32) << 24 | (color::ACCENT & 0x00ff_ffff)
}

pub(super) fn progress_bar(ui: &mut Ui, x: f32, y: f32, w: f32, fraction: f32) {
    ui.rounded(x, y, w, 4.0, 2.0, color::RAISED);
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.0 {
        ui.rounded(x, y, w * fraction, 4.0, 2.0, color::ACCENT);
    }
}
impl Launcher {
    /// The same spatial focus algorithm as the game's controller menus.
    pub(super) fn navigate(&mut self, key: &KeyEvent) -> bool {
        self.navigate_key(&key.logical_key)
    }
    pub(super) fn navigate_key(&mut self, key: &winit::keyboard::Key) -> bool {
        use horizon_client::gamepad::{self, Button};
        use winit::keyboard::{Key, NamedKey};
        if self.pending_focus.is_some() {
            return true;
        }
        if self.catalog.editing.is_some() || self.targets.is_empty() {
            return false;
        }
        let button = match key {
            Key::Named(NamedKey::ArrowUp) => Button::DPadUp,
            Key::Named(NamedKey::ArrowDown) => Button::DPadDown,
            Key::Named(NamedKey::ArrowLeft) => Button::DPadLeft,
            Key::Named(NamedKey::ArrowRight) => Button::DPadRight,
            Key::Named(NamedKey::Tab) => {
                self.focus = Some(self.focus.map_or(0, |i| (i + 1) % self.targets.len()));
                return true;
            }
            _ => return false,
        };
        if matches!(button, Button::DPadUp | Button::DPadDown)
            && let Some((_, action)) = self.focus.and_then(|i| self.targets.get(i))
            && let Some(index) = self.list_targets.iter().position(|(_, a)| a == action)
        {
            let rects: Vec<_> = self.list_targets.iter().map(|(r, _)| *r).collect();
            if let Some(next) = gamepad::focus_step_strict(rects[index], button, &rects)
                && let Some(view) = self.list_view
            {
                let (rect, action) = self.list_targets[next];
                let scroll = if let Some(chooser) = &mut self.chooser {
                    &mut chooser.scroll
                } else {
                    &mut self.catalog.scroll
                };
                *scroll = super::gesture::reveal(*scroll, rect, view);
                self.pending_focus = Some(action);
                self.focus = None;
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
                return true;
            }
        }
        self.focus = Some(
            if let Some(index) = self.focus.filter(|i| *i < self.targets.len()) {
                let rects: Vec<_> = self.targets.iter().map(|(rect, _)| *rect).collect();
                gamepad::focus_step_rect(rects[index], button, &rects).unwrap_or(index)
            } else {
                0
            },
        );
        self.redraw();
        true
    }

    /// The fixed app bar and navigation, drawn after the page so a scrolled
    /// page header slides under an opaque mask instead of over the tabs.
    fn draw_chrome(
        ui: &mut Ui,
        targets: &mut Vec<([f32; 4], Action)>,
        account_label: &str,
        language: Language,
        compact: bool,
        margin: f32,
        content_w: f32,
        nav_bottom: f32,
        current_page: Page,
    ) {
        let w = ui.w;
        ui.rounded(0.0, 0.0, w, nav_bottom + 12.0, 0.0, color::BG);
        ui.text(
            Face::Medium,
            if compact { 20.0 } else { 22.0 },
            margin,
            12.0,
            color::TEXT,
            "Horizon",
        );
        let brand_w = ui.measure(Face::Medium, if compact { 20.0 } else { 22.0 }, "Horizon");
        ui.text(
            Face::Regular,
            11.0,
            margin + brand_w + 8.0,
            19.0,
            color::MUTED,
            crate::backend::VERSION,
        );
        let account_w = (content_w * 0.46).clamp(140.0, 260.0);
        button(
            ui,
            targets,
            [w - margin - account_w, 8.0, account_w, 36.0],
            account_label,
            Action::Account,
            false,
        );

        let nav_top = 56.0;
        let columns = if compact && content_w < 560.0 { 3 } else { 6 };
        let rows = Page::ALL.len().div_ceil(columns);
        let gap = 6.0;
        let tab_h = if rows == 2 { 44.0 } else { 46.0 };
        let tab_w = (content_w - gap * (columns as f32 - 1.0)) / columns as f32;
        for (i, page) in Page::ALL.iter().enumerate() {
            let column = i % columns;
            let row = i / columns;
            let rect = [
                margin + column as f32 * (tab_w + gap),
                nav_top + row as f32 * (tab_h + gap),
                tab_w,
                tab_h,
            ];
            let selected = *page == current_page;
            let label = translate(language, page.name());
            let [x, y, tw, th] = rect;
            ui.rounded(
                x,
                y,
                tw,
                th,
                (th / 2.0).min(24.0),
                if selected { color::TEXT } else { color::RAISED },
            );
            let size = if compact { 13.0 } else { 15.0 };
            let label = ui.fit_text(Face::Medium, size, label, tw - 16.0);
            let text_w = ui.measure(Face::Medium, size, &label);
            ui.text(
                Face::Medium,
                size,
                x + (tw - text_w) / 2.0,
                y + (th - size * 1.45) / 2.0,
                if selected { color::BG } else { color::TEXT },
                &label,
            );
            targets.push((rect, Action::Page(*page)));
        }
        rule(ui, margin, nav_bottom + 10.0, content_w);
    }

    pub(super) fn draw_pages(&mut self) -> Result<()> {
        let (Some(r), Some(gui)) = (&mut self.renderer, &self.gui) else {
            return Ok(());
        };
        if r.suspended {
            return Ok(());
        }
        self.targets.clear();
        self.list_targets.clear();
        self.list_view = None;
        let mut b = HudBatch::new([r.config.width, r.config.height]);
        let mut ui = Ui::mobile(&gui.atlas, &mut b, (0.0, 0.0));
        // OEM navigation bars can remain visible even in fullscreen (keypad phones).
        // Inset the launcher layout only; keep surface and input coordinates intact.
        if let Some(window) = &self.window {
            use winit::platform::android::WindowExtAndroid;
            let rect = window.content_rect();
            let input = winit::platform::android::native_input_scale();
            if rect.bottom > 0 {
                ui.h = ui.h.min(rect.bottom as f32 * input[1] as f32 / ui.density);
            }
            if rect.right > 0 {
                ui.w = ui.w.min(rect.right as f32 * input[0] as f32 / ui.density);
            }
        }
        if let Some(bottom) = crate::native_ime::viewport_bottom() {
            ui.h = ui.h.min(bottom as f32 / ui.density);
        }
        let (w, h) = (ui.w, ui.h);
        let scale = ui.density / ui.batch.scale;
        ui.batch
            .fill(&gui.atlas, 0.0, 0.0, w * scale, h * scale, color::BG);
        let language = self.catalog.config.language;
        let compact = w < 700.0;
        let margin = gutter(w);
        let content_w = w - 2.0 * margin;

        let nav_top = 56.0;
        let columns = if compact && content_w < 560.0 { 3 } else { 6 };
        let rows = Page::ALL.len().div_ceil(columns);
        let gap = 6.0;
        let tab_h = if rows == 2 { 44.0 } else { 46.0 };
        let nav_bottom = nav_top + rows as f32 * tab_h + (rows as f32 - 1.0) * gap;
        let page_top = nav_bottom + 18.0;
        let content_bottom = h - 64.0;
        let page = self.catalog.page;
        if page == Page::Play {
            use horizon_client::official_install::InstallStage;
            let installed = horizon_client::official_install::ready(&self.home);
            let installing = self.worker.is_some();
            let progress = *self.install.lock();
            let title = if installing {
                translate(language, "Getting your game ready")
            } else if installed {
                translate(language, "Ready to play")
            } else if compact {
                translate(language, "Your Java worlds")
            } else {
                translate(language, "Your Java worlds, on Android")
            };
            let caption = tr!(language, "Minecraft 26.3  /  Profile {profile}", {
                profile = self.catalog.config.profile
            });
            let mut top = page_header(&mut ui, title, &caption, margin, page_top, content_w);
            let tight = h < 560.0;
            let error = self.update.error().map(|error| {
                if error
                    == "Allow installs from Horizon in Android settings, then tap Retry installation."
                {
                    translate(
                        language,
                        "Allow installs from Horizon in Android settings, then tap Retry installation.",
                    )
                } else {
                    error
                }
            });
            if let Some(release) = self.update.available() {
                let card_h = if tight {
                    52.0
                } else if compact {
                    100.0
                } else {
                    92.0
                };
                ui.rounded(margin, top, content_w, card_h, 10.0, color::RAISED);
                let dot_color = if error.is_some() {
                    color::DANGER
                } else {
                    // Breathes while it works: the card only shows while something runs.
                    accent_at(0.45 + 0.55 * pulse(std::time::Instant::now(), 1400))
                };
                let dot = 6.0 + 2.0 * pulse(std::time::Instant::now(), 1400);
                ui.rounded(
                    margin + 14.0 - (dot - 6.0) / 2.0,
                    top + 16.0 - (dot - 6.0) / 2.0,
                    dot,
                    dot,
                    dot / 2.0,
                    dot_color,
                );
                let title = if error.is_some() {
                    translate(language, "Update failed").to_owned()
                } else if tight {
                    translate(language, "New version available").to_owned()
                } else {
                    tr!(language, "New version {version} is available", {
                        version = release.version
                    })
                };
                if tight {
                    let title = ui.fit_text(Face::Medium, 13.0, &title, content_w - 190.0);
                    ui.text(
                        Face::Medium,
                        13.0,
                        margin + 30.0,
                        top + (card_h - 13.0 * 1.45) / 2.0,
                        color::TEXT,
                        &title,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - margin - 152.0, top + 8.0, 138.0, 36.0],
                        if error.is_some() {
                            translate(language, "Retry")
                        } else {
                            translate(language, "Download")
                        },
                        Action::DownloadUpdate,
                        false,
                    );
                } else {
                    let title = ui.fit_text(Face::Medium, 16.0, &title, content_w - 56.0);
                    ui.text(
                        Face::Medium,
                        16.0,
                        margin + 30.0,
                        top + 10.0,
                        color::TEXT,
                        &title,
                    );
                    let caption = if let Some(error) = error {
                        tr!(language, "Update failed: {error}", { error = error })
                    } else {
                        translate(language, "Tap to download and install").to_owned()
                    };
                    let caption = ui.fit_text(Face::Regular, 13.0, &caption, content_w - 56.0);
                    ui.text(
                        Face::Regular,
                        13.0,
                        margin + 30.0,
                        top + 34.0,
                        color::MUTED,
                        &caption,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        [margin + 20.0, top + card_h - 46.0, content_w - 40.0, 36.0],
                        if error.is_some() {
                            translate(language, "Retry")
                        } else {
                            translate(language, "Download and install")
                        },
                        Action::DownloadUpdate,
                        false,
                    );
                }
                top += card_h + 10.0;
            } else if self.update.downloaded().is_some() {
                let card_h = if tight {
                    52.0
                } else if compact {
                    100.0
                } else {
                    92.0
                };
                ui.rounded(margin, top, content_w, card_h, 10.0, color::RAISED);
                ui.rounded(
                    margin + 14.0,
                    top + 16.0,
                    6.0,
                    6.0,
                    3.0,
                    if error.is_some() {
                        color::DANGER
                    } else {
                        color::ACCENT
                    },
                );
                let title = translate(language, "Update downloaded");
                if tight {
                    let title = ui.fit_text(Face::Medium, 13.0, title, content_w - 190.0);
                    ui.text(
                        Face::Medium,
                        13.0,
                        margin + 30.0,
                        top + (card_h - 13.0 * 1.45) / 2.0,
                        color::TEXT,
                        &title,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - margin - 152.0, top + 8.0, 138.0, 36.0],
                        translate(language, "Install"),
                        Action::RetryInstall,
                        false,
                    );
                } else {
                    let title = ui.fit_text(Face::Medium, 16.0, title, content_w - 56.0);
                    ui.text(
                        Face::Medium,
                        16.0,
                        margin + 30.0,
                        top + 10.0,
                        color::TEXT,
                        &title,
                    );
                    let caption = match error {
                        Some(error) => {
                            tr!(language, "Update failed: {error}", { error = error })
                        }
                        None => translate(language, "Ready to install").to_owned(),
                    };
                    let caption = ui.fit_text(Face::Regular, 13.0, &caption, content_w - 56.0);
                    ui.text(
                        Face::Regular,
                        13.0,
                        margin + 30.0,
                        top + 34.0,
                        color::MUTED,
                        &caption,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        [margin + 20.0, top + card_h - 46.0, content_w - 40.0, 36.0],
                        translate(language, "Install"),
                        Action::RetryInstall,
                        false,
                    );
                }
                top += card_h + 10.0;
            } else if let Some(progress) = self.update.downloading() {
                let card_h = if tight {
                    52.0
                } else if compact {
                    88.0
                } else {
                    80.0
                };
                ui.rounded(margin, top, content_w, card_h, 10.0, color::RAISED);
                let title = tr!(language, "Downloading update... {percent}%", {
                    percent = (progress * 100.0).round() as u32
                });
                let title = ui.fit_text(
                    Face::Medium,
                    if tight { 13.0 } else { 16.0 },
                    &title,
                    content_w - 40.0,
                );
                ui.text(
                    Face::Medium,
                    if tight { 13.0 } else { 16.0 },
                    margin + 20.0,
                    top + if tight { 8.0 } else { 10.0 },
                    color::TEXT,
                    &title,
                );
                progress_bar(
                    &mut ui,
                    margin + 20.0,
                    top + if tight { 34.0 } else { 40.0 },
                    content_w - 40.0,
                    progress,
                );
                top += card_h + 10.0;
            }
            if installing {
                let current = InstallStage::ALL
                    .iter()
                    .position(|stage| *stage == progress.stage)
                    .unwrap_or(0);
                let available = (content_bottom - top - 108.0).max(0.0);
                let row_height = (available / 6.0).clamp(28.0, 44.0);
                let show_all = available >= 6.0 * 34.0;
                for (index, stage) in InstallStage::ALL.iter().enumerate() {
                    if !show_all && index != current {
                        continue;
                    }
                    let y = top
                        + if show_all {
                            index as f32 * row_height
                        } else {
                            0.0
                        };
                    let number = format!("{:02}", index + 1);
                    ui.text(Face::Regular, 11.0, margin, y + 4.0, color::MUTED, &number);
                    let label = translate(language, stage.label());
                    let label = ui.fit_text(Face::Regular, 15.0, label, content_w - 34.0);
                    ui.text(
                        Face::Regular,
                        15.0,
                        margin + 30.0,
                        y + 2.0,
                        if index == current {
                            color::TEXT
                        } else {
                            color::MUTED
                        },
                        &label,
                    );
                    if index < current {
                        ui.text(
                            Face::Medium,
                            12.0,
                            w - margin - 20.0,
                            y + 3.0,
                            color::SUCCESS,
                            "OK",
                        );
                    }
                    rule(&mut ui, margin, y + row_height - 6.0, content_w);
                }
                let bar_y = content_bottom - 52.0;
                let fraction = if progress.total > 0 {
                    progress.done as f32 / progress.total as f32
                } else {
                    0.0
                };
                let overall = (current as f32 + fraction.clamp(0.0, 1.0)) / 6.0;
                progress_bar(&mut ui, margin, bar_y, content_w, overall);
                let detail = match progress.stage {
                    InstallStage::Download => {
                        // The pace and what is left of it, so a slow network reads as the
                        // network rather than as a stuck installer.
                        let speed = progress.bytes_per_second as f64 / 1048576.0;
                        let left = progress.seconds_left;
                        if speed >= 0.1 {
                            tr!(
                                language,
                                "{done} / {total} MiB  -  {speed} MiB/s  -  {left} left",
                                {
                                    done = format!("{:.1}", progress.done as f64 / 1048576.0),
                                    total = format!("{:.1}", progress.total as f64 / 1048576.0),
                                    speed = format!("{speed:.1}"),
                                    left = if left >= 60 {
                                        tr!(language, "{n} min", { n = left / 60 })
                                    } else {
                                        tr!(language, "{n} s", { n = left })
                                    }
                                }
                            )
                        } else {
                            tr!(
                                language,
                                "{done} / {total} MiB  - verified downloads",
                                {
                                    done = format!("{:.1}", progress.done as f64 / 1048576.0),
                                    total = format!("{:.1}", progress.total as f64 / 1048576.0)
                                }
                            )
                        }
                    }
                    InstallStage::Controls => tr!(
                        language,
                        "{done} / {total} control files verified",
                        { done = progress.done, total = progress.total }
                    ),
                    _ => translate(
                        language,
                        "Please keep Horizon open. Downloaded files are kept for retry.",
                    )
                    .to_owned(),
                };
                let detail = ui.fit_text(Face::Regular, 13.0, &detail, content_w);
                ui.text(
                    Face::Regular,
                    13.0,
                    margin,
                    bar_y + 12.0,
                    color::MUTED,
                    &detail,
                );
            } else {
                let rows = if installed {
                    [
                        "01  Open a world or join a Java server",
                        "02  Touch setup: Options > Controls",
                        "03  Find packs and .hmod in the tabs",
                    ]
                } else {
                    [
                        "01  Download resources from Mojang",
                        "02  Install and verify on this phone",
                        "03  Play offline or sign in for servers",
                    ]
                };
                let button_top = content_bottom - 52.0;
                let rows_bottom = if self.error.is_some() {
                    content_bottom - 126.0
                } else {
                    button_top - 8.0
                };
                for (index, line) in rows.iter().enumerate() {
                    let y = top + index as f32 * 46.0;
                    if y + 38.0 > rows_bottom {
                        break;
                    }
                    let line = translate(language, line);
                    let line = ui.fit_text(Face::Regular, 16.0, line, content_w);
                    ui.text(Face::Regular, 16.0, margin, y + 4.0, color::TEXT, &line);
                    rule(&mut ui, margin, y + 38.0, content_w);
                }
                if let Some(error) = &self.error
                    && top < content_bottom - 126.0
                {
                    let text = ui.fit_text(Face::Regular, 13.0, error, content_w - 24.0);
                    ui.rounded(
                        margin,
                        content_bottom - 118.0,
                        content_w,
                        44.0,
                        8.0,
                        color::RAISED,
                    );
                    ui.rounded(
                        margin + 12.0,
                        content_bottom - 101.0,
                        6.0,
                        6.0,
                        3.0,
                        color::DANGER,
                    );
                    ui.text(
                        Face::Regular,
                        13.0,
                        margin + 26.0,
                        content_bottom - 108.0,
                        color::TEXT,
                        &text,
                    );
                }
                button(
                    &mut ui,
                    &mut self.targets,
                    [margin, content_bottom - 52.0, content_w.min(360.0), 52.0],
                    if installed {
                        translate(language, "Play")
                    } else if self.error.is_some() {
                        translate(language, "Retry installation")
                    } else {
                        translate(language, "Download Minecraft 26.3")
                    },
                    Action::Play,
                    true,
                );
            }
        } else if page.is_catalog() {
            let title = match page {
                Page::Mods => translate(language, "Horizon Mods"),
                Page::Shaders if compact => translate(language, "Shaders"),
                Page::Shaders => translate(language, "Shader Packs"),
                _ => translate(language, "Resource Packs"),
            };
            let hint = match page {
                Page::Mods
                    if self.catalog.config.mod_source
                        == crate::shader_catalog::Origin::Modrinth =>
                {
                    "Modrinth Java mods are browse-only; Fabric/Forge/NeoForge cannot run here"
                }
                Page::Mods => "Search Horizon mods or paste an HTTPS .hmod link",
                Page::Shaders
                    if self.catalog.config.shader_source
                        == crate::shader_catalog::Origin::Modrinth =>
                {
                    "Modrinth shader ZIPs / experimental compatibility; some packs may not work"
                }
                Page::Shaders => "Horizon native .hshader packs",
                _ => "Verified downloads / enable in Installed",
            };
            let hint = translate(language, hint);
            let mut top = page_header(&mut ui, title, hint, margin, page_top, content_w);
            let kind = match page {
                Page::Mods => horizon_client::content_files::Kind::Mods,
                Page::Shaders => horizon_client::content_files::Kind::Shaders,
                _ => horizon_client::content_files::Kind::Packs,
            };
            if matches!(page, Page::Shaders | Page::Mods) {
                use crate::shader_catalog::Origin;
                let width = ((content_w - 8.0) / 2.0).min(180.0);
                for (i, origin) in [Origin::Horizon, Origin::Modrinth].into_iter().enumerate() {
                    let rect = [margin + i as f32 * (width + 8.0), top, width, 38.0];
                    let selected = (if page == Page::Mods {
                        self.catalog.config.mod_source
                    } else {
                        self.catalog.config.shader_source
                    }) == origin;
                    ui.rounded(
                        rect[0],
                        rect[1],
                        rect[2],
                        rect[3],
                        19.0,
                        if selected { color::TEXT } else { color::RAISED },
                    );
                    let label =
                        ui.fit_text(Face::Medium, 14.0, origin.source().label(), rect[2] - 24.0);
                    let text_w = ui.measure(Face::Medium, 14.0, &label);
                    ui.text(
                        Face::Medium,
                        14.0,
                        rect[0] + (rect[2] - text_w) / 2.0,
                        rect[1] + (rect[3] - 14.0 * 1.45) / 2.0,
                        if selected { color::BG } else { color::TEXT },
                        &label,
                    );
                    self.targets.push((
                        rect,
                        if page == Page::Mods {
                            Action::ModSource(origin)
                        } else {
                            Action::ShaderSource(origin)
                        },
                    ));
                }
                top += 46.0;
            }
            let search_button_w = 104.0;
            let search_w = (content_w - search_button_w - 8.0).max(80.0);
            let search_label = if self.catalog.query.is_empty() {
                translate(
                    language,
                    if page == Page::Mods {
                        "Search / .hmod URL: "
                    } else {
                        "Search: "
                    },
                )
                .to_owned()
            } else {
                self.catalog.query.clone()
            };
            field(
                &mut ui,
                &mut self.targets,
                [margin, top, search_w, 44.0],
                &search_label,
                Action::Field("search"),
                self.catalog.editing == Some("search"),
            );
            button(
                &mut ui,
                &mut self.targets,
                [margin + search_w + 8.0, top, search_button_w, 44.0],
                if page == Page::Mods && self.catalog.query.trim().starts_with("https://") {
                    translate(language, "Import .hmod")
                } else {
                    translate(language, "Search")
                },
                Action::Search,
                true,
            );
            let top = top + 60.0;
            let bottom = content_bottom - 48.0;
            let row_height = 76.0;
            let maximum =
                (self.catalog.results.len() as f32 * row_height - (bottom - top)).max(0.0);
            self.catalog.scroll = self.catalog.scroll.clamp(0.0, maximum);
            self.list_view = Some([top, bottom, maximum]);
            if self.catalog.results.is_empty() {
                let text = ui.fit_text(
                    Face::Regular,
                    15.0,
                    translate(language, "No results. Try another query or import a file."),
                    content_w,
                );
                ui.text(
                    Face::Regular,
                    15.0,
                    margin,
                    (top + bottom) / 2.0,
                    color::MUTED,
                    &text,
                );
            }
            for (i, p) in self.catalog.results.iter().enumerate() {
                let y = top + i as f32 * row_height - self.catalog.scroll;
                let download = self.catalog.download_label(p);
                if download.is_none()
                    && !crate::catalog::source_only_mod(p)
                    && !(p.kind == crate::browser::Kind::Mod
                        && p.source == crate::browser::SourceId::Modrinth)
                {
                    self.list_targets
                        .push(([w - margin - 108.0, y, 108.0, 64.0], Action::Install(i)));
                }
                if y < top || y + 64.0 > bottom {
                    continue;
                }
                let title = ui.fit_text(Face::Medium, 17.0, &p.title, content_w - 120.0);
                let summary = ui.fit_text(Face::Regular, 13.0, &p.summary, content_w - 120.0);
                ui.text(Face::Medium, 17.0, margin, y + 4.0, color::TEXT, &title);
                ui.text(
                    Face::Regular,
                    13.0,
                    margin,
                    y + 31.0,
                    color::MUTED,
                    &summary,
                );
                rule(&mut ui, margin, y + row_height - 8.0, content_w);
                if let Some(state) = download {
                    let state = if state == "Installed" {
                        translate(language, "Installed").to_owned()
                    } else {
                        state
                    };
                    ui.text(
                        Face::Medium,
                        14.0,
                        w - margin - 108.0,
                        y + 12.0,
                        color::MUTED,
                        &state,
                    );
                } else if p.kind == crate::browser::Kind::Mod
                    && p.source == crate::browser::SourceId::Modrinth
                {
                    ui.text(
                        Face::Regular,
                        13.0,
                        w - margin - 108.0,
                        y + 12.0,
                        color::MUTED,
                        translate(language, "Java mod"),
                    );
                } else if crate::catalog::source_only_mod(p) {
                    ui.text(
                        Face::Regular,
                        13.0,
                        w - margin - 108.0,
                        y + 12.0,
                        color::MUTED,
                        translate(language, ".hnmod: later"),
                    );
                } else if p.kind == crate::browser::Kind::Mod && !cfg!(target_pointer_width = "64")
                {
                    ui.text(
                        Face::Regular,
                        13.0,
                        w - margin - 108.0,
                        y + 12.0,
                        color::MUTED,
                        translate(language, "64-bit required"),
                    );
                } else {
                    button(
                        &mut ui,
                        &mut self.targets,
                        [w - margin - 108.0, y + 10.0, 108.0, 38.0],
                        translate(language, self.catalog.install_button(p)),
                        Action::Install(i),
                        false,
                    );
                }
            }
            let action_y = content_bottom - 40.0;
            let action_gap = 6.0;
            let action_w = if compact {
                (content_w - 3.0 * action_gap) / 4.0
            } else {
                110.0
            };
            button(
                &mut ui,
                &mut self.targets,
                [margin, action_y, action_w, 36.0],
                translate(language, "Previous"),
                Action::Previous,
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [margin + action_w + action_gap, action_y, action_w, 36.0],
                translate(language, "Next"),
                Action::Next,
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [
                    w - margin - 2.0 * action_w - action_gap,
                    action_y,
                    action_w,
                    36.0,
                ],
                translate(language, "Folder"),
                Action::OpenFolder(kind),
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [w - margin - action_w, action_y, action_w, 36.0],
                translate(language, "Import"),
                Action::Import(kind),
                false,
            );
        } else if page == Page::Installed {
            use horizon_client::content_files::Kind;
            let title = translate(language, "Installed");
            let caption = translate(
                language,
                "Mods, resource packs and shaders for this profile",
            );
            let header_h = page_header_height(content_w);
            let bottom = content_bottom;
            let viewport_top = nav_bottom + 8.0;
            let narrow = content_w < 340.0;
            let section_h = 52.0;
            let item_h = if narrow { 100.0 } else { 68.0 };
            let entries_total = Kind::ALL
                .iter()
                .enumerate()
                .map(|(i, kind)| {
                    section_h
                        + if self.catalog.sections[i] {
                            self.catalog
                                .local
                                .iter()
                                .filter(|entry| entry.kind == *kind)
                                .count() as f32
                                * item_h
                        } else {
                            0.0
                        }
                })
                .sum::<f32>();
            let total = header_h + entries_total;
            let maximum = (total - (bottom - viewport_top)).max(0.0);
            self.catalog.scroll = self.catalog.scroll.clamp(0.0, maximum);
            self.list_view = Some([viewport_top, bottom, maximum]);
            let content_top = page_header(
                &mut ui,
                title,
                caption,
                margin,
                page_top - self.catalog.scroll,
                content_w,
            );
            let mut offset = 0.0;
            for (section, kind) in Kind::ALL.into_iter().enumerate() {
                let count = self.catalog.local.iter().filter(|e| e.kind == kind).count();
                let y = content_top + offset;
                let label_w = (content_w - 88.0).max(80.0);
                let import_rect = [w - margin - 80.0, y, 80.0, 40.0];
                self.list_targets
                    .push(([margin, y, label_w, 44.0], Action::ToggleSection(section)));
                self.list_targets.push((import_rect, Action::Import(kind)));
                if y >= viewport_top && y + 44.0 <= bottom {
                    ui.rounded(margin, y, label_w, 44.0, 22.0, color::RAISED);
                    let label = format!(
                        "{}  {} ({count})",
                        if self.catalog.sections[section] {
                            "-"
                        } else {
                            "+"
                        },
                        translate(language, kind.label())
                    );
                    let label = ui.fit_text(Face::Medium, 15.0, &label, label_w - 28.0);
                    ui.text(
                        Face::Medium,
                        15.0,
                        margin + 14.0,
                        y + (44.0 - 15.0 * 1.45) / 2.0,
                        color::TEXT,
                        &label,
                    );
                    button(
                        &mut ui,
                        &mut self.targets,
                        import_rect,
                        translate(language, "Import"),
                        Action::Import(kind),
                        false,
                    );
                }
                offset += section_h;
                if !self.catalog.sections[section] {
                    continue;
                }
                for (i, item) in self
                    .catalog
                    .local
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.kind == kind)
                {
                    let y = content_top + offset;
                    offset += item_h;
                    let switch_x = if narrow {
                        margin
                    } else {
                        w - margin - 80.0 - 8.0 - 44.0
                    };
                    let remove_x = w - margin - 80.0;
                    let controls_y = if narrow { y + 52.0 } else { y + 14.0 };
                    self.list_targets
                        .push(([switch_x, controls_y, 44.0, 40.0], Action::ToggleLocal(i)));
                    self.list_targets
                        .push(([remove_x, controls_y, 80.0, 36.0], Action::DeleteLocal(i)));
                    if y < viewport_top || y + item_h > bottom {
                        continue;
                    }
                    let title_w = if narrow {
                        content_w
                    } else {
                        (content_w - 148.0).max(80.0)
                    };
                    let title = ui.fit_text(Face::Medium, 16.0, &item.title, title_w);
                    ui.text(Face::Medium, 16.0, margin, y + 4.0, color::TEXT, &title);
                    ui.text(
                        Face::Regular,
                        13.0,
                        margin,
                        y + 29.0,
                        color::MUTED,
                        translate(language, kind.label()),
                    );
                    if kind == Kind::Mods && !cfg!(target_pointer_width = "64") {
                        ui.text(
                            Face::Regular,
                            13.0,
                            switch_x,
                            controls_y + 8.0,
                            color::MUTED,
                            translate(language, "64-bit required"),
                        );
                    } else {
                        toggle(
                            &mut ui,
                            &mut self.targets,
                            [switch_x, controls_y, 44.0, 40.0],
                            item.enabled,
                            Action::ToggleLocal(i),
                            language,
                        );
                    }
                    button(
                        &mut ui,
                        &mut self.targets,
                        [remove_x, controls_y, 80.0, 36.0],
                        translate(language, "Remove"),
                        Action::DeleteLocal(i),
                        false,
                    );
                    rule(&mut ui, margin, y + item_h - 6.0, content_w);
                }
            }
            if self.catalog.local.is_empty() {
                let text = translate(language, "No items installed yet");
                let text_w = ui.measure(Face::Regular, 15.0, text);
                let y = content_top + 3.0 * section_h + 12.0;
                if y >= viewport_top && y + 20.0 <= bottom {
                    ui.text(
                        Face::Regular,
                        15.0,
                        margin + (content_w - text_w) / 2.0,
                        y,
                        color::MUTED,
                        text,
                    );
                }
            }
        } else if page == Page::Journal {
            // What the device recorded: imports (and why one was refused), errors and
            // exits. Scrolls as one column, newest last, so a failed .hmod can be read
            // on the phone itself.
            let title = translate(language, "Journal");
            let caption = translate(language, "Imports, errors and exits on this device");
            let header_h = page_header_height(content_w);
            let bottom = content_bottom;
            let viewport_top = nav_bottom + 8.0;
            let line_h = 18.0;
            let lines = crate::diagnostics::recent_lines(&self.home, 200);
            let mut top = page_header(&mut ui, title, &caption, margin, page_top, content_w);
            if lines.is_empty() {
                ui.text(
                    Face::Regular,
                    14.0,
                    margin,
                    top + 6.0,
                    color::MUTED,
                    translate(language, "Nothing recorded yet."),
                );
            } else {
                let total = lines.len() as f32 * line_h;
                let maximum = (total - (bottom - viewport_top)).max(0.0);
                self.catalog.scroll = self.catalog.scroll.clamp(0.0, maximum);
                self.list_view = Some([viewport_top, bottom, maximum]);
                let start = if maximum > 0.0 {
                    (bottom - viewport_top - self.catalog.scroll) / line_h
                } else {
                    0.0
                };
                let first = lines.len().saturating_sub(start as usize + 1);
                for (i, line) in lines.into_iter().enumerate().skip(first) {
                    let y = viewport_top + (i as f32 - first as f32) * line_h;
                    if y < viewport_top - line_h || y > bottom {
                        continue;
                    }
                    let text = ui.fit_text(Face::Regular, 12.0, &line, content_w);
                    let failing = line.contains("import") && line.contains("failed");
                    ui.text(
                        Face::Regular,
                        12.0,
                        margin,
                        y + top - viewport_top + 4.0,
                        if failing { color::DANGER } else { color::MUTED },
                        &text,
                    );
                }
            }
        } else {
            let c = &self.catalog.config;
            let language = c.language;
            let title = translate(language, "Settings");
            let caption = translate(language, "Display, input and performance");
            let header_h = page_header_height(content_w);

            enum Entry {
                Section(&'static str),
                Row(&'static str, Action, Kind),
            }
            enum Kind {
                Toggle(bool),
                Choice(String),
                Action(String),
            }
            let refresh = if c.device.display_hz == 0 {
                translate(language, "System default").to_owned()
            } else {
                tr!(language, "{hz} Hz (request)", { hz = c.device.display_hz })
            };
            let entries = vec![
                Entry::Section("Display"),
                Entry::Row(
                    "Language",
                    Action::Language,
                    Kind::Choice(c.language.label().to_owned()),
                ),
                Entry::Row(
                    "Rendering",
                    Action::Renderer,
                    Kind::Choice(translate(language, c.renderer.label()).to_owned()),
                ),
                Entry::Row(
                    "Client resolution",
                    Action::RenderScale,
                    Kind::Choice(if self.catalog.editing == Some("render_scale") {
                        self.catalog.render_scale_text.clone()
                    } else {
                        format!("{}%", c.render_scale.percent())
                    }),
                ),
                Entry::Row(
                    "Profile",
                    Action::Profile,
                    Kind::Choice(tr!(language, "Profile {id}", { id = c.profile })),
                ),
                Entry::Row(
                    "Orientation",
                    Action::Orientation,
                    Kind::Choice(translate(language, c.orientation.label()).to_owned()),
                ),
                Entry::Row(
                    "Input",
                    Action::InputHost,
                    Kind::Choice(translate(language, c.input_host.label()).to_owned()),
                ),
                Entry::Row(
                    "Immersive fullscreen",
                    Action::Immersive,
                    Kind::Toggle(c.screen.immersive),
                ),
                Entry::Row(
                    "Avoid camera cutout",
                    Action::Cutout,
                    Kind::Toggle(c.screen.avoid_cutout),
                ),
                Entry::Row(
                    "Aspect ratio",
                    Action::Aspect,
                    Kind::Choice(translate(language, c.screen.aspect.label()).to_owned()),
                ),
                Entry::Row(
                    "Display refresh rate",
                    Action::DisplayHz,
                    Kind::Choice(refresh),
                ),
                Entry::Section("Performance"),
                Entry::Row(
                    "Performance hints",
                    Action::PerformanceHints,
                    Kind::Toggle(c.device.performance_hints),
                ),
                Entry::Row(
                    "GPU profiling",
                    Action::GpuProfile,
                    Kind::Toggle(c.device.gpu_profile),
                ),
                Entry::Row(
                    "Performance recording",
                    Action::RecordPerformance,
                    Kind::Toggle(c.record_performance),
                ),
                Entry::Section("Network"),
                Entry::Row(
                    "Server",
                    Action::Field("server"),
                    Kind::Choice(if c.server.is_empty() {
                        translate(language, "Singleplayer / tap to enter server").to_owned()
                    } else {
                        c.server.clone()
                    }),
                ),
                Entry::Section("Support"),
                Entry::Row(
                    "Journal",
                    Action::Logs,
                    Kind::Action(translate(language, "Recent errors and imports").to_owned()),
                ),
                Entry::Row(
                    "Diagnostic report",
                    Action::ExportDiagnostics,
                    Kind::Action(
                        translate(language, "Export ZIP / review before sharing").to_owned(),
                    ),
                ),
                Entry::Row(
                    "Performance report",
                    Action::ExportPerformance,
                    Kind::Action(translate(language, "Open in Files / Reports").to_owned()),
                ),
            ];
            let row_h = if compact { 56.0 } else { 54.0 };
            let section_h = 30.0;
            let entries_total = entries
                .iter()
                .map(|entry| match entry {
                    Entry::Section(_) => section_h,
                    Entry::Row(..) => row_h,
                })
                .sum::<f32>();
            let viewport_top = nav_bottom + 8.0;
            let action_y = content_bottom - 44.0;
            let bottom = action_y - 12.0;
            let total = header_h + entries_total;
            let maximum = (total - (bottom - viewport_top)).max(0.0);
            self.catalog.scroll = self.catalog.scroll.clamp(0.0, maximum);
            self.list_view = Some([viewport_top, bottom, maximum]);
            let mut y = page_header(
                &mut ui,
                title,
                caption,
                margin,
                page_top - self.catalog.scroll,
                content_w,
            );
            for entry in entries {
                match entry {
                    Entry::Section(name) => {
                        let name = translate(language, name);
                        if y >= viewport_top && y + section_h <= bottom {
                            ui.text(Face::Medium, 12.0, margin, y + 8.0, color::MUTED, name);
                            rule(&mut ui, margin, y + section_h - 8.0, content_w);
                        }
                        y += section_h;
                    }
                    Entry::Row(name, action, kind) => {
                        let rect = [margin, y, content_w, row_h];
                        self.list_targets.push((rect, action));
                        if y >= viewport_top && y + row_h <= bottom {
                            let name = translate(language, name);
                            let choice = matches!(kind, Kind::Choice(_));
                            match kind {
                                Kind::Toggle(on) => {
                                    let state = translate(language, if on { "On" } else { "Off" });
                                    let state_w = ui.measure(Face::Regular, 13.0, state);
                                    let title_w = (content_w - state_w - 64.0).max(80.0);
                                    let title = ui.fit_text(Face::Medium, 15.0, name, title_w);
                                    ui.text(
                                        Face::Medium,
                                        15.0,
                                        margin,
                                        y + (row_h - 15.0 * 1.45) / 2.0,
                                        color::TEXT,
                                        &title,
                                    );
                                    ui.text(
                                        Face::Regular,
                                        13.0,
                                        w - margin - 52.0 - state_w,
                                        y + (row_h - 13.0 * 1.45) / 2.0,
                                        color::MUTED,
                                        state,
                                    );
                                    toggle(
                                        &mut ui,
                                        &mut self.targets,
                                        [w - margin - 44.0, y + (row_h - 40.0) / 2.0, 44.0, 40.0],
                                        on,
                                        action,
                                        language,
                                    );
                                }
                                Kind::Choice(value) | Kind::Action(value) => {
                                    let ends_with_chevron = choice;
                                    let right_pad = if ends_with_chevron { 18.0 } else { 0.0 };
                                    let value = ui.fit_text(
                                        Face::Regular,
                                        14.0,
                                        &value,
                                        (content_w * 0.52).max(80.0),
                                    );
                                    let value_w = ui.measure(Face::Regular, 14.0, &value);
                                    let title_w =
                                        (content_w - value_w - right_pad - 24.0).max(80.0);
                                    let title = ui.fit_text(Face::Medium, 15.0, name, title_w);
                                    ui.text(
                                        Face::Medium,
                                        15.0,
                                        margin,
                                        y + (row_h - 15.0 * 1.45) / 2.0,
                                        color::TEXT,
                                        &title,
                                    );
                                    ui.text(
                                        Face::Regular,
                                        14.0,
                                        w - margin - right_pad - value_w,
                                        y + (row_h - 14.0 * 1.45) / 2.0,
                                        color::MUTED,
                                        &value,
                                    );
                                    if ends_with_chevron {
                                        ui.text(
                                            Face::Medium,
                                            18.0,
                                            w - margin - 10.0,
                                            y + (row_h - 18.0 * 1.45) / 2.0,
                                            color::MUTED,
                                            ">",
                                        );
                                    }
                                    self.targets.push((rect, action));
                                }
                            }
                            rule(&mut ui, margin, y + row_h - 1.0, content_w);
                        }
                        y += row_h;
                    }
                }
            }
            let half = (content_w - 8.0) / 2.0;
            button(
                &mut ui,
                &mut self.targets,
                [margin, action_y, half, 44.0],
                translate(language, "Diagnostics"),
                Action::Logs,
                false,
            );
            button(
                &mut ui,
                &mut self.targets,
                [margin + half + 8.0, action_y, half, 44.0],
                translate(language, "Microsoft / Xbox account"),
                Action::Account,
                true,
            );
        }
        let account_label = self.login.status(&self.catalog.config.name, language);
        Self::draw_chrome(
            &mut ui,
            &mut self.targets,
            &account_label,
            language,
            compact,
            margin,
            content_w,
            nav_bottom,
            page,
        );
        ui.rounded(0.0, h - 64.0, w, 64.0, 0.0, color::BG);
        rule(&mut ui, margin, h - 64.0, content_w);
        let bar_y = h - 52.0;
        rule(&mut ui, margin, bar_y, content_w);
        let status = self.error.as_deref().unwrap_or(&self.catalog.status);
        let status = ui.fit_text(Face::Regular, 12.0, status, content_w - 92.0);
        ui.text(
            Face::Regular,
            12.0,
            margin,
            bar_y + 13.0,
            if self.error.is_some() {
                color::DANGER
            } else {
                color::MUTED
            },
            &status,
        );
        button(
            &mut ui,
            &mut self.targets,
            [w - margin - 72.0, bar_y + 9.0, 72.0, 30.0],
            "GitHub",
            Action::Repository,
            false,
        );
        if self.login.visible {
            self.list_targets.clear();
            self.list_view = None;
            self.login.draw(
                &mut ui,
                &mut self.targets,
                &self.catalog.config.name,
                self.catalog.editing == Some("name"),
                language,
            );
        }
        if let Some(chooser) = &mut self.chooser {
            chooser.draw(&mut ui, &mut self.targets);
            self.list_targets = chooser.navigation(ui.w);
            self.list_view = Some(chooser.viewport(ui.h));
        }
        if let Some([top, bottom, maximum]) = self.list_view
            && maximum > 0.0
            && bottom > top
        {
            let scroll = self
                .chooser
                .as_ref()
                .map_or(self.catalog.scroll, |c| c.scroll);
            let height = bottom - top;
            let thumb = (height * height / (height + maximum)).max(20.0).min(height);
            let y = top + (height - thumb) * (scroll / maximum).clamp(0.0, 1.0);
            ui.rounded(w - 12.0, top, 4.0, height, 2.0, color::BORDER);
            ui.rounded(w - 12.0, y, 4.0, thumb, 2.0, color::MUTED);
        }
        let caret_visible = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .is_ok_and(|t| t.as_millis() % 1000 < 500);
        if caret_visible && let Some(name) = self.catalog.editing {
            let value = match name {
                "name" => self.catalog.config.name.clone(),
                "server" => self.catalog.config.server.clone(),
                "render_scale" => self.catalog.render_scale_text.clone(),
                _ => self.catalog.query.clone(),
            };
            for ([x, y, w, _], action) in &self.targets {
                if *action == Action::Field(name) {
                    let text = ui.fit_text(Face::Medium, 15.0, &value, w - 24.0);
                    let caret_x = x + 12.0 + ui.measure(Face::Medium, 15.0, &text);
                    ui.rounded(
                        caret_x.min(x + w - 10.0),
                        y + 12.0,
                        1.0,
                        18.0,
                        0.0,
                        color::TEXT,
                    );
                }
            }
        }
        if let Some(action) = self.pending_focus.take() {
            self.focus = self.targets.iter().position(|(_, a)| *a == action);
        }
        if let Some(index) = self.focus
            && let Some(([x, y, w, h], _)) = self.targets.get(index)
        {
            ui.border(*x - 2.0, *y - 2.0, *w + 4.0, *h + 4.0, 2.0, color::TEXT);
            self.cursor = (
                f64::from((x + w / 2.0) * ui.density),
                f64::from((y + h / 2.0) * ui.density),
            );
        }
        if self.mouse_down || self.drag.as_ref().is_some_and(|drag| !drag.moved) {
            let point = [
                self.cursor.0 as f32 / ui.density,
                self.cursor.1 as f32 / ui.density,
            ];
            if let Some(([x, y, w, h], _)) = self.targets.iter().rev().find(|(rect, _)| {
                point[0] >= rect[0]
                    && point[0] <= rect[0] + rect[2]
                    && point[1] >= rect[1]
                    && point[1] <= rect[1] + rect[3]
            }) {
                ui.rounded(
                    *x,
                    *y,
                    *w,
                    *h,
                    2.0,
                    horizon_client::hud::rgba(255, 255, 255, 48),
                );
            }
        }
        if !self.mouse_down
            && self.drag.is_none()
            && let Some((action, time)) = self.feedback
            && time.elapsed() < std::time::Duration::from_millis(160)
            && let Some(([x, y, w, h], _)) = self.targets.iter().find(|(_, a)| *a == action)
        {
            ui.rounded(
                *x,
                *y,
                *w,
                *h,
                2.0,
                horizon_client::hud::rgba(255, 255, 255, 48),
            );
        }
        crate::documents::draw_ui(&mut ui);
        ui.finish();

        if !r.render(&game::loading_frame(&b))? && !b.patches.is_empty() {
            // A suspended/lost surface skips the frame, including its glyph
            // uploads. Rebuild them next time instead of caching invisible text.
            horizon_client::ui_theme::reset_atlas();
        }
        Ok(())
    }
    pub(super) fn activate(&mut self, events: &ActiveEventLoop) -> Result<()> {
        if self.session_worker.is_some() {
            return Ok(());
        }
        let Some(r) = &self.renderer else {
            return Ok(());
        };
        let mut b = HudBatch::new([r.config.width, r.config.height]);
        let ui = Ui::mobile(&self.gui.as_ref().unwrap().atlas, &mut b, (0.0, 0.0));
        let p = self.focus.and_then(|index| self.targets.get(index)).map_or(
            [
                self.cursor.0 as f32 / ui.density,
                self.cursor.1 as f32 / ui.density,
            ],
            |([x, y, w, h], _)| [x + w / 2.0, y + h / 2.0],
        );
        let action = self
            .targets
            .iter()
            .find(|([x, y, w, h], _)| {
                (*x..*x + *w).contains(&p[0]) && (*y..*y + *h).contains(&p[1])
            })
            .map(|(_, a)| *a);
        if self.catalog.busy() || crate::documents::busy() {
            return Ok(());
        }
        self.feedback = action.map(|action| (action, std::time::Instant::now()));
        self.focus = None;
        let language = self.catalog.config.language;
        match action {
            Some(Action::Immersive) => {
                self.catalog.config.screen.immersive ^= true;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
            }
            Some(Action::Cutout) => {
                self.catalog.config.screen.avoid_cutout ^= true;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
            }
            Some(Action::Aspect) => self.open_chooser(chooser::Kind::Aspect),
            Some(Action::ChooseAspect(aspect)) => {
                self.catalog.config.screen.aspect = aspect;
                self.catalog.config.save(&self.home)?;
                self.configure_screen();
                self.chooser = None;
            }
            Some(Action::DisplayHz) => self.open_chooser(chooser::Kind::DisplayHz),
            Some(Action::ChooseDisplayHz(hz)) => {
                self.catalog.config.device.display_hz = hz;
                self.catalog.config.device.sanitize();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
                self.chooser = None;
            }
            Some(Action::PerformanceHints) => {
                self.catalog.config.device.performance_hints ^= true;
                self.catalog.status = translate(
                    language,
                    "ADPF gives Android CPU frame-work hints on supported devices. It does not cap FPS or guarantee a speedup.",
                )
                .to_owned();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
            }
            Some(Action::GpuProfile) => {
                self.catalog.config.device.gpu_profile ^= true;
                self.catalog.status = translate(
                    language,
                    "Detailed GPU profiling records diagnostics and has overhead. Reports remain on this device.",
                )
                .to_owned();
                self.catalog.config.save(&self.home)?;
                crate::device_runtime::configure(self.catalog.config.device);
            }

            Some(Action::ModSource(origin)) => {
                self.catalog.config.mod_source = origin;
                self.catalog.config.save(&self.home)?;
                self.catalog.offset = 0;
                self.catalog.browse(&self.home);
            }
            Some(Action::ShaderSource(origin)) => {
                self.catalog.config.shader_source = origin;
                self.catalog.config.save(&self.home)?;
                self.catalog.offset = 0;
                self.catalog.browse(&self.home);
            }
            Some(Action::OpenFolder(kind)) => {
                crate::platform::hide();
                let folder = match kind {
                    horizon_client::content_files::Kind::Mods => "mods",
                    horizon_client::content_files::Kind::Packs => "packs",
                    horizon_client::content_files::Kind::Shaders => "shaders",
                };
                let id = crate::document_store::folder_id(self.catalog.config.profile, folder);
                self.catalog.status = if crate::platform::open_documents(&id) {
                    translate(
                        language,
                        "Opened in Files. Return here after copying files.",
                    )
                    .to_owned()
                } else {
                    translate(
                        language,
                        "Open the system Files app and choose Horizon Android.",
                    )
                    .to_owned()
                };
            }
            Some(Action::Import(kind)) => {
                crate::platform::hide();
                let home = self.catalog.config.game_home(&self.home);
                if let Err(e) = crate::documents::begin(kind, home) {
                    self.catalog.status = e;
                }
            }
            Some(Action::ToggleSection(i)) => {
                self.catalog.sections[i] = !self.catalog.sections[i];
                self.catalog.scroll = 0.0;
            }
            Some(Action::ToggleLocal(i)) => self.catalog.toggle_local(&self.home, i),
            Some(Action::DeleteLocal(i)) => {
                self.catalog.pending_delete = self.catalog.local.get(i).cloned();
                if let Some(item) = &self.catalog.pending_delete {
                    self.chooser = Some(super::chooser::Chooser::confirm_delete(
                        item.file.clone(),
                        self.catalog.config.language,
                    ));
                }
            }
            Some(Action::ConfirmDeleteLocal) => {
                self.catalog.remove_local(&self.home);
                self.chooser = None;
            }
            Some(Action::Repository) => {
                self.catalog.status = if crate::platform::open_repository() {
                    translate(language, "Repository opened in browser.").to_owned()
                } else {
                    translate(language, "No browser available to open the repository.").to_owned()
                };
            }
            Some(Action::Account) => {
                self.login.visible = true;
            }
            Some(Action::CloseLogin) => {
                self.login.hide();
                crate::platform::hide();
                self.catalog.editing = None;
            }
            Some(Action::DeviceSignIn) => self
                .login
                .start_device(&self.catalog.config.microsoft_client_id),
            Some(Action::CopyLoginCode) => self.login.copy_code(),
            Some(Action::CancelLogin) => {
                self.login.close();
                self.login.visible = true;
            }
            Some(Action::ReviewApp) => self.login.review_app(),
            Some(Action::SignIn) => self.login.start(&self.catalog.config.microsoft_client_id),
            Some(Action::SignOut) => self.login.sign_out(),
            Some(Action::LoginLink) => self.login.open_link(),
            Some(Action::Page(page)) => {
                self.catalog.editing = None;
                if crate::platform::active() {
                    crate::platform::hide();
                }
                self.catalog.page = page;
                self.catalog.scroll = 0.0;
                self.error = None;
                self.catalog.offset = 0;
                self.catalog.browse(&self.home);
            }
            Some(Action::Play) => self.action(events)?,
            Some(Action::Search) => {
                crate::platform::hide();
                self.catalog.offset = 0;
                self.catalog.search(&self.home);
            }
            Some(Action::FinishNameEdit) => {
                self.catalog.editing = None;
                crate::platform::hide();
                self.catalog.config.save(&self.home)?;
            }
            Some(Action::Field(name)) => {
                if name == "render_scale" {
                    self.catalog.render_scale_text =
                        self.catalog.config.render_scale.percent().to_string();
                    self.chooser = None;
                }
                self.catalog.editing = Some(name);
                let initial = match name {
                    "name" => &self.catalog.config.name,
                    "server" => &self.catalog.config.server,
                    "render_scale" => &self.catalog.render_scale_text,
                    "client_id" => &self.catalog.config.microsoft_client_id,
                    _ => &self.catalog.query,
                };
                crate::platform::show(initial);
            }
            Some(Action::Install(i)) => self.catalog.install(&self.home, i),
            Some(Action::Next) => {
                self.catalog.offset += 12;
                self.catalog.search(&self.home);
            }
            Some(Action::Previous) => {
                self.catalog.offset = self.catalog.offset.saturating_sub(12);
                self.catalog.search(&self.home);
            }
            Some(Action::DownloadUpdate) => self.update.start_download(&self.home),
            Some(Action::RetryInstall) => self.update.retry_install(),
            Some(Action::Language) => self.open_chooser(chooser::Kind::Language),
            Some(Action::ChooseLanguage(language)) => {
                self.catalog.config.language = language;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
            }
            Some(Action::Profile) => self.open_chooser(chooser::Kind::Profile),
            Some(Action::Renderer) => self.open_chooser(chooser::Kind::Renderer),
            Some(Action::RenderScale) => self.open_chooser(chooser::Kind::RenderScale),
            Some(Action::ChooseRenderScale(scale)) => {
                self.catalog.config.render_scale = scale;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::display::set_scale(scale);
            }
            Some(Action::Orientation) => self.open_chooser(chooser::Kind::Orientation),
            Some(Action::InputHost) => self.open_chooser(chooser::Kind::InputHost),
            Some(Action::ChooseInputHost(mode)) => {
                self.catalog.config.input_host = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::configure_input_mode(mode);
            }
            Some(Action::ChooseOrientation(mode)) => {
                self.catalog.config.orientation = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
                crate::platform::set_orientation(mode);
            }
            Some(Action::ChooseRenderer(mode)) => {
                self.catalog.config.renderer = mode;
                self.catalog.config.save(&self.home)?;
                self.chooser = None;
            }
            Some(Action::ChooseProfile(profile)) => {
                std::fs::create_dir_all(if profile == 0 {
                    self.home.clone()
                } else {
                    self.home
                        .join("profiles")
                        .join(format!("profile-{profile}"))
                })?;
                self.catalog.config.profile = profile;
                self.catalog.config.save(&self.home)?;
                self.catalog.refresh(&self.home);
                self.chooser = None;
            }
            Some(Action::Dismiss) => self.chooser = None,
            Some(Action::Logs) => {
                // The journal: what failed and what was imported, newest last, so a
                // refused mod can be read on the phone without exporting anything.
                let lines = crate::diagnostics::recent_lines(&self.home, 40);
                let summary = crate::diagnostics::summary(&self.home);
                self.catalog.status = if lines.is_empty() {
                    summary
                } else {
                    format!("{summary}\n\n{}", lines.join("\n"))
                };
            }
            Some(Action::ExportDiagnostics) if self.diagnostic_export.is_none() => {
                let home = self.home.clone();
                let (sender, receiver) = mpsc::channel();
                self.diagnostic_export = Some(receiver);
                self.catalog.status = translate(
                    language,
                    "Preparing local diagnostic ZIP. No upload; review before sharing.",
                )
                .to_owned();
                std::thread::spawn(move || {
                    let result = crate::diagnostics::export(&home).map(|p| {
                        p.file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned()
                    });
                    let _ = sender.send(result);
                });
            }
            Some(Action::ExportDiagnostics) => {}
            Some(Action::RecordPerformance) => {
                self.catalog.config.record_performance = !self.catalog.config.record_performance;
                if !self.catalog.config.record_performance {
                    crate::performance::stop();
                }
                self.catalog.config.save(&self.home)?;
                self.catalog.status = if self.catalog.config.record_performance {
                    translate(
                        language,
                        "Recording starts on Play. Detailed timings add overhead. Return here to save the report.",
                    )
                    .to_owned()
                } else {
                    translate(language, "Performance recording off.").to_owned()
                };
            }
            Some(Action::ExportPerformance) => {
                self.catalog.status = match crate::performance::export(&self.home) {
                    Ok(()) => translate(
                        language,
                        "Reports opened in Files. Copy or share a report from there.",
                    )
                    .to_owned(),
                    Err(e) => e,
                };
            }
            None => {}
        }
        self.redraw();
        Ok(())
    }
    pub(super) fn ui_density(&self) -> f32 {
        let (Some(r), Some(gui)) = (&self.renderer, &self.gui) else {
            return 1.0;
        };
        let mut batch = HudBatch::new([r.config.width, r.config.height]);
        Ui::mobile(&gui.atlas, &mut batch, (0.0, 0.0)).density
    }

    pub(super) fn touch_event(&mut self, events: &ActiveEventLoop, t: Touch) -> Result<()> {
        self.focus = None;
        self.pending_focus = None;
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        self.cursor = (t.location.x, t.location.y);
        match t.phase {
            TouchPhase::Started => {
                self.drag = Some(super::gesture::Drag::new(t.location.x, t.location.y));
            }
            TouchPhase::Moved => {
                let density = self.ui_density() as f64;
                if let Some(drag) = &mut self.drag {
                    let delta = drag.update(t.location.x, t.location.y, density);
                    if drag.moved {
                        if let Some(chooser) = &mut self.chooser {
                            chooser.scroll = (chooser.scroll + delta).max(0.0);
                        } else if self
                            .list_view
                            .is_some_and(|view| drag.origin_y() / density >= view[0] as f64)
                        {
                            self.catalog.scroll = (self.catalog.scroll + delta).max(0.0);
                        }
                    }
                }
            }
            TouchPhase::Ended => {
                if self.drag.take().is_some_and(|drag| !drag.moved) {
                    self.activate(events)?;
                }
            }
            TouchPhase::Cancelled => {
                self.drag = None;
            }
        }
        Ok(())
    }
    pub(super) fn type_key(&mut self, key: KeyEvent) {
        if key.physical_key == PhysicalKey::Code(KeyCode::Escape)
            || key.logical_key
                == winit::keyboard::Key::Named(winit::keyboard::NamedKey::BrowserBack)
        {
            if self.login.visible {
                self.login.hide();
                crate::platform::hide();
                self.catalog.editing = None;
                return;
            }
            if self.chooser.take().is_some() {
                return;
            }
            crate::platform::hide();
            self.catalog.editing = None;
            let _ = self.catalog.config.save(&self.home);
            return;
        }
        if self.catalog.editing.is_none() {
            return;
        }
        let out = match key.physical_key {
            PhysicalKey::Code(KeyCode::Backspace) => Some(Output::Backspace),
            PhysicalKey::Code(KeyCode::Enter) => {
                crate::platform::hide();
                Some(Output::Enter)
            }
            _ => key.text.map(|s| Output::Text(s.into())),
        };
        if let Some(out) = out {
            self.catalog.edit(out, &self.home);
        }
    }
}
