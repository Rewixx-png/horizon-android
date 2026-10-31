//! Native account panel; authentication is shared with the desktop launcher.
use super::md3;
use super::md3 as color;
use super::pages::{Action, button, field};
use crate::account::{self, Account, Accounts, Secrets, SignIn, SignInState};
use horizon_client::{
    launcher_i18n::{Language, tr as translate},
    tr,
    ui_theme::{Face, Ui},
};

pub(super) struct Login {
    pub visible: bool,
    pub signed_in: bool,
    pub profile: Option<String>,
    task: Option<SignIn>,
    auto_open_device: bool,
    state: Option<SignInState>,
    message: String,
    language: Language,
}
impl Login {
    pub fn new() -> Self {
        let mut login = Self {
            visible: false,
            signed_in: false,
            profile: None,
            task: None,
            auto_open_device: false,
            state: None,
            message: String::new(),
            language: Language::English,
        };
        login.refresh_status();
        login
    }
    pub(super) fn refresh_status(&mut self) {
        let account = Accounts::load().selected().cloned();
        self.profile = account.as_ref().map(|a| a.name.clone());
        self.signed_in = account.is_some_and(|a| {
            account::store()
                .get(&a.uuid)
                .ok()
                .flatten()
                .is_some_and(|s| serde_json::from_str::<Secrets>(&s).is_ok())
        });
    }
    pub fn status(&mut self, offline_name: &str, language: Language) -> String {
        self.language = language;
        match (&self.profile, self.signed_in) {
            (Some(name), true) => {
                tr!(language, "Signed in: {name}", { name = name })
            }
            (Some(name), false) => {
                tr!(language, "Sign in required: {name}", { name = name })
            }
            _ if self.pending() => tr!(language, "Signing in...").to_owned(),
            _ if !offline_name.trim().is_empty() => {
                tr!(language, "Offline: {name}", { name = offline_name })
            }
            _ => tr!(language, "Offline / Sign in").to_owned(),
        }
    }

