//! Modpacks: import a Java modpack (a Modrinth `.mrpack`, a CurseForge zip, an instance folder),
//! show what Horizon runs of it, port that on this computer from the player's own downloads, and
//! play it as a profile of its own (docs/modpacks.md).
//!
//! Each pack lives in `ports/<slug>/` in the game's home: the work folder `horizon-port-pack`
//! writes (pack.json, inventory, the jars, the carried-over mods, the coverage report) and
//! `home/`, the profile's own saves and options. Nothing is uploaded or shared: mod jars are
//! often All Rights Reserved, so the ports stay on the player's machine.
use crate::backend;
use crate::browser::horizon::pack as horizon_pack;
use horizon_port_pack::{
    fetch::{Fetched, How},
    import::Pack,
    pipeline::{self, Log},
    report::{Row, Status},
    resourcepacks::{self, Report as ResourceReport, State},
};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where imported packs live.
pub fn ports_dir() -> PathBuf {
    backend::game_home().join("ports")
}

/// A pack's folder name: its name in lowercase words joined by dashes.
pub fn slug(name: &str) -> String {
    let words: Vec<String> = name
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
        .collect();
    if words.is_empty() {
        "pack".into()
    } else {
        words.join("-")
    }
}

/// An imported pack, as its work folder has it.
pub struct Imported {
    pub slug: String,
    pub work: PathBuf,
    pub pack: Pack,
    pub rows: Vec<Row>,
    pub fetched: Fetched,
    /// The carried-over mods the profile loads.
    pub mods: Vec<PathBuf>,
    /// Mods left off, with why: a ported one an earlier one owns a namespace of, or one whose
    /// jar only CurseForge serves (CurseForge downloads coming soon).
    pub left_off: Vec<String>,
    /// The mods the port step gets and carries over (content, data tier).
    pub portable: usize,
    /// Ordinary resource packs staged for this profile; enabling them is the player's choice.
    pub resourcepacks: usize,
    /// Index-listed resource packs the port step can obtain (separate from Java mods).
    pub indexed_resources: usize,
}

impl Imported {
    pub fn can_play(&self) -> bool {
        !self.mods.is_empty() || self.resourcepacks > 0
    }
    pub fn can_port(&self) -> bool {
        self.portable > 0 || self.indexed_resources > 0
    }
    pub fn title(&self) -> &str {
        if self.pack.name.is_empty() {
            &self.slug
        } else {
            &self.pack.name
        }
    }
    pub fn count(&self, status: Status) -> usize {
        self.rows.iter().filter(|r| r.status == status).count()
    }
    /// "Minecraft 1.21.1 · neoforge 21.1.259 · 496 mods".
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if !self.pack.minecraft.is_empty() {
            parts.push(format!("Minecraft {}", self.pack.minecraft));
        }
        if !self.pack.loader.is_empty() {
            parts.push(format!("{} {}", self.pack.loader, self.pack.loader_version));
        }
        parts.push(crate::i18n::trf(
            "{count} files",
            &[(
                "count",
                &self.rows.len().max(self.pack.files.len()).to_string(),
            )],
        ));
        parts.join("  ·  ")
    }
    /// What getting the jars did, in one line, or None before it ran.
    pub fn jars(&self) -> Option<String> {
        if self.fetched.files.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for (how, what) in [
            (How::Downloaded, "downloaded"),
            (How::Local, "copied from your folder"),
            (How::Present, "already here"),
            (
                How::NeedsKey,
                "only on CurseForge (CurseForge downloads coming soon: choose your installed \
                 instance instead)",
            ),
            (How::NotFound, "not found"),
            (How::Failed, "failed"),
        ] {
            let n = self.fetched.count(how);
            if n > 0 {
                parts.push(format!("{n} {}", crate::i18n::tr(what)));
            }
        }
        Some(crate::i18n::trf(
            "Jars: {summary}.",
            &[("summary", &parts.join(", "))],
        ))
    }
}

