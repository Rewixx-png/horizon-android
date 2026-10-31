//! In-app updater: asks GitHub for the latest release, downloads the APK for
//! this ABI, verifies the release's SHA-256 and hands it to the system
//! installer through the app's small FileProvider-compatible provider.

use horizon_client::update_check::{self, REPOSITORY, Release};
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::{Read, Write},
    path::Path,
    sync::mpsc,
    time::Duration,
};

const ABI: &str = if cfg!(target_arch = "aarch64") {
    "arm64-v8a"
} else if cfg!(target_arch = "x86_64") {
    "x86_64"
} else if cfg!(target_arch = "arm") {
    "armeabi-v7a"
} else {
    "x86"
};

enum State {
    Idle,
    Checking,
    Available(Release),
    Downloading { release: Release, progress: f32 },
    Ready,
}

enum DownloadEvent {
    Progress(f32),
    Done(Result<(), String>),
}

pub(super) struct Update {
    state: State,
    error: Option<String>,
    check: Option<mpsc::Receiver<Result<Option<Release>, String>>>,
    download: Option<mpsc::Receiver<DownloadEvent>>,
    checked: bool,
}

impl Update {
    pub(super) fn new() -> Self {
        Self {
            state: State::Idle,
            error: None,
            check: None,
            download: None,
            checked: false,
        }
    }

    pub(super) fn check(&mut self) {
        if self.checked || self.check.is_some() {
            return;
        }
        self.checked = true;
        if !crate::PUBLIC_BUILD {
            return;
        }
        self.state = State::Checking;
        let (sender, receiver) = mpsc::channel();
        self.check = Some(receiver);
        std::thread::spawn(move || {
            let _ = sender.send(fetch_latest());
        });
    }

    pub(super) fn busy(&self) -> bool {
        self.download.is_some()
    }

    pub(super) fn checking(&self) -> bool {
        self.check.is_some()
    }

    pub(super) fn available(&self) -> Option<&Release> {
        match &self.state {
            State::Available(release) => Some(release),
            _ => None,
        }
    }

    pub(super) fn downloading(&self) -> Option<f32> {
        match &self.state {
            State::Downloading { progress, .. } => Some(*progress),
            _ => None,
        }
    }

    pub(super) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub(super) fn start_download(&mut self, home: &Path) {
        let release = match &self.state {
            State::Available(release) => release.clone(),
            _ => return,
        };
        self.error = None;
        self.state = State::Downloading {
            release: release.clone(),
            progress: 0.0,
        };
        let (sender, receiver) = mpsc::channel();
        self.download = Some(receiver);
        let home = home.to_path_buf();
        std::thread::spawn(move || {
            let result = download(&release, &home, &sender);
            let _ = sender.send(DownloadEvent::Done(result));
        });
    }

    /// Drain background results. Returns true when the launcher should redraw.
    pub(super) fn poll(&mut self) -> bool {
        let mut changed = false;
        let check = self.check.as_ref().map(mpsc::Receiver::try_recv);
        match check {
            Some(Ok(result)) => {
                self.check = None;
                self.state = match result {
                    Ok(Some(release)) => State::Available(release),
                    Ok(None) => State::Idle,
                    Err(_) => State::Idle,
                };
                changed = true;
            }
            Some(Err(mpsc::TryRecvError::Disconnected)) => {
                self.check = None;
                self.state = State::Idle;
                changed = true;
            }
            Some(Err(mpsc::TryRecvError::Empty)) | None => {}
        }
        loop {
            let event = match self.download.as_ref().map(mpsc::Receiver::try_recv) {
                Some(Ok(event)) => event,
                Some(Err(mpsc::TryRecvError::Disconnected)) => {
                    self.download = None;
                    if let State::Downloading { release, .. } =
                        std::mem::replace(&mut self.state, State::Idle)
                    {
                        self.state = State::Available(release);
                    }
                    self.error = Some("The update download stopped unexpectedly.".into());
                    changed = true;
                    break;
                }
                Some(Err(mpsc::TryRecvError::Empty)) | None => break,
            };
            match event {
                DownloadEvent::Progress(progress) => {
                    if let State::Downloading {
                        progress: current, ..
                    } = &mut self.state
                    {
                        *current = progress;
                    }
                    changed = true;
                }
                DownloadEvent::Done(result) => {
                    self.download = None;
                    let release = match std::mem::replace(&mut self.state, State::Idle) {
                        State::Downloading { release, .. } => release,
                        other => {
                            self.state = other;
                            continue;
                        }
                    };
                    match result {
                        Ok(()) => {
                            if crate::platform::install_update() {
                                self.state = State::Ready;
                                self.error = None;
                            } else {
                                self.state = State::Available(release);
                                self.error = Some(
                                    "The installer could not be opened. Allow installs from Horizon and try again."
                                        .into(),
                                );
                            }
                        }
                        Err(error) => {
                            self.state = State::Available(release);
                            self.error = Some(error);
                        }
                    }
                    changed = true;
                }
            }
        }
        changed
    }
}