    pub fn pending(&self) -> bool {
        self.task.is_some()
    }
    pub fn hide(&mut self) {
        self.visible = false;
    }
    pub fn close(&mut self) {
        self.auto_open_device = false;
        if let Some(task) = self.task.take() {
            task.cancel();
        }
        self.state = None;
        self.visible = false;
    }
    pub fn start(&mut self, id: &str) {
        self.start_device(id);
        self.auto_open_device = true;
    }
    pub fn start_device(&mut self, id: &str) {
        self.close();
        self.visible = true;
        let id = id.trim();
        if !account::legacy_client(id)
            && (id.len() != 36
                || !id.chars().enumerate().all(|(i, c)| {
                    if [8, 13, 18, 23].contains(&i) {
                        c == '-'
                    } else {
                        c.is_ascii_hexdigit()
                    }
                }))
        {
            self.message = tr!(
                self.language,
                "Register our Microsoft public client application first, then enter its client ID."
            )
            .to_owned();
            return;
        }
        self.message.clear();
        self.task = Some(SignIn::start_with_client_id(Some(id.into())));
        self.state = Some(SignInState::Starting);
    }
    pub fn poll(&mut self) {
        if let Some(task) = &self.task {
            self.state = Some(task.state());
            if let Some(SignInState::Done(result)) = &self.state {
                self.message = match result {
                    Ok(a) => tr!(self.language, "Signed in as {name}", { name = a.name }),
                    Err(e) => e.clone(),
                };
                self.task = None;
                self.refresh_status();
            }
        }
        if self.auto_open_device && matches!(self.state, Some(SignInState::Code(_))) {
            self.auto_open_device = false;
            self.open_link();
        }
    }
    pub fn sign_out(&mut self) {
        self.close();
        self.visible = true;
        let mut accounts = Accounts::load();
        if let Some(a) = accounts.selected().cloned() {
            self.message = match accounts
                .remove(account::store(), &a.uuid)
                .and_then(|_| accounts.save())
            {
                Ok(()) => tr!(self.language, "Signed out. Offline play is available.").to_owned(),
                Err(e) => e,
            };
        }
        self.refresh_status();
    }
    pub fn copy_code(&mut self) {
        if let Some(SignInState::Code(code)) = &self.state {
            self.message = if crate::platform::copy_login_code(&code.user_code) {
                tr!(
                    self.language,
                    "Code copied. Paste it on the Microsoft page."
                )
                .to_owned()
            } else {
                tr!(self.language, "Could not copy the code. Try again.").to_owned()
            };
        }
    }
    pub fn open_link(&mut self) {
        if let Some(SignInState::Code(code)) = &self.state {
            let mut url = url::Url::parse(&code.verification_uri).ok();
            if let Some(uri) = &mut url {
                uri.query_pairs_mut().append_pair("otc", &code.user_code);
            }
            let url = url
                .map(String::from)
                .unwrap_or_else(|| code.verification_uri.clone());
            self.copy_code();
            if let Err(e) = crate::platform::open_login_url(&url) {
                self.message = e;
            }
        }
    }
    pub fn review_app(&mut self) {
        if let Err(e) = crate::platform::open_login_url("https://aka.ms/mce-reviewappid") {
            self.message = e;
        }
    }
    pub fn session(id: &str) -> Result<Option<(Account, Secrets)>, String> {
        let mut accounts = Accounts::load();
        let Some(a) = accounts.selected().cloned() else {
            return Ok(None);
        };
        let secrets = account::store()
            .get(&a.uuid)?
            .and_then(|s| serde_json::from_str(&s).ok())
            .ok_or("Session ended. Sign in again, or sign out to play offline.")?;
        let (a, secrets) = account::fresh(&account::Endpoints::live(), id.trim(), &a, secrets)?;
        accounts.add(account::store(), a.clone(), &secrets)?;
        accounts.save()?;
        Ok(Some((a, secrets)))
    }
    pub fn draw(
        &mut self,
        ui: &mut Ui,
        targets: &mut Vec<([f32; 4], Action)>,
        offline_name: &str,
        editing_name: bool,
        language: Language,
    ) {
        targets.clear();
        ui.rounded(
            0.0,
            0.0,
            ui.w,
            ui.h,
            0.0,
            horizon_client::hud::rgba(0, 0, 0, 140),
        );
        let w = (ui.w - 32.0).min(640.0);
        let h = (ui.h - 32.0).min(560.0);
        let x = (ui.w - w) / 2.0;
        let y = (ui.h - h) / 2.0;
        ui.rounded(x, y, w, h, md3::SHAPE_XL, color::SURFACE_HIGH);
        let compact = h < 550.0;
        let code_state = matches!(self.state, Some(SignInState::Code(_)));
        let message_y = if compact {
            y + if code_state { 126.0 } else { 102.0 }
        } else {
            y + 166.0
        };
        let message_lines = if compact { 2 } else { 5 };
        let message_size = if compact { 13.0 } else { 14.0 };
        let message_line = if compact { 18.0 } else { 20.0 };
        let offline_label_y = if compact { y + 150.0 } else { y + h - 247.0 };
        let offline_field_y = if compact { y + 168.0 } else { y + h - 226.0 };
        let primary_y = if compact {
            y + h - 104.0
        } else {
            y + h - 148.0
        };
        let secondary_y = if compact { y + h - 54.0 } else { y + h - 98.0 };
        let show_offline = !self.pending() && (!compact || h >= 320.0);
        if editing_name {
            ui.text_line(
                Face::Medium,
                24.0,
                32.0,
                x + 24.0,
                y + 24.0,
                color::TEXT,
                tr!(language, "Offline player name"),
            );
            ui.text(
                Face::Regular,
                14.0,
                x + 24.0,
                y + 62.0,
                color::MUTED,
                tr!(language, "Used when signed out of Microsoft."),
            );
            field(
                ui,
                targets,
                [x + 24.0, y + 104.0, w - 48.0, 56.0],
                offline_name,
                Action::Field("name"),
                true,
            );
            button(
                ui,
                targets,
                [x + 24.0, y + h - 60.0, w - 48.0, 44.0],
                tr!(language, "Done"),
                Action::FinishNameEdit,
                true,
            );
            return;
        }

        ui.text_line(
            Face::Medium,
            24.0,
            32.0,
            x + 24.0,
            y + 24.0,
            color::TEXT,
            tr!(language, "Accounts"),
        );
        let status = self.status(offline_name, language);
        ui.text_line(
            Face::Medium,
            md3::BODY_MEDIUM,
            md3::BODY_MEDIUM_LINE,
            x + 24.0,
            y + 62.0,
            if self.signed_in {
                color::PRIMARY
            } else {
                color::MUTED
            },
            &status,
        );
        if let Some(SignInState::Code(code)) = &self.state {
            let label = tr!(language, "{code}    /    Copy code", {
                code = code.user_code
            });
            button(
                ui,
                targets,
                [
                    x + 24.0,
                    if compact { y + 72.0 } else { y + 96.0 },
                    w - 48.0,
                    if compact { 42.0 } else { 48.0 },
                ],
                &label,
                Action::CopyLoginCode,
                false,
            );
        } else {
            ui.text(
                Face::Regular,
                14.0,
                x + 24.0,
                if compact { y + 72.0 } else { y + 100.0 },
                color::TEXT,
                translate(
                    language,
                    if self.signed_in {
                        "Account saved securely. Sessions renew automatically."
                    } else {
                        "Your password is entered in Microsoft's browser."
                    },
                ),
            );
        }
        let text = match &self.state {
            Some(SignInState::Starting) => {
                translate(language, "Requesting a sign-in code...").to_owned()
            }
            _ => self.message.clone(),
        };
        let limit = ((w - 48.0) / 7.5).max(20.0) as usize;
        let mut line = String::new();
        let mut row = 0;
        for word in text.split_whitespace() {
            if line.len() + word.len() + 1 > limit {
                ui.text(
                    Face::Regular,
                    message_size,
                    x + 24.0,
                    message_y + row as f32 * message_line,
                    color::TEXT,
                    &line,
                );
                row += 1;
                line.clear();
                if row >= message_lines {
                    break;
                }
            }
            if !line.is_empty() {
                line.push(' ');
            }
            line.push_str(word);
        }
        if row < message_lines {
            ui.text(
                Face::Regular,
                message_size,
                x + 24.0,
                message_y + row as f32 * message_line,
                color::TEXT,
                &line,
            );
        }
        if show_offline {
            ui.text(
                Face::Regular,
                13.0,
                x + 24.0,
                offline_label_y,
                color::MUTED,
                tr!(language, "Offline player name"),
            );
            field(
                ui,
                targets,
                [
                    x + 24.0,
                    offline_field_y,
                    w - 48.0,
                    if compact { 48.0 } else { 56.0 },
                ],
                if offline_name.is_empty() {
                    tr!(language, "Enter an offline name")
                } else {
                    offline_name
                },
                Action::Field("name"),
                false,
            );
        }
        let half = (w - 60.0) / 2.0;
        if !compact && self.message.contains("not approved by Mojang") {
            button(
                ui,
                targets,
                [x + 24.0, y + h - 183.0, w - 48.0, 34.0],
                tr!(language, "Request Minecraft API approval"),
                Action::ReviewApp,
                false,
            );
        }
        let (label, action) = if matches!(self.state, Some(SignInState::Code(_))) {
            (
                translate(language, "Copy code and open browser"),
                Action::LoginLink,
            )
        } else if self.pending() {
            (
                translate(language, "Restart browser sign-in"),
                Action::SignIn,
            )
        } else {
            (
                translate(
                    language,
                    if self.signed_in {
                        "Switch Microsoft account"
                    } else {
                        "Sign in with browser"
                    },
                ),
                Action::SignIn,
            )
        };
        button(
            ui,
            targets,
            [
                x + 24.0,
                primary_y,
                w - 48.0,
                if compact { 44.0 } else { 48.0 },
            ],
            label,
            action,
            true,
        );
        button(
            ui,
            targets,
            [x + 24.0, secondary_y, half, 40.0],
            tr!(language, "Use a device code instead"),
            Action::DeviceSignIn,
            false,
        );
        button(
            ui,
            targets,
            [x + 36.0 + half, secondary_y, half, 40.0],
            if self.pending() {
                tr!(language, "Cancel sign-in")
            } else if self.profile.is_some() {
                tr!(language, "Sign out")
            } else {
                tr!(language, "Close")
            },
            if self.pending() {
                Action::CancelLogin
            } else if self.profile.is_some() {
                Action::SignOut
            } else {
                Action::CloseLogin
            },
            false,
        );
        if !compact {
            button(
                ui,
                targets,
                [x + 24.0, y + h - 52.0, w - 48.0, 36.0],
                if self.pending() {
                    tr!(language, "Back (sign-in continues)")
                } else {
                    tr!(language, "Done")
                },
                Action::CloseLogin,
                false,
            );
        }
    }
}
impl Drop for Login {
    fn drop(&mut self) {
        if let Some(task) = &self.task {
            task.cancel();
        }
    }
}