/// Reads the pack in `work`.
pub fn load(work: &Path) -> Result<Imported, String> {
    let pack: Pack = pipeline::read_json(&work.join("pack.json"))?;
    let rows: Vec<Row> = pipeline::read_json(&work.join("coverage.json")).unwrap_or_default();
    let records: Vec<horizon_port_pack::inventory::Record> =
        pipeline::read_json(&work.join("inventory.json")).unwrap_or_default();
    let (mods, mut left_off) = pipeline::loadable(&carried(work));
    let fetched = Fetched::load(work);
    left_off.extend(
        fetched
            .files
            .iter()
            .filter(|o| o.how == How::NeedsKey)
            .map(|o| format!("{}: CurseForge downloads coming soon", o.key)),
    );
    let resources = ResourceReport::load(work);
    let profile: ResourceReport =
        pipeline::read_json(&work.join("profile-resourcepacks.json")).unwrap_or_default();
    for (path, outcome) in resources.files.iter().chain(&profile.files) {
        if matches!(
            outcome.state,
            State::Conflict | State::Failed | State::Missing | State::Unsupported
        ) {
            left_off.push(format!("{path}: {:?}: {}", outcome.state, outcome.detail));
        }
    }
    left_off.sort();
    left_off.dedup();
    let resourcepacks = resourcepacks::available(work).len();
    let indexed_resources = pack
        .files
        .iter()
        .filter(|f| f.kind() == "resourcepacks" && f.client != "unsupported")
        .count();
    Ok(Imported {
        resourcepacks,
        indexed_resources,
        slug: work
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
        work: work.to_path_buf(),
        pack,
        rows,
        fetched,
        mods,
        left_off,
        portable: records.iter().filter(|r| pipeline::portable(r)).count(),
    })
}

/// Every imported pack, by name.
pub fn list() -> Vec<Imported> {
    list_in(&ports_dir())
}

pub(crate) fn list_in(ports: &Path) -> Vec<Imported> {
    let mut packs: Vec<Imported> = fs::read_dir(ports)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| load(&e.path()).ok())
        .collect();
    packs.sort_by_key(|p| p.title().to_ascii_lowercase());
    packs
}

