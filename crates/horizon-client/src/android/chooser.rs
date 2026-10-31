//! Explicit, scrollable selections. Values never cycle on repeated button taps.
use super::{
    Launcher,
    pages::{Action, button, gutter, page_header, rule},
};
use crate::catalog::RendererMode;
use horizon_client::{
    launcher_i18n::{Language, tr as translate},
    tr,
    ui_theme::{Face, Ui, color},
};

const LIST_TOP: f32 = 128.0;
const ROW_HEIGHT: f32 = 68.0;

pub(super) enum Kind {
    Aspect,
    DisplayHz,
    Renderer,
    RenderScale,
    Profile,
    Orientation,
    InputHost,
    Language,
}
pub(super) struct Chooser {
    title: &'static str,
    help: &'static str,
    device: Option<String>,
    choices: Vec<(String, String, Action, bool)>,
    language: Language,
    pub scroll: f32,
}
impl Launcher {
    pub(super) fn open_chooser(&mut self, kind: Kind) {
        crate::platform::hide();
        self.catalog.editing = None;
        let device = if matches!(kind, Kind::Renderer) {
            self.renderer.as_ref().map(|r| {
                format!(
                    "GPU: {} / {} {}",
                    r.info.name, r.info.driver, r.info.driver_info
                )
            })
        } else {
            None
        };
        let c = &self.catalog.config;
        let (title, help, choices) = match kind {
            Kind::Language => (
                tr!(c.language, "Language"),
                tr!(
                    c.language,
                    "Applies to the launcher. The game has its own language setting."
                ),
                Language::ALL
                    .into_iter()
                    .map(|language| {
                        (
                            language.label().into(),
                            String::new(),
                            Action::ChooseLanguage(language),
                            c.language == language,
                        )
                    })
                    .collect(),
            ),
            Kind::Aspect => (
                tr!(c.language, "Aspect ratio"),
                tr!(
                    c.language,
                    "Fits the entire picture without stretching. Touch coordinates follow the picture. Portrait swaps width and height."
                ),
                [
                    crate::display::Aspect::Native,
                    crate::display::Aspect::Wide,
                    crate::display::Aspect::Classic,
                    crate::display::Aspect::Square,
                ]
                .into_iter()
                .map(|a| {
                    (
                        translate(c.language, a.label()).into(),
                        String::new(),
                        Action::ChooseAspect(a),
                        c.screen.aspect == a,
                    )
                })
                .collect(),
            ),
            Kind::DisplayHz => (
                tr!(c.language, "Display refresh rate"),
                tr!(
                    c.language,
                    "An Android display preference, not a game FPS cap. Unsupported rates may be ignored."
                ),
                crate::device_preferences::DISPLAY_RATES
                    .into_iter()
                    .map(|hz| {
                        (
                            if hz == 0 {
                                translate(c.language, "System default").into()
                            } else {
                                tr!(c.language, "{hz} Hz", { hz = hz })
                            },
                            String::new(),
                            Action::ChooseDisplayHz(hz),
                            c.device.display_hz == hz,
                        )
                    })
                    .collect(),
            ),
            Kind::RenderScale => {
                let mut choices: Vec<_> = crate::display::RenderScale::PRESETS
                    .into_iter()
                    .map(|percent| {
                        (
                            format!("{percent}%"),
                            if percent == 100 {
                                translate(c.language, "Native resolution").into()
                            } else {
                                translate(c.language, "Lower resolution for the whole picture")
                                    .into()
                            },
                            Action::ChooseRenderScale(crate::display::RenderScale::new(percent)),
                            c.render_scale.percent() == percent,
                        )
                    })
                    .collect();
                choices.push((
                    translate(c.language, "Custom percentage...").into(),
                    translate(c.language, "Enter a value from 25 to 100").into(),
                    Action::Field("render_scale"),
                    false,
                ));
                (
                    tr!(c.language, "Client resolution"),
                    tr!(c.language, "Small screens retain a usable menu size."),
                    choices,
                )
            }
            Kind::InputHost => (
                tr!(c.language, "Input mode"),
                tr!(
                    c.language,
                    "Mouse mode uses a physical keyboard. Touch mode keeps the Android keyboard."
                ),
                crate::catalog::InputHost::ALL
                    .into_iter()
                    .map(|mode| {
                        (
                            translate(c.language, mode.label()).into(),
                            String::new(),
                            Action::ChooseInputHost(mode),
                            c.input_host == mode,
                        )
                    })
                    .collect(),
            ),
            Kind::Orientation => (
                tr!(c.language, "Screen orientation"),
                tr!(
                    c.language,
                    "Applies to the launcher and game. Does not change touch bindings."
                ),
                crate::catalog::Orientation::ALL
                    .into_iter()
                    .map(|mode| {
                        (
                            translate(c.language, mode.label()).into(),
                            String::new(),
                            Action::ChooseOrientation(mode),
                            c.orientation == mode,
                        )
                    })
                    .collect(),
            ),
            Kind::Renderer => (
                tr!(c.language, "Rendering mode"),
                tr!(
                    c.language,
                    "Uses the system GPU driver. Applies on the next game launch."
                ),
                vec![
                    (
                        translate(c.language, "OpenGL ES / experimental").into(),
                        translate(
                            c.language,
                            "For older GPUs. Requires ES 3.0+ and sufficient driver limits.",
                        )
                        .into(),
                        Action::ChooseRenderer(RendererMode::OpenGl),
                        c.renderer == RendererMode::OpenGl,
                    ),
                    (
                        translate(c.language, "Vulkan / GPU batching").into(),
                        translate(
                            c.language,
                            "Groups terrain draws and enables supported GPU culling.",
                        )
                        .into(),
                        Action::ChooseRenderer(RendererMode::Vulkan),
                        c.renderer == RendererMode::Vulkan,
                    ),
                    (
                        translate(c.language, "Vulkan / compatibility").into(),
                        translate(
                            c.language,
                            "Direct terrain draws for drivers with missing or transparent blocks.",
                        )
                        .into(),
                        Action::ChooseRenderer(RendererMode::VulkanCompatibility),
                        c.renderer == RendererMode::VulkanCompatibility,
                    ),
                ],
            ),
            Kind::Profile => {
                let mut ids = vec![0, c.profile];
                ids.extend(
                    std::fs::read_dir(self.home.join("profiles"))
                        .into_iter()
                        .flatten()
                        .flatten()
                        .filter(|e| e.path().is_dir())
                        .filter_map(|e| {
                            e.file_name()
                                .to_str()?
                                .strip_prefix("profile-")?
                                .parse::<u32>()
                                .ok()
                        }),
                );
                ids.sort_unstable();
                ids.dedup();
                let next = ids.last().copied().unwrap_or(0).checked_add(1);
                let mut choices: Vec<_> = ids
                    .into_iter()
                    .map(|id| {
                        (
                            tr!(c.language, "Profile {id}", { id = id }),
                            if id == 0 {
                                translate(c.language, "Default profile").into()
                            } else {
                                translate(c.language, "Separate saves, mods and packs").into()
                            },
                            Action::ChooseProfile(id),
                            id == c.profile,
                        )
                    })
                    .collect();
                if let Some(id) = next {
                    choices.push((
                        translate(c.language, "Create profile").into(),
                        tr!(c.language, "Create Profile {id}", { id = id }),
                        Action::ChooseProfile(id),
                        false,
                    ));
                }
                (
                    tr!(c.language, "Choose profile"),
                    tr!(
                        c.language,
                        "Each profile keeps its own game settings and saves."
                    ),
                    choices,
                )
            }
        };
        self.chooser = Some(Chooser {
            title,
            help,
            choices,
            device,
            language: c.language,
            scroll: 0.0,
        });
    }
}
impl Chooser {
    pub(super) fn confirm_delete(name: String, language: Language) -> Self {
        Self {
            title: tr!(language, "Remove add-on?"),
            help: tr!(
                language,
                "Deletes only the selected file from this profile. This cannot be undone."
            ),
            device: Some(name),
            choices: vec![
                (
                    tr!(language, "Remove").into(),
                    String::new(),
                    Action::ConfirmDeleteLocal,
                    false,
                ),
                (
                    tr!(language, "Cancel").into(),
                    String::new(),
                    Action::Dismiss,
                    true,
                ),
            ],
            language,
            scroll: 0.0,
        }
    }
    pub(super) fn navigation(&self, width: f32) -> Vec<([f32; 4], Action)> {
        let margin = gutter(width);
        self.choices
            .iter()
            .enumerate()
            .map(|(i, (_, _, action, _))| {
                (
                    [
                        margin,
                        LIST_TOP + i as f32 * ROW_HEIGHT - self.scroll,
                        width - 2.0 * margin,
                        ROW_HEIGHT - 4.0,
                    ],
                    *action,
                )
            })
            .collect()
    }
    pub(super) fn viewport(&self, height: f32) -> [f32; 3] {
        [
            LIST_TOP,
            height - 76.0,
            (self.choices.len() as f32 * ROW_HEIGHT - (height - 76.0 - LIST_TOP)).max(0.0),
        ]
    }
    pub fn draw(&mut self, ui: &mut Ui, targets: &mut Vec<([f32; 4], Action)>) {
        let (w, h) = (ui.w, ui.h);
        let margin = gutter(w);
        targets.clear();
        ui.rounded(0.0, 0.0, w, h, 0.0, color::BG);
        let top = page_header(ui, self.title, self.help, margin, 24.0, w - 2.0 * margin);
        if let Some(device) = &self.device {
            ui.text(
                Face::Regular,
                12.0,
                margin,
                top - 10.0,
                color::MUTED,
                device,
            );
        }
        let top = LIST_TOP;
        let bottom = h - 76.0;
        self.scroll = self.scroll.clamp(
            0.0,
            (self.choices.len() as f32 * ROW_HEIGHT - (bottom - top)).max(0.0),
        );
        for (i, (label, detail, action, selected)) in self.choices.iter().enumerate() {
            let y = top + i as f32 * ROW_HEIGHT - self.scroll;
            if y < top - ROW_HEIGHT || y + ROW_HEIGHT - 4.0 > bottom {
                continue;
            }
            if *selected {
                ui.rounded(
                    margin,
                    y,
                    w - 2.0 * margin,
                    ROW_HEIGHT - 8.0,
                    8.0,
                    color::RAISED,
                );
            }
            let radio_y = y + (ROW_HEIGHT - 24.0) / 2.0;
            ui.rounded(margin + 2.0, radio_y, 20.0, 20.0, 10.0, color::TEXT);
            ui.rounded(margin + 4.0, radio_y + 2.0, 16.0, 16.0, 8.0, color::BG);
            if *selected {
                ui.rounded(margin + 8.0, radio_y + 6.0, 8.0, 8.0, 4.0, color::TEXT);
            }
            let label = ui.fit_text(Face::Medium, 16.0, label, w - 2.0 * margin - 48.0);
            ui.text(
                Face::Medium,
                16.0,
                margin + 32.0,
                y + (if detail.is_empty() { 14.0 } else { 8.0 }),
                color::TEXT,
                &label,
            );
            if !detail.is_empty() {
                let detail = ui.fit_text(Face::Regular, 13.0, detail, w - 2.0 * margin - 48.0);
                ui.text(
                    Face::Regular,
                    13.0,
                    margin + 32.0,
                    y + 31.0,
                    color::MUTED,
                    &detail,
                );
            }
            rule(ui, margin, y + ROW_HEIGHT - 5.0, w - 2.0 * margin);
            targets.push(([margin, y, w - 2.0 * margin, ROW_HEIGHT - 4.0], *action));
        }
        button(
            ui,
            targets,
            [margin, h - 62.0, 140.0, 42.0],
            tr!(self.language, "Cancel"),
            Action::Dismiss,
            false,
        );
    }
}