fn fetch_latest() -> Result<Option<Release>, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(15))
        .timeout_read(Duration::from_secs(30))
        .build();
    let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
    let body = agent
        .get(&url)
        .set("User-Agent", "Horizon-Android")
        .call()
        .map_err(|error| error.to_string())?
        .into_string()
        .map_err(|error| error.to_string())?;
    update_check::latest(&body, ABI, crate::backend::VERSION)
}

fn download(
    release: &Release,
    home: &Path,
    events: &mpsc::Sender<DownloadEvent>,
) -> Result<(), String> {
    let sums_url = release
        .sums_url
        .as_deref()
        .ok_or("The release has no SHA256SUMS.txt")?;
    let agent = ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(20))
        .timeout_read(Duration::from_secs(120))
        .build();
    let sums = agent
        .get(sums_url)
        .set("User-Agent", "Horizon-Android")
        .call()
        .map_err(|error| error.to_string())?
        .into_string()
        .map_err(|error| error.to_string())?;
    let expected = sums
        .lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            let hash = parts.next()?;
            let name = parts.next()?.trim_start_matches('*');
            (name == release.apk_name).then_some(hash)
        })
        .next()
        .ok_or_else(|| format!("SHA256SUMS.txt has no {}", release.apk_name))?
        .to_owned();
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("SHA256SUMS.txt has an invalid hash".into());
    }

    let target = home.join("update.apk");
    let partial = home.join("update.apk.partial");
    let response = agent
        .get(&release.apk_url)
        .set("User-Agent", "Horizon-Android")
        .call()
        .map_err(|error| error.to_string())?;
    let mut reader = response.into_reader();
    let mut file = fs::File::create(&partial).map_err(|error| error.to_string())?;
    let mut digest = Sha256::new();
    let mut buffer = [0u8; 128 * 1024];
    let mut received = 0u64;
    let mut percent = -1i32;
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|error| error.to_string())?;
        if count == 0 {
            break;
        }
        file.write_all(&buffer[..count])
            .map_err(|error| error.to_string())?;
        digest.update(&buffer[..count]);
        received += count as u64;
        if let Some(next) = received.saturating_mul(100).checked_div(release.apk_size) {
            let next = next as i32;
            if next != percent {
                percent = next;
                let _ = events.send(DownloadEvent::Progress(
                    (next as f32 / 100.0).clamp(0.0, 1.0),
                ));
            }
        }
    }
    file.flush().map_err(|error| error.to_string())?;
    drop(file);
    if release.apk_size > 0 && received != release.apk_size {
        let _ = fs::remove_file(&partial);
        return Err(format!(
            "APK size mismatch: expected {} bytes, got {received}",
            release.apk_size
        ));
    }
    let actual = format!("{:x}", digest.finalize());
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = fs::remove_file(&partial);
        return Err("APK SHA-256 does not match SHA256SUMS.txt".into());
    }
    let _ = fs::remove_file(&target);
    fs::rename(&partial, &target).map_err(|error| error.to_string())?;
    Ok(())
}