/// The carried-over mods in a work folder (`mods/<id>/carry/` with its manifest).
pub fn carried(work: &Path) -> Vec<PathBuf> {
    let mut mods: Vec<PathBuf> = fs::read_dir(work.join("mods"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path().join("carry"))
        .filter(|c| c.join("horizon-mod.toml").is_file())
        .collect();
    mods.sort();
    mods
}

/// Imports the pack at `path` into `ports/<slug>/` and writes its coverage report from
/// metadata (Modrinth answers by file hash; nothing is downloaded yet). Importing the same pack
/// again keeps the jars and ports it already has.
pub fn import(path: &Path, log: Log) -> Result<Imported, String> {
    let pack = horizon_port_pack::import::import(path)?;
    let name = if pack.name.is_empty() || pack.name == "modlist" {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        pack.name.clone()
    };
    let work = ports_dir().join(slug(&name));
    log(format!("Importing {name} into {}", work.display()));
    pipeline::import(path, &work, log)?;
    pipeline::inventory(&work, false, log)?;
    pipeline::report(&work, log)?;
    prepare_resourcepacks(&work);
    load(&work)
}

/// Ports what this computer can: the portable mods' jars (from `from` when the player has them,
/// else the pack's links or Modrinth), carried over for `resources`, then the report again.
pub fn port(
    work: &Path,
    resources: Option<&Path>,
    from: Option<&Path>,
    log: Log,
) -> Result<Imported, String> {
    pipeline::port(work, resources, from, log)?;
    prepare_resourcepacks(work);
    load(work)
}

/// What the mod browser does with a modpack: imports it, then ports what this computer can
/// (from the pack's links and Modrinth), so it plays straight away. A port that fails leaves the
/// pack imported; the error says so.
pub fn import_and_port(
    path: &Path,
    resources: Option<&Path>,
    log: Log,
) -> Result<Imported, String> {
    let pack = import(path, log)?;
    if !pack.can_port() {
        return Ok(pack);
    }
    port(&pack.work, resources, None, log)
        .map_err(|e| format!("{} is imported, but porting it failed: {e}", pack.title()))
}

/// One line on what an import and port gave: the mods carried over and the ones that weren't.
pub fn outcome(pack: &Imported) -> String {
    let not = pack.count(Status::NotYet);
    format!(
        "{} is imported{}: {} of its {} files carried over ({} ported, {} partly), {} don't \
         yet, {} aren't needed{}. {} resource packs available; enable them in Resource Packs. Its list says why.",
        pack.title(),
        if pack.mods.is_empty() {
            ""
        } else {
            " and ported"
        },
        pack.count(Status::Ported) + pack.count(Status::Partial),
        pack.rows.len(),
        pack.count(Status::Ported),
        pack.count(Status::Partial),
        not,
        pack.count(Status::NotNeeded),
        match pack.left_off.len() {
            0 => String::new(),
            n => format!("; {n} are left off (listed under the pack)"),
        },
        pack.resourcepacks,
    )
}

/// Removes an imported pack: its jars, ports and the profile's saves.
pub fn remove(work: &Path) -> Result<(), String> {
    if horizon_pack::is_pack(work) {
        return horizon_pack::remove(work);
    }
    if work.parent() != Some(ports_dir().as_path()) {
        return Err(format!("{} is not an imported pack", work.display()));
    }
    fs::remove_dir_all(work).map_err(|e| format!("{}: {e}", work.display()))
}

/// The profile's own game home: saves, options.txt, logs.
pub fn profile_home(work: &Path) -> PathBuf {
    work.join("home")
}

/// Turns the game's command into this pack's profile: its own home (the player's options.txt
/// copied in the first time, so keys and video settings carry over) and its carried-over mods.
/// A modpack from Horizon's registry (`horizon-packs/<id>/`) plays its own profile the same way.
pub fn apply_profile(cmd: &mut Command, work: &Path) -> Result<(), String> {
    if horizon_pack::is_pack(work) {
        return horizon_pack::apply(cmd, work);
    }
    // Only mods that load together: the game would refuse the later of two sharing a namespace.
    let (mods, _) = pipeline::loadable(&carried(work));
    if mods.is_empty() && resourcepacks::available(work).is_empty() {
        return Err("No content mods or resource packs are available: port the pack first.".into());
    }
    prepare_resourcepacks(work);
    let home = profile_home(work);
    fs::create_dir_all(&home).map_err(|e| format!("{}: {e}", home.display()))?;
    let options = home.join("options.txt");
    let own = backend::game_home().join("options.txt");
    if !options.exists() && own.is_file() {
        let _ = fs::copy(own, &options);
    }
    cmd.current_dir(&home).env("HORIZON_HOME", &home);
    for m in mods {
        cmd.arg("--dev-mod").arg(m);
    }
    Ok(())
}

/// Installs the staged resource packs into the profile. A pack that cannot be installed
/// is logged and left off; it never blocks importing, porting or launching the mods.
fn prepare_resourcepacks(work: &Path) {
    let result = resourcepacks::install(work, &profile_home(work))
        .and_then(|report| pipeline::write_json(&work.join("profile-resourcepacks.json"), &report));
    if let Err(e) = result {
        eprintln!("resource packs not installed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("All the Mods 10"), "all-the-mods-10");
        assert_eq!(slug("Create: Above & Beyond!"), "create-above-beyond");
        assert_eq!(slug("  "), "pack");
    }

    #[test]
    fn jars_only_curseforge_serves_are_left_off_as_coming_soon() {
        use horizon_port_pack::fetch::Outcome;
        let work = std::env::temp_dir().join(format!("horizon-launcher-cf-{}", std::process::id()));
        let _ = fs::remove_dir_all(&work);
        fs::create_dir_all(&work).unwrap();
        pipeline::write_json(&work.join("pack.json"), &Pack::default()).unwrap();
        let fetched = Fetched {
            files: vec![Outcome {
                key: "allthemodium".into(),
                file: "mods/allthemodium.jar".into(),
                how: How::NeedsKey,
                detail: String::new(),
            }],
        };
        pipeline::write_json(&work.join("fetch.json"), &fetched).unwrap();
        let pack = load(&work).unwrap();
        assert_eq!(
            pack.left_off,
            ["allthemodium: CurseForge downloads coming soon"]
        );
        assert!(pack
            .jars()
            .unwrap()
            .contains("CurseForge downloads coming soon"));
        fs::remove_dir_all(&work).unwrap();
    }

    #[test]
    fn a_profile_has_its_own_home_and_mods() {
        let root =
            std::env::temp_dir().join(format!("horizon-launcher-pack-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let work = root.join("ports/small");
        let mut cmd = Command::new("true");
        assert!(apply_profile(&mut cmd, &work).is_err());
        for (id, namespaces) in [("a", ""), ("b", ""), ("d", "\"a\"")] {
            let carry = work.join("mods").join(id).join("carry");
            fs::create_dir_all(&carry).unwrap();
            fs::write(
                carry.join("horizon-mod.toml"),
                format!(
                    "id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\napi = \"0.2\"\n\
                     capabilities = [\"registries\"]\nnamespaces = [{namespaces}]\n"
                ),
            )
            .unwrap();
        }
        // Left out: no manifest (c), and one owning a namespace of an earlier mod (d).
        fs::create_dir_all(work.join("mods/c/carry")).unwrap();
        apply_profile(&mut cmd, &work).unwrap();
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args.len(), 4);
        assert_eq!(
            Path::new(&args[1]),
            work.join("mods").join("a").join("carry")
        );
        assert_eq!(
            Path::new(&args[3]),
            work.join("mods").join("b").join("carry")
        );
        assert_eq!(cmd.get_current_dir(), Some(profile_home(&work).as_path()));
        assert!(profile_home(&work).is_dir());
        fs::remove_dir_all(&root).unwrap();
    }
    #[test]
    fn resource_only_profile_can_play_without_enabling_or_overwriting_packs() {
        let root = std::env::temp_dir().join(format!(
            "horizon-launcher-resource-profile-{}",
            std::process::id()
        ));
        let work = root.join("ports/looks");
        fs::create_dir_all(work.join("resourcepacks/Look")).unwrap();
        fs::write(work.join("resourcepacks/Look/pack.mcmeta"), b"{}").unwrap();
        fs::write(work.join("resourcepacks/look.zip"), b"zip").unwrap();
        pipeline::write_json(&work.join("pack.json"), &Pack::default()).unwrap();
        let home = profile_home(&work);
        fs::create_dir_all(home.join("resourcepacks")).unwrap();
        fs::write(home.join("resourcepacks/look.zip"), b"user edit").unwrap();
        fs::write(home.join("options.txt"), b"resourcePacks:[]\n").unwrap();
        let mut cmd = Command::new("true");
        apply_profile(&mut cmd, &work).unwrap();
        assert_eq!(cmd.get_args().count(), 0);
        assert_eq!(cmd.get_current_dir(), Some(home.as_path()));
        assert!(cmd
            .get_envs()
            .any(|(k, v)| k == "HORIZON_HOME" && v == Some(home.as_os_str())));
        assert!(home.join("resourcepacks/Look/pack.mcmeta").is_file());
        assert_eq!(
            fs::read(home.join("resourcepacks/look.zip")).unwrap(),
            b"user edit"
        );
        assert_eq!(
            fs::read(home.join("options.txt")).unwrap(),
            b"resourcePacks:[]\n"
        );
        let pack = load(&work).unwrap();
        assert!(pack.can_play());
        assert!(!pack.can_port());
        assert_eq!(pack.resourcepacks, 2);
        assert!(pack
            .left_off
            .iter()
            .any(|l| l.contains("Conflict") && l.contains("look.zip")));
        assert!(outcome(&pack).contains("2 resource packs available"));
        apply_profile(&mut Command::new("true"), &work).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn resource_downloads_enable_port_even_when_no_mods_are_portable() {
        let root = std::env::temp_dir().join(format!(
            "horizon-launcher-resource-index-{}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        let pack = Pack {
            files: vec![horizon_port_pack::import::PackFile {
                path: "resourcepacks/look.zip".into(),
                ..Default::default()
            }],
            ..Default::default()
        };
        pipeline::write_json(&root.join("pack.json"), &pack).unwrap();
        let p = load(&root).unwrap();
        assert!(p.can_port());
        assert!(!p.can_play());
        assert_eq!(p.portable, 0);
        assert_eq!(p.indexed_resources, 1);
        fs::remove_dir_all(root).unwrap();
    }
}

/// A local file the player picked on the Mods page, by what it holds: a
/// content mod (`.hmod`) installs as a mod, and every other archive imports as
/// a modpack.
pub enum Picked {
    /// A `.hmod`, installed into `home`'s `horizon-mods/` and turned on.
    Mod(InstalledMod),
    /// Anything else: the modpack import.
    Pack,
}

/// A content mod installed from a file.
#[derive(Debug)]
pub struct InstalledMod {
    pub id: String,
    pub name: String,
    pub version: String,
}

/// Whether `path` is a Horizon content mod: a `.hmod`, or a folder holding
/// `horizon-mod.toml`. A zip whose root holds that manifest counts too, which
/// is exactly what the game reads (a jar of a Java mod does not).
pub fn is_content_mod(path: &Path) -> bool {
    if path.is_dir() {
        return path.join("horizon-mod.toml").is_file();
    }
    match path.extension().and_then(|e| e.to_str()) {
        Some(e) if e.eq_ignore_ascii_case("hmod") => true,
        Some(e) if e.eq_ignore_ascii_case("zip") || e.eq_ignore_ascii_case("hnmod") => false,
        _ => false,
    }
}

/// Installs the content mod at `path` into `home`: `horizon-mods/<id>.hmod`,
/// turned on in `horizon-mods.txt` as the game's Options > Mods would. The file
/// is read first (its manifest and the content files the game needs), so a
/// broken or foreign archive is refused here instead of failing in the game.
pub fn install_content_mod(path: &Path, home: &Path) -> Result<InstalledMod, String> {
    let manifest = read_manifest(path)?;
    let id = manifest.id.clone();
    let folder = home.join(crate::browser::java::MODS_DIR);
    fs::create_dir_all(&folder).map_err(|e| format!("{}: {e}", folder.display()))?;
    let target = folder.join(format!("{id}.hmod"));
    if path.is_file() {
        if path != target {
            fs::copy(path, &target)
                .map_err(|e| format!("{}: {e}", target.display()))?;
        }
    } else {
        write_folder_mod(path, &target)?;
    }
    crate::browser::java::set_enabled(home, &id, true)?;
    Ok(InstalledMod {
        id,
        name: manifest.name,
        version: manifest.version,
    })
}

/// What a content mod says about itself: `horizon-mod.toml`, read without
/// unpacking the rest.
struct Manifest {
    id: String,
    name: String,
    version: String,
}

fn read_manifest(path: &Path) -> Result<Manifest, String> {
    let text = if path.is_dir() {
        fs::read_to_string(path.join("horizon-mod.toml"))
            .map_err(|e| format!("{}: {e}", path.display()))?
    } else {
        let file = fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut zip = zip::ZipArchive::new(file)
            .map_err(|e| format!("{} is not a Horizon mod: {e}", path.display()))?;
        let mut entry = zip
            .by_name("horizon-mod.toml")
            .map_err(|_| {
                format!(
                    "{} is not a Horizon mod: it has no horizon-mod.toml at its root",
                    path.display()
                )
            })?;
        let mut text = String::new();
        std::io::Read::read_to_string(&mut entry, &mut text)
            .map_err(|e| format!("horizon-mod.toml: {e}"))?;
        text
    };
    // `toml::from_str`, not `str::parse`: the latter reads the file as one value and
    // refuses a manifest whose first line is a comment, as carried-over mods have.
    let value: toml::Value =
        toml::from_str(&text).map_err(|e| format!("horizon-mod.toml: {e}"))?;
    let field = |key: &str| {
        value
            .get(key)
            .and_then(|v| v.as_str())
            .map(str::to_owned)
    };
    let id = field("id").ok_or("horizon-mod.toml has no id")?;
    let valid = !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_');
    if !valid {
        return Err(format!(
            "{id:?} is not a mod id: lowercase letters, digits and _ only"
        ));
    }
    Ok(Manifest {
        name: field("name").unwrap_or_else(|| id.clone()),
        version: field("version").unwrap_or_else(|| "unknown".into()),
        id,
    })
}

/// Packs a folder mod (`horizon-mod.toml`, `assets/`, `data/`) into a `.hmod`,
/// as `cargo horizon-mod build` does.
fn write_folder_mod(dir: &Path, target: &Path) -> Result<(), String> {
    let file = fs::File::create(target).map_err(|e| format!("{}: {e}", target.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let mut add = |name: String, from: &Path| -> Result<(), String> {
        let bytes = fs::read(from).map_err(|e| format!("{}: {e}", from.display()))?;
        zip.start_file(name, options)
            .map_err(|e| format!("{}: {e}", target.display()))?;
        std::io::Write::write_all(&mut zip, &bytes).map_err(|e| format!("{}: {e}", target.display()))
    };
    add("horizon-mod.toml".into(), &dir.join("horizon-mod.toml"))?;
    for top in ["assets", "data"] {
        let root = dir.join(top);
        if !root.is_dir() {
            continue;
        }
        for entry in walk(&root)? {
            // The path from the mod's folder (`assets/my_mod/...`), already carrying `top`.
            let name = entry
                .strip_prefix(dir)
                .map_err(|e| e.to_string())?
                .to_string_lossy()
                .replace('\\', "/");
            add(name, &entry)?;
        }
    }
    zip.finish().map_err(|e| format!("{}: {e}", target.display()))?;
    Ok(())
}

fn walk(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
            let entry = entry.map_err(|e| e.to_string())?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

#[cfg(test)]
mod content_mod_tests {
    use super::*;

    fn home(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("hmod-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn hmod(path: &Path, manifest: &str) {
        let file = fs::File::create(path).unwrap();
        let mut zip = zip::ZipWriter::new(file);
        let options = zip::write::SimpleFileOptions::default();
        zip.start_file("horizon-mod.toml", options).unwrap();
        std::io::Write::write_all(&mut zip, manifest.as_bytes()).unwrap();
        zip.start_file("data/ruby/horizon/block/ruby.json", options)
            .unwrap();
        std::io::Write::write_all(&mut zip, br#"{"template": "minecraft:stone"}"#).unwrap();
        zip.finish().unwrap();
    }

    /// A mod carried over from a Java mod starts with comments: it still installs.
    #[test]
    fn a_carried_over_mod_installs_and_turns_on() {
        let dir = home("carried");
        let file = dir.join("dynamictrees.hmod");
        hmod(
            &file,
            "# Carried over by horizon-port-pack, for local use only.\n# Licence: MIT\n\
             id = \"dynamictrees\"\nname = \"Dynamic Trees\"\nversion = \"1.8.0\"\napi = \"0.2\"\n",
        );
        let installed = install_content_mod(&file, &dir).unwrap();
        assert_eq!(installed.id, "dynamictrees");
        assert_eq!(installed.name, "Dynamic Trees");
        assert!(dir.join("horizon-mods/dynamictrees.hmod").is_file());
        assert!(crate::browser::java::is_enabled(&dir, "dynamictrees"));
    }

    /// A jar of a Java mod is a modpack's business, not a mod's.
    #[test]
    fn a_foreign_archive_is_refused() {
        let dir = home("foreign");
        let file = dir.join("create.jar");
        std::fs::write(&file, b"PK not really").unwrap();
        assert!(!is_content_mod(&file));
        let bogus = dir.join("bogus.hmod");
        std::fs::write(&bogus, b"not a zip").unwrap();
        assert!(install_content_mod(&bogus, &dir).is_err());
        assert!(!dir.join("horizon-mods/bogus.hmod").exists());
    }

    /// A folder mod packs into the `.hmod` the game reads.
    #[test]
    fn a_folder_mod_becomes_an_hmod() {
        let dir = home("folder");
        let src = dir.join("my-mod");
        fs::create_dir_all(src.join("assets/my_mod/lang")).unwrap();
        fs::write(
            src.join("horizon-mod.toml"),
            "id = \"my_mod\"\nname = \"My mod\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        fs::write(src.join("assets/my_mod/lang/en_us.json"), b"{}").unwrap();
        let installed = install_content_mod(&src, &dir).unwrap();
        assert_eq!(installed.id, "my_mod");
        let packed = dir.join("horizon-mods/my_mod.hmod");
        assert!(packed.is_file());
        let mut zip = zip::ZipArchive::new(fs::File::open(&packed).unwrap()).unwrap();
        assert!(zip.by_name("assets/my_mod/lang/en_us.json").is_ok());
    }

    /// An id the game would refuse is refused here, where the player can read why.
    #[test]
    fn an_invalid_id_is_refused() {
        let dir = home("badid");
        let file = dir.join("bad.hmod");
        hmod(&file, "id = \"Bad-Id\"\nversion = \"1\"\n");
        let err = install_content_mod(&file, &dir).unwrap_err();
        assert!(err.contains("not a mod id"), "{err}");
    }
}
