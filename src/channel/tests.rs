//! The decisions of `sopen channel` / `sopen update` against a scripted [`FakeHost`]: no brew,
//! no network, no process. The filesystem halves (a keg reached through a symlinked prefix,
//! `bin/sopen`'s keg) run in a temp dir.

use super::*;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

/// `formula` → installed version, the store `installed_version` answers from.
type Versions = RefCell<Vec<(String, String)>>;
/// Run after each `brew` command, to model what it changed.
type RunHook = Box<dyn Fn(&[&str], &Versions)>;

/// Scripted [`Host`]: canned answers, and an ordered log of the calls that matter — `cellar`
/// (the first brew query an install check makes), `uses <formula>`, `info <full name>`,
/// `brew <args>` and `git <args>`.
struct FakeHost {
    formulae: Option<Vec<String>>,
    linked: Option<Vec<String>>,
    cellar: Option<PathBuf>,
    exe: Option<PathBuf>,
    marker: String,
    versions: Versions,
    /// `formula` → `dependents` answer; a formula with no entry has none.
    dependents: Vec<(String, Option<Vec<String>>)>,
    /// `full name` → `in_tap` answer; a name with no entry is in the tap.
    in_tap: Vec<(String, Option<bool>)>,
    tap_repo: Option<PathBuf>,
    /// The call (as logged) that fails, and how.
    fail: Vec<(String, Failed)>,
    calls: RefCell<Vec<String>>,
    on_run: Option<RunHook>,
}

