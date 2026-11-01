//! Android `NativeActivity` entry point.
//!
//! The APK bundles the private 1.16.5 inputs (`world/`, `tables.json` and
//! `client.jar`) as the zip asset `assets/1165.zip`. Java-style assets are
//! only reachable through the asset manager, not as files, so the entry
//! point streams that zip into the app's private data directory
//! (`<internal_data_path>/1165`) and then hands the unpacked paths to
//! [`crate::app::run_android`].
//!
//! Unpacking is repeated on every launch: files are written to a temporary
//! name next to their target and renamed into place, so a crash halfway
//! through can never leave a half-written world file and the next launch
//! simply overwrites the tree again.

use std::ffi::OsString;
use std::fs::{self, File};
use std::io::{self, Read, Seek, Write};
use std::path::{Component, Path, PathBuf};

use winit::platform::android::activity::AndroidApp;

use crate::app;

/// Logcat tag for the whole native activity.
const TAG: &str = "horizon-1-16-5";
/// The zip inside the APK.
const ASSET_NAME: &str = "1165.zip";
/// Directory (under the internal data path) the zip is unpacked into.
const EXTRACT_DIR: &str = "1165";
/// Temporary copy of the zip while it is being read.
const STAGING_FILE: &str = ".1165.zip.part";

/// The native activity's main thread. Errors are logged and end the process
/// quietly instead of unwinding into the Java/NativeActivity glue.
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(
        android_logger::Config::default()
            .with_max_level(log::LevelFilter::Info)
            .with_tag(TAG),
    );

    let Some(home) = app.internal_data_path() else {
        log::error!("the activity exposes no internal data path; cannot unpack the game assets");
        std::process::exit(1);
    };
    let root = home.join(EXTRACT_DIR);

    if let Err(error) = unpack_assets(&app, &root) {
        log::error!(
            "unpacking {ASSET_NAME} into {} failed: {error}",
            root.display()
        );
        std::process::exit(1);
    }

    log::info!("starting the 1.16.5 play loop from {}", root.display());
    let result = app::run_android(
        app,
        root.join("world"),
        root.join("tables.json"),
        root.join("client.jar"),
    );
    if let Err(error) = result {
        log::error!("the 1.16.5 play loop stopped: {error}");
        std::process::exit(1);
    }
    // Winit permits one EventLoop per process. NativeActivity otherwise keeps
    // this process cached after Quit, making the next launch fail.
    std::process::exit(0);
}

/// Whether the extracted tree is complete and should not be overwritten.
///
/// The world is writable after the first launch; re-extracting the bundled
/// zip on every start would silently discard saved blocks. `.ready` is
/// written only after a complete extraction, so a crash halfway through is
/// retried on the next launch.
fn extracted_tree_is_ready(root: &Path) -> bool {
    // The world directory is optional: without it the game generates a flat
    // offline world, which is the default standalone mode.
    root.join(".ready").is_file()
        && root.join("tables.json").is_file()
        && root.join("client.jar").is_file()
}

/// Streams `assets/1165.zip` out of the APK and unpacks it into `root`.
fn unpack_assets(app: &AndroidApp, root: &Path) -> Result<(), String> {
    if extracted_tree_is_ready(root) {
        return Ok(());
    }
    let mut asset = app
        .asset_manager()
        .open(c"1165.zip")
        .ok_or_else(|| format!("the APK has no '{ASSET_NAME}' asset"))?;
    fs::create_dir_all(root).map_err(|error| format!("creating {}: {error}", root.display()))?;

    // Keep the archive outside the extracted tree: `client.jar` alone is
    // large, and a crash must not leave a stale zip among the game files.
    let staging = root.parent().unwrap_or(root).join(STAGING_FILE);

    let result = (|| {
        let mut file = File::create(&staging)
            .map_err(|error| format!("creating {}: {error}", staging.display()))?;
        io::copy(&mut asset, &mut file)
            .map_err(|error| format!("copying {ASSET_NAME} to {}: {error}", staging.display()))?;
        file.flush()
            .map_err(|error| format!("flushing {}: {error}", staging.display()))?;
        drop(file);

        let archive = File::open(&staging)
            .map_err(|error| format!("opening {}: {error}", staging.display()))?;
        let mut archive = zip::ZipArchive::new(archive)
            .map_err(|error| format!("reading {ASSET_NAME}: {error}"))?;
        extract_archive(&mut archive, root)?;
        fs::write(root.join(".ready"), b"1\n")
            .map_err(|error| format!("marking {} ready: {error}", root.display()))
    })();

    // Only needed while the archive is being read; the extracted tree is the
    // durable copy.
    let _ = fs::remove_file(&staging);
    result
}

