//! The decisions of `sopen channel` / `sopen update` against a scripted [`FakeHost`] — a small
//! model of brew (installed kegs, the keg `bin/sopen` resolves into, what the tap offers) with
//! failures injected at any command: no brew, no network. The filesystem halves (a keg reached
//! through a symlinked prefix, `bin/sopen`'s keg), the stderr pass-through and git's
//! environment run for real in a temp dir.

use super::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CELLAR: &str = "/brew/Cellar";
const REPO: &str = "/brew/Library/Taps/2lab-ai/homebrew-tap";
const PREVIEW_V: &str = "2026.11.27.0138";
const PREVIEW_NEXT: &str = "2026.11.28.0101";
const STABLE_V: &str = "0.2.0";

/// One injected failure: the logged call that fails, how, and whether its effect lands before
/// it fails (brew's `uninstall --force` unlinks `bin/sopen` first, then can still fail).
struct Fault {
    call: String,
    failed: Failed,
    effect_first: bool,
}

/// Scripted [`Host`] over a model of brew. Every query and command is logged in order:
/// `cellar`, `list --formula`, `linked`, `version <f>`, `uses <f>`, `info <full name>`, `repo`,
/// `brew <args>`, `git -C <repo> <args>`.
struct FakeHost {
    /// brew answers `brew list --formula`.
    lists: bool,
    /// Installed formula → version.
    versions: RefCell<Vec<(String, String)>>,
    /// The formula whose keg `bin/sopen` resolves into.
    bin_sopen: RefCell<Option<String>>,
    /// `linked` cannot read the link.
    link_unknown: bool,
    /// What the tap offers, formula → the version `install` / `upgrade` give; a formula not
    /// here is not in the tap.
    tap: Vec<(String, String)>,
    /// `in_tap` cannot tell.
    tap_unknown: bool,
    cellar: Option<PathBuf>,
    exe: Option<PathBuf>,
    marker: String,
    /// `formula` → `dependents` answer; a formula with no entry has none.
    dependents: Vec<(String, Option<Vec<String>>)>,
    tap_repo: Option<PathBuf>,
    faults: Vec<Fault>,
    calls: RefCell<Vec<String>>,
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

fn keg_exe(formula: &str, version: &str) -> PathBuf {
    Path::new(CELLAR)
        .join(formula)
        .join(version)
        .join("bin/sopen")
}

impl FakeHost {
    /// brew's sopen-preview, the one that runs and is linked, as after `brew install
    /// 2lab-ai/tap/sopen-preview`; the tap has no stable release yet.
    fn preview() -> Self {
        FakeHost {
            lists: true,
            versions: RefCell::new(vec![("sopen-preview".into(), PREVIEW_V.into())]),
            bin_sopen: RefCell::new(Some("sopen-preview".into())),
            link_unknown: false,
            tap: vec![("sopen-preview".into(), PREVIEW_V.into())],
            tap_unknown: false,
            cellar: Some(PathBuf::from(CELLAR)),
            exe: Some(keg_exe("sopen-preview", PREVIEW_V)),
            marker: "preview".into(),
            dependents: Vec::new(),
            tap_repo: Some(PathBuf::from(REPO)),
            faults: Vec::new(),
            calls: RefCell::new(Vec::new()),
        }
    }

    /// brew's stable sopen, the one that runs and is linked, after the first release.
    fn stable() -> Self {
        FakeHost {
            versions: RefCell::new(vec![("sopen".into(), STABLE_V.into())]),
            bin_sopen: RefCell::new(Some("sopen".into())),
            tap: vec![
                ("sopen".into(), STABLE_V.into()),
                ("sopen-preview".into(), PREVIEW_V.into()),
            ],
            exe: Some(keg_exe("sopen", STABLE_V)),
            marker: "release".into(),
            ..FakeHost::preview()
        }
    }

    /// A branch build: `target/debug/sopen`, marker `dev`, beside an installed sopen-preview.
    fn dev_build() -> Self {
        FakeHost {
            exe: Some(PathBuf::from("/src/sopen/target/debug/sopen")),
            marker: "dev".into(),
            ..FakeHost::preview()
        }
    }

    fn with_installed(self, formula: &str, version: &str) -> Self {
        set_version(&self.versions, formula, Some(version));
        self
    }

    /// `call` fails with `last` as its last stderr line — `effect_first`: after its effect.
    fn failing(mut self, call: &str, last: &str, effect_first: bool) -> Self {
        self.faults.push(Fault {
            call: call.to_string(),
            failed: Failed {
                command: call.to_string(),
                how: "exit 1".to_string(),
                last_line: Some(last.to_string()),
            },
            effect_first,
        });
        self
    }