const CELLAR: &str = "/brew/Cellar";
const PREVIEW_V: &str = "2026.11.27.0138";
const STABLE_V: &str = "0.2.0";

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
    /// brew's sopen-preview, the one that runs: the state after `brew install
    /// 2lab-ai/tap/sopen-preview`, with no stable release in the tap.
    fn preview() -> Self {
        FakeHost {
            formulae: Some(strs(&["ffmpeg", "sopen-preview"])),
            linked: Some(strs(&["sopen-preview"])),
            cellar: Some(PathBuf::from(CELLAR)),
            exe: Some(keg_exe("sopen-preview", PREVIEW_V)),
            marker: "preview".into(),
            versions: RefCell::new(vec![("sopen-preview".into(), PREVIEW_V.into())]),
            dependents: Vec::new(),
            in_tap: vec![("2lab-ai/tap/sopen".into(), Some(false))],
            tap_repo: Some(PathBuf::from("/brew/Library/Taps/2lab-ai/homebrew-tap")),
            fail: Vec::new(),
            calls: RefCell::new(Vec::new()),
            on_run: None,
        }
    }

    /// brew's stable sopen, the one that runs, after the first release.
    fn stable() -> Self {
        FakeHost {
            formulae: Some(strs(&["ffmpeg", "sopen"])),
            linked: Some(strs(&["sopen"])),
            exe: Some(keg_exe("sopen", STABLE_V)),
            marker: "release".into(),
            versions: RefCell::new(vec![("sopen".into(), STABLE_V.into())]),
            in_tap: Vec::new(),
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

    fn failing(mut self, call: &str, how: &str, last: &str) -> Self {
        let failed = Failed {
            command: call.to_string(),
            how: how.to_string(),
            last_line: Some(last.to_string()),
        };
        self.fail.push((call.to_string(), failed));
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn log(&self, call: String) -> std::result::Result<(), Failed> {
        self.calls.borrow_mut().push(call.clone());
        match self.fail.iter().find(|(c, _)| *c == call) {
            Some((_, failed)) => Err(failed.clone()),
            None => Ok(()),
        }
    }
}

impl Host for FakeHost {
    fn formulae(&self) -> Option<Vec<String>> {
        self.formulae.clone()
    }
    fn linked(&self) -> Option<Vec<String>> {
        self.linked.clone()
    }
    fn cellar(&self) -> Option<PathBuf> {
        self.calls.borrow_mut().push("cellar".into());
        self.cellar.clone()
    }
    fn exe(&self) -> Option<PathBuf> {
        self.exe.clone()
    }
    fn marker(&self) -> String {
        self.marker.clone()
    }
    fn installed_version(&self, formula: &str) -> Option<String> {
        self.versions
            .borrow()
            .iter()
            .find(|(f, _)| f == formula)
            .map(|(_, v)| v.clone())
    }
    fn dependents(&self, formula: &str) -> Option<Vec<String>> {
        self.calls.borrow_mut().push(format!("uses {formula}"));
        match self.dependents.iter().find(|(f, _)| f == formula) {
            Some((_, answer)) => answer.clone(),
            None => Some(Vec::new()),
        }
    }
    fn in_tap(&self, full_name: &str) -> Option<bool> {
        self.calls.borrow_mut().push(format!("info {full_name}"));
        match self.in_tap.iter().find(|(n, _)| n == full_name) {
            Some((_, answer)) => *answer,
            None => Some(true),
        }
    }
    fn run_brew(&self, args: &[&str]) -> std::result::Result<(), Failed> {
        self.log(format!("brew {}", args.join(" ")))?;
        if let Some(hook) = &self.on_run {
            hook(args, &self.versions);
        }
        Ok(())
    }
    fn tap_repo(&self) -> Option<PathBuf> {
        self.tap_repo.clone()
    }
    fn run_git(&self, _repo: &Path, args: &[&str]) -> std::result::Result<(), Failed> {
        self.log(format!("git {}", args.join(" ")))
    }
}

/// Sets `formula`'s version (`None`: removes it).
fn set_version(versions: &Versions, formula: &str, version: Option<&str>) {
    let mut v = versions.borrow_mut();
    v.retain(|(f, _)| f != formula);
    if let Some(version) = version {
        v.push((formula.to_string(), version.to_string()));
    }
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
    for why in [
        NotBrew::BrewUnavailable,
        NotBrew::ExeUnknown,
        NotBrew::OutsideKeg,
    ] {
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

// --- plans ------------------------------------------------------------------------------------

#[test]
fn a_switch_installs_the_target_before_it_removes_the_other() {
    let plan = switch_plan(Channel::Preview, Channel::Stable, false, &[]);
    assert_eq!(
        plan,
        vec![
            BrewCmd(strs(&["install", "2lab-ai/tap/sopen"])),
            BrewCmd(strs(&["uninstall", "--force", "sopen-preview"])),
            BrewCmd(strs(&["unlink", "sopen"])),
            BrewCmd(strs(&["link", "--overwrite", "sopen"])),
        ]
    );
}

#[test]
fn a_switch_upgrades_an_installed_target_and_only_unlinks_what_others_need() {
    let needed = strs(&["someone/tap/sopen-plugin"]);
    let plan = switch_plan(Channel::Stable, Channel::Preview, true, &needed);
    assert_eq!(
        plan,
        vec![
            BrewCmd(strs(&["upgrade", "2lab-ai/tap/sopen-preview"])),
            BrewCmd(strs(&["unlink", "sopen"])),
            BrewCmd(strs(&["unlink", "sopen-preview"])),
            BrewCmd(strs(&["link", "--overwrite", "sopen-preview"])),
        ]
    );
    assert_eq!(blocking_dependents(&needed), strs(&["sopen-plugin"]));
}

#[test]
fn consolidating_removes_the_stray_and_relinks_the_target() {
    assert_eq!(
        consolidate_plan(Channel::Preview, &[]),
        vec![
            BrewCmd(strs(&["uninstall", "--force", "sopen"])),
            BrewCmd(strs(&["unlink", "sopen-preview"])),
            BrewCmd(strs(&["link", "--overwrite", "sopen-preview"])),
        ]
    );
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
}

#[test]
fn sopen_channel_warns_when_both_are_installed() {
    let mut host = FakeHost::stable();
    host.formulae = Some(strs(&["sopen", "sopen-preview"]));
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
    host.formulae = None;
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
    // The tap refreshed and asked, nothing else: no install, uninstall, link or query of
    // what depends on sopen-preview.
    assert_eq!(
        host.calls(),
        strs(&["cellar", "brew update", "info 2lab-ai/tap/sopen"])
    );
}

#[test]
fn a_switch_to_preview_from_stable_installs_removes_and_relinks() {
    let mut host = FakeHost::stable();
    host.on_run = Some(Box::new(|args: &[&str], versions: &Versions| match args {
        ["install", "2lab-ai/tap/sopen-preview"] => {
            set_version(versions, "sopen-preview", Some(PREVIEW_V))
        }
        ["uninstall", "--force", "sopen"] => set_version(versions, "sopen", None),
        _ => {}
    }));
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(
        host.calls(),
        strs(&[
            "cellar",
            "brew update",
            "info 2lab-ai/tap/sopen-preview",
            "uses sopen",
            "brew install 2lab-ai/tap/sopen-preview",
            "brew uninstall --force sopen",
            "brew unlink sopen-preview",
            "brew link --overwrite sopen-preview",
        ])
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
}

#[test]
fn a_switch_to_stable_once_released_names_old_and_new() {
    let mut host = FakeHost::preview();
    host.in_tap.clear();
    host.on_run = Some(Box::new(|args: &[&str], versions: &Versions| match args {
        ["install", "2lab-ai/tap/sopen"] => set_version(versions, "sopen", Some(STABLE_V)),
        ["uninstall", "--force", "sopen-preview"] => set_version(versions, "sopen-preview", None),
        _ => {}
    }));
    let s = said(|o, e| switch(&host, Channel::Stable, o, e));
    s.result.unwrap();
    assert!(
        s.out
            .contains(&format!("sopen-preview {PREVIEW_V} → sopen {STABLE_V}")),
        "{}",
        s.out
    );
}

#[test]
fn a_switch_to_the_same_channel_is_one_line_and_runs_nothing() {
    let host = FakeHost::preview();
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(s.out, "already on the preview channel — nothing to do\n");
    assert_eq!(host.calls(), strs(&["cellar"]));
    // `bin/sopen` not its binary: relinked, still no tap refresh.
    let mut host = FakeHost::preview();
    host.linked = Some(Vec::new());
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert_eq!(
        host.calls(),
        strs(&[
            "cellar",
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
}

#[test]
fn a_failed_install_stops_the_switch_with_the_old_channel_whole() {
    let host = FakeHost::stable().failing(
        "brew install 2lab-ai/tap/sopen-preview",
        "exit 1",
        "Error: Failed to download resource \"sopen-preview\"",
    );
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert_eq!(
        error_of(&s),
        "`brew install 2lab-ai/tap/sopen-preview` failed (exit 1): Error: Failed to download \
         resource \"sopen-preview\""
    );
    assert!(
        !host
            .calls()
            .iter()
            .any(|c| c.contains("uninstall") || c.contains("link")),
        "{:?}",
        host.calls()
    );
}

#[test]
fn a_formula_others_need_is_kept_unlinked_and_said_so() {
    let mut host = FakeHost::stable();
    host.dependents = vec![("sopen".into(), Some(strs(&["someone/tap/sopen-plugin"])))];
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    s.result.unwrap();
    assert!(host.calls().contains(&"brew unlink sopen".to_string()));
    assert!(!host.calls().iter().any(|c| c.contains("uninstall")));
    assert!(
        s.out
            .contains("kept sopen installed (required by sopen-plugin) — unlinked\n"),
        "{}",
        s.out
    );
    // An unanswerable dependents query changes nothing.
    let mut host = FakeHost::stable();
    host.dependents = vec![("sopen".into(), None)];
    let s = said(|o, e| switch(&host, Channel::Preview, o, e));
    assert!(
        error_of(&s).contains("nothing was changed"),
        "{}",
        error_of(&s)
    );
    assert!(!host.calls().iter().any(|c| c.starts_with("brew install")));
}

// --- `sopen update` ---------------------------------------------------------------------------

/// `brew upgrade` brings sopen-preview to `to`.
fn upgrading_to(host: &mut FakeHost, to: &'static str) {
    host.on_run = Some(Box::new(move |args: &[&str], versions: &Versions| {
        if args == ["upgrade", "2lab-ai/tap/sopen-preview"] {
            set_version(versions, "sopen-preview", Some(to));
        }
    }));
}

#[test]
fn an_update_that_changed_the_version_says_before_after_and_the_windows_line() {
    let mut host = FakeHost::preview();
    upgrading_to(&mut host, "2026.11.28.0101");
    let s = said(|o, e| update(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        host.calls(),
        strs(&[
            "cellar",
            "brew update",
            "brew upgrade 2lab-ai/tap/sopen-preview"
        ])
    );
    assert_eq!(
        s.out,
        format!(
            "updating sopen-preview on the preview channel…\n\
             sopen-preview {PREVIEW_V} → 2026.11.28.0101\n\
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
        "exit 1",
        "Error: sopen-preview is pinned. You must unpin it to upgrade.",
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
    let mut host = FakeHost::preview().failing(
        "brew update",
        "exit 1",
        "Error: Some taps failed to update!",
    );
    upgrading_to(&mut host, "2026.11.28.0101");
    let s = said(|o, e| update(&host, o, e));
    s.result.unwrap();
    assert_eq!(
        s.err,
        "sopen: `brew update` failed (exit 1): Error: Some taps failed to update!; refreshing \
         only 2lab-ai/tap by its git\n"
    );
    assert_eq!(
        host.calls(),
        strs(&[
            "cellar",
            "brew update",
            "git fetch -q https://github.com/2lab-ai/homebrew-tap.git master",
            "git merge -q --ff-only FETCH_HEAD",
            "brew upgrade 2lab-ai/tap/sopen-preview",
        ])
    );
    assert!(s.out.contains("→ 2026.11.28.0101"), "{}", s.out);
}

#[test]
fn a_tap_that_cannot_be_refreshed_at_all_upgrades_nothing() {
    let host = FakeHost::preview()
        .failing(
            "brew update",
            "exit 1",
            "Error: Some taps failed to update!",
        )
        .failing(
            "git fetch -q https://github.com/2lab-ai/homebrew-tap.git master",
            "exit 128",
            "fatal: unable to access 'https://github.com/2lab-ai/homebrew-tap.git/'",
        );
    let s = said(|o, e| update(&host, o, e));
    let e = error_of(&s);
    assert!(
        e.starts_with("could not refresh 2lab-ai/tap: `git fetch"),
        "{e}"
    );
    assert!(e.contains("(exit 128): fatal: unable to access"), "{e}");
    assert!(!host.calls().iter().any(|c| c.starts_with("brew upgrade")));
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
        assert!(
            !host.calls().iter().any(|c| c.starts_with("brew ")),
            "{:?}",
            host.calls()
        );
    }
}