/// Extracts every entry of `archive` under `root`, rejecting names that
/// could escape it.
fn extract_archive<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    root: &Path,
) -> Result<(), String> {
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| format!("reading archive entry {index}: {error}"))?;
        let name = entry.name().to_owned();
        let relative = safe_entry_path(&name)
            .ok_or_else(|| format!("refusing unsafe archive entry '{name}'"))?;
        let target = root.join(&relative);

        if entry.is_dir() {
            if target.exists() && !target.is_dir() {
                fs::remove_file(&target).map_err(|error| {
                    format!(
                        "replacing file {} with a directory: {error}",
                        target.display()
                    )
                })?;
            }
            fs::create_dir_all(&target)
                .map_err(|error| format!("creating {}: {error}", target.display()))?;
            continue;
        }

        let parent = target.parent().unwrap_or(root);
        fs::create_dir_all(parent)
            .map_err(|error| format!("creating {}: {error}", parent.display()))?;
        // A directory left where a file belongs cannot be replaced by a
        // rename, so clear it first.
        if target.is_dir() {
            fs::remove_dir_all(&target).map_err(|error| {
                format!(
                    "replacing directory {} with a file: {error}",
                    target.display()
                )
            })?;
        }

        let staging = staging_path(&target);
        let copy = (|| -> io::Result<()> {
            let mut file = File::create(&staging)?;
            io::copy(&mut entry, &mut file)?;
            file.flush()?;
            Ok(())
        })();
        if let Err(error) = copy {
            let _ = fs::remove_file(&staging);
            return Err(format!("writing {}: {error}", target.display()));
        }
        // Writing under a temporary name and renaming keeps every visible
        // file complete, even if the process dies mid-extraction.
        if let Err(error) = fs::rename(&staging, &target) {
            let _ = fs::remove_file(&staging);
            return Err(format!("installing {}: {error}", target.display()));
        }
    }
    Ok(())
}

/// Maps an archive entry name to a relative path, or `None` when the name is
/// absolute, climbs out with `..`, or is not a plain relative path.
fn safe_entry_path(name: &str) -> Option<PathBuf> {
    if name.is_empty() || name.contains('\\') || name.contains('\0') {
        return None;
    }
    let mut path = PathBuf::new();
    for component in Path::new(name).components() {
        match component {
            Component::Normal(part) => path.push(part),
            // `./x` is harmless and some jar makers emit it.
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if path.as_os_str().is_empty() {
        None
    } else {
        Some(path)
    }
}

/// The hidden temporary file an entry is written to before being renamed.
fn staging_path(target: &Path) -> PathBuf {
    let name = target
        .file_name()
        .map_or_else(|| OsString::from("entry"), |name| name.to_os_string());
    let mut staged = OsString::from(".");
    staged.push(name);
    staged.push(".part");
    target.with_file_name(staged)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_names_stay_relative() {
        assert_eq!(
            safe_entry_path("tables.json"),
            Some(PathBuf::from("tables.json"))
        );
        assert_eq!(
            safe_entry_path("world/level.dat"),
            Some(PathBuf::from("world").join("level.dat"))
        );
        assert_eq!(
            safe_entry_path("./world/./region/r.0.0.mca"),
            Some(PathBuf::from("world").join("region").join("r.0.0.mca"))
        );
    }

    #[test]
    fn absolute_names_are_rejected() {
        assert_eq!(safe_entry_path("/etc/passwd"), None);
        assert_eq!(safe_entry_path("//server/share"), None);
        assert_eq!(safe_entry_path("\\\\server\\share"), None);
        assert_eq!(safe_entry_path("C:\\Windows\\system32"), None);
    }

    #[test]
    fn parent_components_are_rejected() {
        assert_eq!(safe_entry_path(".."), None);
        assert_eq!(safe_entry_path("../zipslip"), None);
        assert_eq!(safe_entry_path("world/../../zipslip"), None);
        assert_eq!(safe_entry_path("world/.."), None);
    }

    #[test]
    fn empty_and_component_free_names_are_rejected() {
        assert_eq!(safe_entry_path(""), None);
        assert_eq!(safe_entry_path("."), None);
        assert_eq!(safe_entry_path("./"), None);
        assert_eq!(safe_entry_path("a\0b"), None);
    }

    #[test]
    fn staging_files_are_hidden_siblings() {
        let target = Path::new("/data/user/0/dev.horizon/world/level.dat");
        assert_eq!(
            staging_path(target),
            PathBuf::from("/data/user/0/dev.horizon/world/.level.dat.part")
        );
    }
}