    fn log(&self, call: impl Into<String>) {
        self.calls.borrow_mut().push(call.into());
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    /// The commands that ran (`brew …`, `git …`), queries left out.
    fn ran(&self) -> Vec<String> {
        self.calls()
            .into_iter()
            .filter(|c| c.starts_with("brew ") || c.starts_with("git "))
            .collect()
    }

    fn version_of(&self, formula: &str) -> Option<String> {
        self.versions
            .borrow()
            .iter()
            .find(|(f, _)| f == formula)
            .map(|(_, v)| v.clone())
    }

    fn offered(&self, formula: &str) -> Option<String> {
        self.tap
            .iter()
            .find(|(f, _)| f == formula)
            .map(|(_, v)| v.clone())
    }

    /// What a brew command changes, as brew does it.
    fn apply(&self, args: &[&str]) {
        let short = |name: &str| name.rsplit('/').next().unwrap_or(name).to_string();
        match args {
            ["install", name] => {
                let f = short(name);
                if let Some(v) = self.offered(&f) {
                    set_version(&self.versions, &f, Some(&v));
                    // Both formulas `link_overwrite "bin/sopen"`.
                    *self.bin_sopen.borrow_mut() = Some(f);
                }
            }
            ["upgrade", name] => {
                let f = short(name);
                let newer = self
                    .offered(&f)
                    .filter(|v| Some(v) != self.version_of(&f).as_ref());
                if let Some(v) = newer {
                    set_version(&self.versions, &f, Some(&v));
                    // The new keg is linked only where the old one was.
                }
            }
            ["uninstall", "--force", f] => {
                set_version(&self.versions, f, None);
                self.unlink(f);
            }
            ["unlink", f] => self.unlink(f),
            ["link", "--overwrite", f] if self.version_of(f).is_some() => {
                *self.bin_sopen.borrow_mut() = Some(f.to_string());
            }
            _ => {}
        }
    }

    /// `brew unlink` removes only the links into the formula's own keg.
    fn unlink(&self, formula: &str) {
        let mut bin = self.bin_sopen.borrow_mut();
        if bin.as_deref() == Some(formula) {
            *bin = None;
        }
    }

    fn fault(&self, call: &str) -> Option<&Fault> {
        self.faults.iter().find(|f| f.call == call)
    }
}

impl Host for FakeHost {
    fn formulae(&self) -> Option<Vec<String>> {
        self.log("list --formula");
        let installed: Vec<String> = self
            .versions
            .borrow()
            .iter()
            .map(|(f, _)| f.clone())
            .collect();
        self.lists.then(|| {
            std::iter::once("ffmpeg".to_string())
                .chain(installed)
                .collect()
        })
    }
    fn linked(&self) -> Option<Vec<String>> {
        self.log("linked");
        if self.link_unknown {
            return None;
        }
        Some(self.bin_sopen.borrow().iter().cloned().collect())
    }
    fn cellar(&self) -> Option<PathBuf> {
        self.log("cellar");
        self.cellar.clone()
    }
    fn exe(&self) -> Option<PathBuf> {
        self.exe.clone()
    }
    fn marker(&self) -> String {
        self.marker.clone()
    }
    fn installed_version(&self, formula: &str) -> Option<String> {
        self.log(format!("version {formula}"));
        self.version_of(formula)
    }
    fn dependents(&self, formula: &str) -> Option<Vec<String>> {
        self.log(format!("uses {formula}"));
        match self.dependents.iter().find(|(f, _)| f == formula) {
            Some((_, answer)) => answer.clone(),
            None => Some(Vec::new()),
        }
    }
    fn in_tap(&self, full_name: &str) -> Option<bool> {
        self.log(format!("info {full_name}"));
        if self.tap_unknown {
            return None;
        }
        let formula = full_name.rsplit('/').next().unwrap_or(full_name);
        Some(self.offered(formula).is_some())
    }
    fn run_brew(&self, args: &[&str]) -> std::result::Result<(), Failed> {
        let call = format!("brew {}", args.join(" "));
        self.log(call.clone());
        match self.fault(&call) {
            Some(fault) => {
                if fault.effect_first {
                    self.apply(args);
                }
                Err(fault.failed.clone())
            }
            None => {
                self.apply(args);
                Ok(())
            }
        }
    }
    fn tap_repo(&self) -> Option<PathBuf> {
        self.log("repo");
        self.tap_repo.clone()
    }
    fn run_git(&self, repo: &Path, args: &[&str]) -> std::result::Result<(), Failed> {
        let call = format!("git -C {} {}", repo.display(), args.join(" "));
        self.log(call.clone());
        match self.fault(&call) {
            Some(fault) => Err(fault.failed.clone()),
            None => Ok(()),
        }
    }
}

/// Sets `formula`'s version (`None`: removes it).
fn set_version(versions: &RefCell<Vec<(String, String)>>, formula: &str, version: Option<&str>) {
    let mut v = versions.borrow_mut();
    v.retain(|(f, _)| f != formula);
    if let Some(version) = version {
        v.push((formula.to_string(), version.to_string()));
    }
}

/// `bin/sopen` is a sopen on PATH: it resolves into the keg of an installed formula.
fn assert_sopen_on_path(host: &FakeHost, formula: &str) {
    let bin = host.bin_sopen.borrow().clone();
    assert_eq!(bin.as_deref(), Some(formula), "calls: {:?}", host.calls());
    assert!(
        host.version_of(formula).is_some(),
        "{formula} is not installed"
    );
}

/// What a run printed: stdout, stderr and its result.
struct Said {
    out: String,
    err: String,
    result: Result<()>,
}

fn said(run: impl FnOnce(&mut dyn Write, &mut dyn Write) -> Result<()>) -> Said {
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let result = run(&mut out, &mut err);
    Said {
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
        result,
    }
}

fn error_of(said: &Said) -> String {
    format!("{:#}", said.result.as_ref().expect_err("an error"))
}

const NO_STABLE: &str = "the stable channel has no release yet (its first v* release is the \
                         owner's step); staying on preview";

// --- the build marker, the channel's names --------------------------------------------------

#[test]
fn the_build_marker_names_a_channel_or_none() {
    assert_eq!(marker_channel("preview"), Some(Channel::Preview));
    // release.yml builds with `SOPEN_BUILD_CHANNEL: release`.
    assert_eq!(marker_channel("release"), Some(Channel::Stable));
    assert_eq!(marker_channel("stable"), Some(Channel::Stable));
    assert_eq!(marker_channel("dev"), None);
    assert_eq!(marker_channel(""), None);
    assert_eq!(Channel::Preview.formula(), "sopen-preview");
    assert_eq!(Channel::Stable.full_name(), "2lab-ai/tap/sopen");
    assert_eq!(Channel::from_word("stable"), Some(Channel::Stable));
    assert_eq!(Channel::from_word("beta"), None);
    assert_eq!(Channel::Stable.other(), Channel::Preview);
}

// --- installed by brew? ----------------------------------------------------------------------

#[test]
fn a_keg_is_cellar_formula_version_bin_sopen() {
    let shape = |p: &str| keg_shape(Path::new(p));
    assert_eq!(
        shape("/opt/homebrew/Cellar/sopen-preview/2026.11.27.0138/bin/sopen"),
        Some((PathBuf::from("/opt/homebrew/Cellar"), Channel::Preview))
    );
    assert_eq!(
        shape("/home/linuxbrew/.linuxbrew/Cellar/sopen/0.2.0/bin/sopen"),
        Some((
            PathBuf::from("/home/linuxbrew/.linuxbrew/Cellar"),
            Channel::Stable
        ))
    );
    for not in [
        "/src/sopen/target/debug/sopen",
        "/usr/local/bin/sopen",
        "/opt/homebrew/Cellar/llmux/0.3.0/bin/sopen",
        "/opt/homebrew/Cellar/sopen-preview/2026.11.27.0138/bin/llmux",
        "/opt/homebrew/Cellar/sopen-preview/2026.11.27.0138/libexec/sopen",
    ] {
        assert_eq!(shape(not), None, "{not}");
    }
}

#[test]
fn installed_by_brew_needs_a_keg_in_brews_own_cellar() {
    let exe = keg_exe("sopen-preview", PREVIEW_V);
    let here = || Some(PathBuf::from(CELLAR));
    assert_eq!(
        install_from(Some(&exe), here, "preview"),
        Install::Keg(Channel::Preview)
    );
    // Another brew's Cellar (two prefixes on one Mac) is not this brew's keg.
    assert_eq!(
        install_from(
            Some(&exe),
            || Some(PathBuf::from("/other/Cellar")),
            "preview"
        ),
        Install::Not(NotBrew::OutsideKeg)
    );
    assert_eq!(
        install_from(Some(&exe), || None, "preview"),
        Install::Not(NotBrew::BrewUnavailable)
    );
    // Not keg-shaped: brew is never asked.
    let debug = PathBuf::from("/src/sopen/target/debug/sopen");
    let never = || -> Option<PathBuf> { panic!("brew asked for a binary that is no keg") };
    assert_eq!(
        install_from(Some(&debug), never, "dev"),
        Install::Not(NotBrew::DevBuild)
    );
    assert_eq!(
        install_from(Some(&debug), never, "preview"),
        Install::Not(NotBrew::OutsideKeg)
    );
    assert_eq!(
        install_from(None, never, "preview"),
        Install::Not(NotBrew::ExeUnknown)
    );
}

/// Brew's binaries are built `preview` or `release`: a `dev` build is never brew's, even copied
/// into a keg's place, and brew is not asked.
#[test]
fn a_dev_build_inside_a_keg_is_still_a_dev_build() {
    let exe = keg_exe("sopen-preview", PREVIEW_V);
    let never = || -> Option<PathBuf> { panic!("brew asked about a dev build") };
    assert_eq!(
        install_from(Some(&exe), never, "dev"),
        Install::Not(NotBrew::DevBuild)
    );
    let mut host = FakeHost::dev_build();
    host.exe = Some(exe);
    let s = said(|o, e| update(&host, o, e));
    assert!(
        error_of(&s).contains("(it is a dev build)"),
        "{}",
        error_of(&s)
    );
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

#[test]
fn a_keg_reached_through_a_symlinked_prefix_is_still_brews() {
    let tmp = tempfile::tempdir().unwrap();
    let real = tmp.path().join("real");
    let keg_bin = real
        .join("Cellar/sopen-preview")
        .join(PREVIEW_V)
        .join("bin");
    std::fs::create_dir_all(&keg_bin).unwrap();
    std::fs::write(keg_bin.join("sopen"), b"").unwrap();
    std::fs::create_dir_all(real.join("bin")).unwrap();
    let relative = Path::new("../Cellar/sopen-preview")
        .join(PREVIEW_V)
        .join("bin/sopen");
    std::os::unix::fs::symlink(&relative, real.join("bin/sopen")).unwrap();
    let prefix = tmp.path().join("prefix");
    std::os::unix::fs::symlink(&real, &prefix).unwrap();
    // As `current_exe` may give it (the link typed) and `brew --cellar` prints it.
    let exe = prefix.join("bin/sopen");
    let cellar = prefix.join("Cellar");
    assert_eq!(
        install_from(Some(&exe), || Some(cellar.clone()), "preview"),
        Install::Keg(Channel::Preview)
    );
}

#[test]
fn bin_sopen_is_linked_to_the_keg_it_resolves_into() {
    let tmp = tempfile::tempdir().unwrap();
    let cellar = tmp.path().join("Cellar");
    for (formula, version) in [("sopen-preview", PREVIEW_V), ("sopen", STABLE_V)] {
        let bin = cellar.join(formula).join(version).join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("sopen"), b"").unwrap();
    }
    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let link = bin.join("sopen");
    let point = |to: &Path| {
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(to, &link).unwrap();
    };
    assert_eq!(linked_of(&link, &cellar), Some(Vec::new()), "no bin/sopen");
    point(
        &cellar
            .join("sopen-preview")
            .join(PREVIEW_V)
            .join("bin/sopen"),
    );
    assert_eq!(linked_of(&link, &cellar), Some(strs(&["sopen-preview"])));
    point(&cellar.join("sopen").join(STABLE_V).join("bin/sopen"));
    assert_eq!(linked_of(&link, &cellar), Some(strs(&["sopen"])));
    // A link whose keg is gone, or to anything else, is no formula's.
    point(&cellar.join("sopen/0.1.0/bin/sopen"));
    assert_eq!(linked_of(&link, &cellar), Some(Vec::new()));
    let elsewhere = tmp.path().join("elsewhere");
    std::fs::write(&elsewhere, b"").unwrap();
    point(&elsewhere);
    assert_eq!(linked_of(&link, &cellar), Some(Vec::new()));
}

#[test]
fn the_refusal_says_why_and_how_to_install() {
    assert_eq!(
        refusal(NotBrew::DevBuild),
        "this sopen was not installed by brew (it is a dev build); install it with: brew \
         install 2lab-ai/tap/sopen-preview"
    );
    for why in [NotBrew::ExeUnknown, NotBrew::OutsideKeg] {
        let line = refusal(why);
        assert!(
            line.starts_with("this sopen was not installed by brew ("),
            "{line}"
        );
        assert!(
            line.ends_with("); install it with: brew install 2lab-ai/tap/sopen-preview"),
            "{line}"
        );
    }
    // A keg brew cannot be asked about may well be brew's: it is not said otherwise.
    assert_eq!(
        refusal(NotBrew::BrewUnavailable),
        "brew is not available here (`brew --cellar` did not answer); sopen updates and \
         switches only through brew"
    );
}

// --- channel detection ----------------------------------------------------------------------

#[test]
fn only_preview_installed_is_the_preview_channel() {
    let keg = Install::Keg(Channel::Preview);
    let formulae = strs(&["ffmpeg", "sopen-preview"]);
    let d = detect(keg, Some(&formulae), None, "preview").unwrap();
    // `sopen-preview` is not `sopen`: names match exactly.
    assert_eq!(d.channel, Channel::Preview);
    assert_eq!(d.source, Source::Brew);
    assert_eq!(d.warning, None);
}

#[test]
fn only_stable_installed_is_the_stable_channel() {
    let keg = Install::Keg(Channel::Stable);
    let d = detect(keg, Some(&strs(&["sopen"])), None, "release").unwrap();
    assert_eq!(
        (d.channel, d.source, d.warning),
        (Channel::Stable, Source::Brew, None)
    );
}

#[test]
fn both_installed_the_linked_one_decides_with_a_warning() {
    let both = strs(&["sopen", "sopen-preview"]);
    let keg = Install::Keg(Channel::Preview);
    let preview = detect(keg, Some(&both), Some(&strs(&["sopen-preview"])), "preview").unwrap();
    assert_eq!(preview.channel, Channel::Preview);
    let warning = preview.warning.unwrap();
    assert!(
        warning.contains("both sopen and sopen-preview are installed"),
        "{warning}"
    );
    assert!(
        warning.contains("bin/sopen is sopen-preview's"),
        "{warning}"
    );
    assert!(
        warning.contains("`sopen channel preview` removes sopen,"),
        "{warning}"
    );
    assert!(
        warning.contains("`sopen channel stable` removes sopen-preview"),
        "{warning}"
    );
    let stable = detect(keg, Some(&both), Some(&strs(&["sopen"])), "preview").unwrap();
    assert_eq!(stable.channel, Channel::Stable);
    assert!(stable.warning.unwrap().contains("bin/sopen is sopen's"));
    // Neither linked, or unknown: preview wins, and the warning says the link did not decide.
    for linked in [None, Some(Vec::new())] {
        let d = detect(keg, Some(&both), linked.as_deref(), "preview").unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert!(d.warning.unwrap().contains("preview wins"));
    }
}

#[test]
fn neither_formula_falls_back_to_the_build_marker() {
    let jq = strs(&["jq"]);
    let d = detect(Install::Keg(Channel::Preview), Some(&jq), None, "preview").unwrap();
    assert_eq!(d.source, Source::Marker(MarkerWhy::NotInstalledByBrew));
    let outside = Install::Not(NotBrew::OutsideKeg);
    let d = detect(outside, Some(&jq), None, "release").unwrap();
    assert_eq!(d.channel, Channel::Stable);
    assert_eq!(d.source, Source::Marker(MarkerWhy::NotInstalledByBrew));
    assert_eq!(d.source.via(), "build marker (not installed by brew)");
}

#[test]
fn brew_unavailable_falls_back_to_the_build_marker() {
    for install in [
        Install::Keg(Channel::Preview),
        Install::Not(NotBrew::BrewUnavailable),
    ] {
        let d = detect(install, None, None, "preview").unwrap();
        assert_eq!(d.channel, Channel::Preview);
        assert_eq!(d.source, Source::Marker(MarkerWhy::BrewUnavailable));
        assert_eq!(d.source.via(), "build marker (brew unavailable)");
    }
    assert_eq!(Source::Brew.via(), "brew");
}

/// Not llmux's rule: a binary that is no keg reports its own marker, whatever brew has
/// installed beside it — `sopen update` would not update it.
#[test]
fn a_binary_brew_did_not_install_reports_its_marker_not_brews_formula() {
    let installed = strs(&["sopen-preview"]);
    let outside = Install::Not(NotBrew::OutsideKeg);
    let d = detect(outside, Some(&installed), Some(&installed), "release").unwrap();
    assert_eq!(d.channel, Channel::Stable);
    assert_eq!(d.source, Source::Marker(MarkerWhy::NotInstalledByBrew));
}

#[test]
fn a_dev_build_has_no_channel() {
    let installed = strs(&["sopen-preview"]);
    let dev = Install::Not(NotBrew::DevBuild);
    let e = detect(dev, Some(&installed), None, "dev").unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        "this sopen has no channel: it was not installed by brew and its build marker is dev"
    );
    let e = detect(Install::Not(NotBrew::BrewUnavailable), None, None, "dev").unwrap_err();
    assert_eq!(
        format!("{e:#}"),
        "this sopen has no channel: brew is not available and its build marker is dev"
    );
}

// --- what brew and git print ----------------------------------------------------------------

#[test]
fn brew_info_on_a_formula_the_tap_lacks_is_a_no() {
    // `brew info --formula 2lab-ai/tap/sopen` on Homebrew 7.0, 2026-11-27, before any v* tag
    // (its "Did you mean" list of the tap's other formulas left out).
    let captured = "Error: No available formula with the name \"2lab-ai/tap/sopen\".\n\
                    ==> Searching for a previously deleted formula (in the last month)...\n\
                    Error: No previously deleted formula found.\n";
    assert_eq!(tap_answer(false, captured), Some(false));
    assert_eq!(tap_answer(true, ""), Some(true));
    assert_eq!(
        tap_answer(false, "Error: Permission denied @ rb_sysopen\n"),
        None
    );
}

#[test]
fn the_last_line_is_the_last_one_with_text_without_colour() {
    let stderr = b"==> Upgrading 2lab-ai/tap/sopen-preview\n\x1b[31mError:\x1b[0m boom\n\n  \n";
    assert_eq!(last_line(stderr), Some("Error: boom".into()));
    assert_eq!(last_line(b"10%\r50%\r100%\r"), Some("100%".into()));
    assert_eq!(last_line(b"\n \n"), None);
    assert_eq!(
        last_line(b"no newline at the end"),
        Some("no newline at the end".into())
    );
}

#[test]
fn a_failure_names_the_command_its_exit_and_last_line() {
    let failed = Failed {
        command: "brew upgrade 2lab-ai/tap/sopen-preview".into(),
        how: "exit 1".into(),
        last_line: Some("Error: sopen-preview is pinned".into()),
    };
    assert_eq!(
        failed.to_string(),
        "`brew upgrade 2lab-ai/tap/sopen-preview` failed (exit 1): Error: sopen-preview is pinned"
    );
    let silent = Failed {
        last_line: None,
        ..failed
    };
    assert_eq!(
        silent.to_string(),
        "`brew upgrade 2lab-ai/tap/sopen-preview` failed (exit 1)"
    );
}

#[test]
fn versions_come_from_the_kegs_in_use_or_the_newest_listed() {
    // `brew list --versions` lists kegs in no meaningful order.
    let listing = "sopen-preview 2026.11.28.0101 2026.11.27.0138 2026.11.9.0000\n";
    assert_eq!(
        highest_listed_version(listing),
        Some("2026.11.28.0101".into())
    );
    assert_eq!(
        compare_versions("0.2.10", "0.2.9"),
        std::cmp::Ordering::Greater
    );
    assert_eq!(
        compare_versions("0.2.0_1", "0.2.0"),
        std::cmp::Ordering::Greater
    );
    let keg = PathBuf::from("/brew/Cellar/sopen-preview/2026.11.27.0138");
    let never = || -> Option<String> { panic!("listed though the opt link answered") };
    assert_eq!(
        formula_version("sopen-preview", Some(&keg), never),
        Some(PREVIEW_V.into())
    );
    // An opt link into another rack is no answer: the listing decides.
    assert_eq!(
        formula_version("sopen", Some(&keg), || Some("sopen 0.2.0\n".into())),
        Some(STABLE_V.into())
    );
    assert_eq!(formula_version("sopen", None, || None), None);
}

// --- running brew and git -------------------------------------------------------------------

/// A shared buffer, for what the pass-through copied.
#[derive(Clone, Default)]
struct Shared(Arc<Mutex<Vec<u8>>>);

impl Write for Shared {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn sh(script: &str) -> Command {
    let mut command = Command::new("/bin/sh");
    command.args(["-c", script]).stdout(Stdio::null());
    command
}

#[test]
fn the_pass_through_copies_stderr_and_names_the_exit_and_last_line() {
    let copy = Shared::default();
    let failed = run_passing_stderr(
        sh("printf 'a\\nb: boom\\n' >&2; exit 3"),
        "sh".into(),
        Box::new(copy.clone()),
        DRAIN,
    )
    .unwrap_err();
    assert_eq!(failed.how, "exit 3");
    assert_eq!(failed.last_line.as_deref(), Some("b: boom"));
    assert_eq!(failed.to_string(), "`sh` failed (exit 3): b: boom");
    assert_eq!(*copy.0.lock().unwrap(), b"a\nb: boom\n");
    let ok = run_passing_stderr(sh("exit 0"), "sh".into(), Box::new(copy.clone()), DRAIN);
    assert_eq!(ok, Ok(()));
    let missing = Command::new("/nonexistent/brew");
    let failed = run_passing_stderr(missing, "brew".into(), Box::new(copy), DRAIN).unwrap_err();
    assert!(failed.how.starts_with("could not start it: "), "{failed}");
}

/// A descendant that keeps the command's stderr open must not hold sopen past the command's
/// exit by more than [`DRAIN`].
#[test]
fn a_child_left_holding_stderr_does_not_hold_the_command() {
    let tmp = tempfile::tempdir().unwrap();
    let pid_file = tmp.path().join("pid");
    let mut command = sh("sleep 30 & echo $! > \"$0\"; printf 'a\\nb: boom\\n' >&2; exit 3");
    command.arg(&pid_file);
    let started = Instant::now();
    let result = run_passing_stderr(command, "sh".into(), Box::new(std::io::sink()), DRAIN);
    let took = started.elapsed();
    if let Ok(pid) = std::fs::read_to_string(&pid_file) {
        if let Ok(pid) = pid.trim().parse::<libc::pid_t>() {
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }
    let failed = result.unwrap_err();
    assert!(took < DRAIN + Duration::from_secs(8), "took {took:?}");
    assert_eq!(failed.how, "exit 3");
    assert_eq!(failed.last_line.as_deref(), Some("b: boom"));
}

#[test]
fn git_never_prompts_and_never_climbs_out_of_the_tap() {
    let tmp = tempfile::tempdir().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("Taps");
    let tap = parent.join("homebrew-tap");
    std::fs::create_dir_all(&tap).unwrap();
    let command = git_command(&tap, &["fetch", "-q", TAP_URL, TAP_BRANCH]);
    let envs: Vec<(String, String)> = command
        .get_envs()
        .filter_map(|(k, v)| Some((k.to_str()?.to_string(), v?.to_str()?.to_string())))
        .collect();
    assert!(
        envs.contains(&("GIT_TERMINAL_PROMPT".into(), "0".into())),
        "{envs:?}"
    );
    assert!(
        envs.contains(&(
            "GIT_CEILING_DIRECTORIES".into(),
            parent.to_str().unwrap().into()
        )),
        "{envs:?}"
    );
    // A tap folder that is no repository, inside one: git refuses rather than run there.
    let Some(git) = crate::git::history::fixture::git() else {
        return;
    };
    let init = Command::new(&git)
        .args(["init", "-q"])
        .arg(&parent)
        .output()
        .unwrap();
    assert!(init.status.success(), "{init:?}");
    let mut probe = git_command(&tap, &["rev-parse", "--git-dir"]);
    // git's own words, whatever this machine's language.
    probe.env("PATH", git.parent().unwrap()).env("LC_ALL", "C");
    let out = probe.output().unwrap();
    assert!(
        !out.status.success(),
        "git found {}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("not a git repository"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- plans ------------------------------------------------------------------------------------

#[test]
fn the_install_step_installs_or_upgrades_the_target() {
    assert_eq!(
        install_step(Channel::Stable, false),
        BrewCmd(strs(&["install", "2lab-ai/tap/sopen"]))
    );
    assert_eq!(
        install_step(Channel::Preview, true),
        BrewCmd(strs(&["upgrade", "2lab-ai/tap/sopen-preview"]))
    );
}

/// `bin/sopen` already the target's: nothing unlinks it; the other formula goes last.
#[test]
fn converging_relinks_only_when_bin_sopen_is_not_the_targets_and_retires_last() {
    let p = Channel::Preview;
    assert_eq!(
        converge_plan(p, Some(&strs(&["sopen-preview"])), &[]),
        vec![BrewCmd(strs(&["uninstall", "--force", "sopen"]))]
    );
    let relink_then_retire = vec![
        BrewCmd(strs(&["unlink", "sopen-preview"])),
        BrewCmd(strs(&["link", "--overwrite", "sopen-preview"])),
        BrewCmd(strs(&["uninstall", "--force", "sopen"])),
    ];
    assert_eq!(
        converge_plan(p, Some(&strs(&["sopen"])), &[]),
        relink_then_retire
    );
    assert_eq!(converge_plan(p, Some(&[]), &[]), relink_then_retire);
    // Unknown link: relinked, to be certain.
    assert_eq!(converge_plan(p, None, &[]), relink_then_retire);
    let needed = strs(&["someone/tap/sopen-plugin"]);
    assert_eq!(
        converge_plan(Channel::Stable, Some(&strs(&["sopen"])), &needed),
        vec![BrewCmd(strs(&["unlink", "sopen-preview"]))]
    );
    assert_eq!(blocking_dependents(&needed), strs(&["sopen-plugin"]));
}

#[test]
fn the_same_channel_is_nothing_a_relink_or_a_consolidation() {
    let p = Channel::Preview;
    let linked = strs(&["sopen-preview"]);
    let nothing = strs(&[]);
    assert_eq!(
        same_channel_decision(p, true, false, &[], Some(&linked)),
        SameChannel::NothingToDo
    );
    // Link state unknown: no reason to touch brew.
    assert_eq!(
        same_channel_decision(p, true, false, &[], None),
        SameChannel::NothingToDo
    );
    assert_eq!(
        same_channel_decision(p, true, false, &[], Some(&nothing)),
        SameChannel::Relink
    );
    assert_eq!(
        same_channel_decision(p, true, true, &[], Some(&linked)),
        SameChannel::Consolidate { kept: None }
    );
    let needed = strs(&["someone/tap/sopen-plugin"]);
    let kept = KeptFormula {
        formula: "sopen".into(),
        required_by: strs(&["sopen-plugin"]),
    };
    assert_eq!(
        same_channel_decision(p, true, true, &needed, Some(&linked)),
        SameChannel::KeptUnlinked(kept.clone())
    );
    assert_eq!(
        same_channel_decision(p, true, true, &needed, Some(&nothing)),
        SameChannel::Consolidate { kept: Some(kept) }
    );
}

// --- `sopen channel` --------------------------------------------------------------------------

#[test]
fn sopen_channel_prints_the_channel_via_brew() {
    let host = FakeHost::preview();
    let s = said(|o, e| show(&host, o, e));
    s.result.unwrap();
    assert_eq!(s.out, "channel: preview (via brew)\n");
    assert_eq!(s.err, "");
    assert_eq!(host.calls(), strs(&["cellar", "list --formula", "linked"]));
}

#[test]
fn sopen_channel_warns_when_both_are_installed() {
    let host = FakeHost::stable().with_installed("sopen-preview", PREVIEW_V);
    let s = said(|o, e| show(&host, o, e));
    s.result.unwrap();
    assert_eq!(s.out, "channel: stable (via brew)\n");
    assert_eq!(s.err.lines().count(), 1, "{}", s.err);
    assert!(
        s.err.starts_with("sopen: both sopen and sopen-preview"),
        "{}",
        s.err
    );
}

#[test]
fn sopen_channel_with_brew_unavailable_says_build_marker() {
    let mut host = FakeHost::preview();
    host.cellar = None;
    host.lists = false;
    let s = said(|o, e| show(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        s.out,
        "channel: preview (via build marker (brew unavailable))\n"
    );
}

#[test]
fn sopen_channel_on_a_dev_build_is_an_error_and_asks_brew_nothing() {
    let host = FakeHost::dev_build();
    let s = said(|o, e| show(&host, o, e));
    assert_eq!(
        error_of(&s),
        "this sopen has no channel: it was not installed by brew and its build marker is dev"
    );
    assert_eq!(s.out, "");
    assert!(host.calls().is_empty(), "{:?}", host.calls());
}

// --- `sopen channel stable|preview` ----------------------------------------------------------

#[test]
fn stable_without_a_release_is_refused_with_nothing_installed() {
    let host = FakeHost::preview();
    let s = said(|o, e| switch(&host, Channel::Stable, o, e));
    assert_eq!(error_of(&s), NO_STABLE);
    assert_eq!(s.out, "");
    // The tap refreshed and asked, nothing else: no install, uninstall or link, and not even
    // what depends on sopen-preview.
    assert_eq!(
        host.calls(),
        strs(&[
            "cellar",
            "list --formula",
            "linked",
            "brew update",
            "info 2lab-ai/tap/sopen"
        ])
    );
    assert_sopen_on_path(&host, "sopen-preview");
}

#[test]
fn a_switch_to_preview_from_stable_installs_then_retires_the_other_last() {
    let host = FakeHost::stable();
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    // `brew install` linked sopen-preview over bin/sopen: no unlink, no relink.
    assert_eq!(
        host.ran(),
        strs(&[
            "brew update",
            "brew install 2lab-ai/tap/sopen-preview",
            "brew uninstall --force sopen",
        ])
    );
    let calls = host.calls();
    let uses = calls.iter().position(|c| c == "uses sopen").unwrap();
    let install = calls
        .iter()
        .position(|c| c.starts_with("brew install"))
        .unwrap();
    assert!(
        uses < install,
        "dependents asked before anything changed: {calls:?}"
    );
    assert_eq!(
        s.out,
        format!(
            "switching channel: stable → preview…\n\
             switched to the preview channel: sopen {STABLE_V} → sopen-preview {PREVIEW_V}\n\
             {WINDOWS_KEEP_OLD}\n"
        )
    );
    assert_eq!(s.err, "");
    assert_sopen_on_path(&host, "sopen-preview");
    assert_eq!(host.version_of("sopen"), None);
}

#[test]
fn a_switch_to_stable_once_released_names_old_and_new() {
    let mut host = FakeHost::preview();
    host.tap.push(("sopen".into(), STABLE_V.into()));
    let s = said(|o, e| switch(&host, Channel::Stable, o, e));
    s.result.unwrap();
    assert!(
        s.out
            .contains(&format!("sopen-preview {PREVIEW_V} → sopen {STABLE_V}")),
        "{}",
        s.out
    );
    assert_sopen_on_path(&host, "sopen");
}

#[test]
fn a_switch_to_the_same_channel_is_one_line_and_runs_nothing() {
    let host = FakeHost::preview();
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(s.out, "already on the preview channel — nothing to do\n");
    assert!(host.ran().is_empty(), "{:?}", host.calls());
    // `bin/sopen` not its binary: relinked, still no tap refresh.
    let host = FakeHost::preview();
    *host.bin_sopen.borrow_mut() = None;
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(
        host.ran(),
        strs(&[
            "brew unlink sopen-preview",
            "brew link --overwrite sopen-preview"
        ])
    );
    assert!(
        s.out
            .ends_with("relinked sopen-preview — `bin/sopen` is its binary again\n"),
        "{}",
        s.out
    );
    assert_sopen_on_path(&host, "sopen-preview");
}

/// Both installed and `bin/sopen` neither's (or unreadable): preview wins, and `sopen channel
/// preview` links it first and retires the stray last, so a failure at the retire leaves the
/// target on PATH.
#[test]
fn the_same_channel_with_both_installed_relinks_first_and_retires_last() {
    for link_unknown in [false, true] {
        let mut host = FakeHost::preview()
            .with_installed("sopen", STABLE_V)
            .failing(
                "brew uninstall --force sopen",
                "Error: Permission denied @ unlink_internal",
                true,
            );
        *host.bin_sopen.borrow_mut() = None;
        host.link_unknown = link_unknown;
        let s = said(|o, e| switch(&host, Channel::Preview, o, e));
        assert_eq!(
            host.ran(),
            strs(&[
                "brew unlink sopen-preview",
                "brew link --overwrite sopen-preview",
                "brew uninstall --force sopen",
            ])
        );
        assert!(
            s.out.starts_with(
                "already on the preview channel — retiring the other formula, sopen…\n"
            ),
            "{}",
            s.out
        );
        let e = error_of(&s);
        assert!(
            e.ends_with("to make bin/sopen the preview one: brew link --overwrite sopen-preview"),
            "{e}"
        );
        assert_sopen_on_path(&host, "sopen-preview");
    }
    // Without the failure: converged, the stray gone.
    let host = FakeHost::preview().with_installed("sopen", STABLE_V);
    *host.bin_sopen.borrow_mut() = None;
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert!(s.out.ends_with("removed sopen\n"), "{}", s.out);
    assert_sopen_on_path(&host, "sopen-preview");
    assert_eq!(host.version_of("sopen"), None);
}

#[test]
fn a_failed_install_stops_the_switch_with_the_old_channel_whole() {
    let host = FakeHost::stable().failing(
        "brew install 2lab-ai/tap/sopen-preview",
        "Error: Failed to download resource \"sopen-preview\"",
        false,
    );
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert_eq!(
        error_of(&s),
        "`brew install 2lab-ai/tap/sopen-preview` failed (exit 1): Error: Failed to download \
         resource \"sopen-preview\"; nothing was removed — installed: sopen 0.2.0; bin/sopen \
         is sopen's"
    );
    assert_eq!(
        host.ran(),
        strs(&["brew update", "brew install 2lab-ai/tap/sopen-preview"])
    );
    assert_sopen_on_path(&host, "sopen");
}

#[test]
fn a_failed_upgrade_of_an_installed_target_removes_nothing() {
    let mut host = FakeHost::stable()
        .with_installed("sopen-preview", PREVIEW_V)
        .failing(
            "brew upgrade 2lab-ai/tap/sopen-preview",
            "Error: sopen-preview is pinned. You must unpin it to upgrade.",
            false,
        );
    host.tap = vec![
        ("sopen".into(), STABLE_V.into()),
        ("sopen-preview".into(), PREVIEW_NEXT.into()),
    ];
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    let e = error_of(&s);
    assert!(e.contains("; nothing was removed — installed: "), "{e}");
    assert!(!e.contains("brew link"), "{e}");
    assert_eq!(
        host.ran(),
        strs(&["brew update", "brew upgrade 2lab-ai/tap/sopen-preview"])
    );
    assert_sopen_on_path(&host, "sopen");
}

/// The retire is the last step: whether `uninstall --force` fails before or after unlinking
/// its own keg, `bin/sopen` is already the target's.
#[test]
fn a_failed_retire_leaves_the_target_on_path_and_says_the_way_back() {
    for effect_first in [false, true] {
        let host = FakeHost::stable().failing(
            "brew uninstall --force sopen",
            "Error: Permission denied @ unlink_internal",
            effect_first,
        );
        let s = said(|o, e| switch(&host, Channel::Preview, o, e));
        assert_sopen_on_path(&host, "sopen-preview");
        let installed = if effect_first {
            format!("sopen-preview {PREVIEW_V}")
        } else {
            format!("sopen-preview {PREVIEW_V}, sopen {STABLE_V}")
        };
        assert_eq!(
            error_of(&s),
            format!(
                "`brew uninstall --force sopen` failed (exit 1): Error: Permission denied @ \
                 unlink_internal; it stopped part way — installed: {installed}; bin/sopen is \
                 sopen-preview's; to make bin/sopen the preview one: brew link --overwrite \
                 sopen-preview"
            )
        );
    }
}

/// The target already current and unlinked (kept for its dependents on an earlier switch):
/// `upgrade` leaves it unlinked, so it is relinked — and any step of that may fail with a sopen
/// still on PATH, the old one until the target's link lands.
#[test]
fn a_failed_relink_leaves_a_sopen_on_path() {
    for (call, effect_first, on_path) in [
        ("brew unlink sopen-preview", false, "sopen"),
        ("brew unlink sopen-preview", true, "sopen"),
        ("brew link --overwrite sopen-preview", false, "sopen"),
        ("brew link --overwrite sopen-preview", true, "sopen-preview"),
    ] {
        let host = FakeHost::stable()
            .with_installed("sopen-preview", PREVIEW_V)
            .failing(call, "Error: boom", effect_first);
        let s = said(|o, e| switch(&host, Channel::Preview, o, e));
        assert_eq!(
            host.ran()[..2],
            strs(&["brew update", "brew upgrade 2lab-ai/tap/sopen-preview"]),
            "{call}"
        );
        assert!(
            !host.ran().iter().any(|c| c.contains("uninstall")),
            "{call}: {:?}",
            host.ran()
        );
        assert_sopen_on_path(&host, on_path);
        let e = error_of(&s);
        assert!(
            e.ends_with("to make bin/sopen the preview one: brew link --overwrite sopen-preview"),
            "{call}: {e}"
        );
    }
    // No failure: relinked, then the other retired.
    let host = FakeHost::stable().with_installed("sopen-preview", PREVIEW_V);
    said(|o, e| switch(&host, Channel::Preview, o, e))
        .result
        .unwrap();
    assert_eq!(
        host.ran(),
        strs(&[
            "brew update",
            "brew upgrade 2lab-ai/tap/sopen-preview",
            "brew unlink sopen-preview",
            "brew link --overwrite sopen-preview",
            "brew uninstall --force sopen",
        ])
    );
    assert_sopen_on_path(&host, "sopen-preview");
}

#[test]
fn a_formula_others_need_is_kept_unlinked_and_said_so() {
    let mut host = FakeHost::stable();
    host.dependents = vec![("sopen".into(), Some(strs(&["someone/tap/sopen-plugin"])))];
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(
        host.ran(),
        strs(&[
            "brew update",
            "brew install 2lab-ai/tap/sopen-preview",
            "brew unlink sopen",
        ])
    );
    assert!(
        s.out
            .contains("kept sopen installed (required by sopen-plugin) — unlinked\n"),
        "{}",
        s.out
    );
    assert_sopen_on_path(&host, "sopen-preview");
    // An unanswerable dependents query changes nothing.
    let mut host = FakeHost::stable();
    host.dependents = vec![("sopen".into(), None)];
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert!(
        error_of(&s).contains("nothing was changed"),
        "{}",
        error_of(&s)
    );
    assert_eq!(host.ran(), strs(&["brew update"]));
}

#[test]
fn a_target_brew_cannot_find_or_cannot_say_is_refused_before_anything_changes() {
    let mut host = FakeHost::stable();
    host.tap_unknown = true;
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert_eq!(
        error_of(&s),
        "brew could not say whether 2lab-ai/tap/sopen-preview is in its tap (`brew info \
         --formula 2lab-ai/tap/sopen-preview`); nothing was changed"
    );
    assert_eq!(host.ran(), strs(&["brew update"]));
    let mut host = FakeHost::stable();
    host.tap.retain(|(f, _)| f != "sopen-preview");
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert_eq!(
        error_of(&s),
        "2lab-ai/tap/sopen-preview is not in 2lab-ai/tap; staying on stable"
    );
    assert_eq!(host.ran(), strs(&["brew update"]));
    assert_sopen_on_path(&host, "sopen");
}

// --- `sopen update` ---------------------------------------------------------------------------

fn offering_next(mut host: FakeHost) -> FakeHost {
    host.tap = vec![("sopen-preview".into(), PREVIEW_NEXT.into())];
    host
}

#[test]
fn an_update_that_changed_the_version_says_before_after_and_the_windows_line() {
    let host = offering_next(FakeHost::preview());
    let s = said(|o, e| update(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        host.ran(),
        strs(&["brew update", "brew upgrade 2lab-ai/tap/sopen-preview"])
    );
    assert_eq!(
        s.out,
        format!(
            "updating sopen-preview on the preview channel…\n\
             sopen-preview {PREVIEW_V} → {PREVIEW_NEXT}\n\
             {WINDOWS_KEEP_OLD}\n"
        )
    );
}

#[test]
fn an_update_with_nothing_newer_is_up_to_date() {
    let host = FakeHost::preview();
    let s = said(|o, e| update(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        s.out,
        format!(
            "updating sopen-preview on the preview channel…\n\
             sopen-preview is up to date ({PREVIEW_V})\n"
        )
    );
}

#[test]
fn a_failed_upgrade_is_an_error_with_brews_exit_and_last_line() {
    let host = FakeHost::preview().failing(
        "brew upgrade 2lab-ai/tap/sopen-preview",
        "Error: sopen-preview is pinned. You must unpin it to upgrade.",
        false,
    );
    let s = said(|o, e| update(&host, o, e));
    assert_eq!(
        error_of(&s),
        "`brew upgrade 2lab-ai/tap/sopen-preview` failed (exit 1): Error: sopen-preview is \
         pinned. You must unpin it to upgrade."
    );
}

#[test]
fn a_failed_brew_update_refreshes_only_the_tap_by_its_git() {
    let host = offering_next(FakeHost::preview()).failing(
        "brew update",
        "Error: Some taps failed to update!",
        false,
    );
    let s = said(|o, e| update(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        s.err,
        "sopen: `brew update` failed (exit 1): Error: Some taps failed to update!; refreshing \
         only 2lab-ai/tap by its git\n"
    );
    assert_eq!(
        host.ran(),
        strs(&[
            "brew update",
            &format!("git -C {REPO} fetch -q https://github.com/2lab-ai/homebrew-tap.git master"),
            &format!("git -C {REPO} merge -q --ff-only FETCH_HEAD"),
            "brew upgrade 2lab-ai/tap/sopen-preview",
        ])
    );
    assert!(s.out.contains(&format!("→ {PREVIEW_NEXT}")), "{}", s.out);
}

#[test]
fn a_tap_that_cannot_be_refreshed_at_all_upgrades_nothing() {
    let host = FakeHost::preview()
        .failing("brew update", "Error: Some taps failed to update!", false)
        .failing(
            &format!("git -C {REPO} fetch -q https://github.com/2lab-ai/homebrew-tap.git master"),
            "fatal: unable to access 'https://github.com/2lab-ai/homebrew-tap.git/'",
            false,
        );
    let s = said(|o, e| update(&host, o, e));
    let e = error_of(&s);
    assert!(
        e.starts_with("could not refresh 2lab-ai/tap: `git -C "),
        "{e}"
    );
    assert!(e.contains("(exit 1): fatal: unable to access"), "{e}");
    assert!(!host.ran().iter().any(|c| c.starts_with("brew upgrade")));
    // No folder for the tap at all.
    let mut host =
        FakeHost::preview().failing("brew update", "Error: Some taps failed to update!", false);
    host.tap_repo = None;
    let s = said(|o, e| update(&host, o, e));
    assert_eq!(
        error_of(&s),
        "could not find 2lab-ai/tap's folder (`brew --repo 2lab-ai/tap`)"
    );
    assert_eq!(host.ran(), strs(&["brew update"]));
}

#[test]
fn brews_own_sopen_that_brew_cannot_list_is_refused_with_nothing_changed() {
    let mut host = FakeHost::preview();
    host.lists = false;
    let s = said(|o, e| update(&host, o, e));
    assert_eq!(
        error_of(&s),
        "brew did not answer `brew list --formula`; nothing was changed"
    );
    assert!(host.ran().is_empty());
    let host = FakeHost::preview();
    host.versions.borrow_mut().clear();
    let s = said(|o, e| switch(&host, Channel::Stable, o, e));
    assert_eq!(
        error_of(&s),
        "brew lists neither sopen nor sopen-preview as installed; nothing was changed"
    );
    assert!(host.ran().is_empty());
}

// --- not installed by brew ---------------------------------------------------------------------

#[test]
fn update_and_switch_refuse_a_sopen_brew_did_not_install() {
    let dev = "this sopen was not installed by brew (it is a dev build); install it with: brew \
               install 2lab-ai/tap/sopen-preview";
    for run in [
        &(|h: &FakeHost, o: &mut dyn Write, e: &mut dyn Write| update(h, o, e))
            as &dyn Fn(&FakeHost, &mut dyn Write, &mut dyn Write) -> Result<()>,
        &|h, o, e| switch(h, Channel::Stable, o, e),
        &|h, o, e| switch(h, Channel::Preview, o, e),
    ] {
        let host = FakeHost::dev_build();
        let s = said(|o, e| run(&host, o, e));
        assert_eq!(error_of(&s), dev);
        assert_eq!(s.out, "");
        assert!(host.calls().is_empty(), "{:?}", host.calls());
        // A release binary copied out of brew's keg: no keg, so refused all the same.
        let mut host = FakeHost::preview();
        host.exe = Some(PathBuf::from("/usr/local/bin/sopen"));
        let s = said(|o, e| run(&host, o, e));
        assert!(error_of(&s).starts_with("this sopen was not installed by brew ("));
        assert!(host.calls().is_empty(), "{:?}", host.calls());
        // brew's keg, but brew cannot be asked where its Cellar is.
        let mut host = FakeHost::preview();
        host.cellar = None;
        let s = said(|o, e| run(&host, o, e));
        assert!(
            error_of(&s).starts_with("brew is not available here"),
            "{}",
            error_of(&s)
        );
        assert_eq!(host.calls(), strs(&["cellar"]));
    }
}
