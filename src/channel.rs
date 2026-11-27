//! `sopen channel [stable|preview]` and `sopen update` (owner message 25, `.prd/01-spec.md` §4b),
//! modelled on llmux's `channel` / `update`: which release channel this sopen is on, a switch to
//! the other one, and an update on its own channel — all through Homebrew.
//!
//! Distribution facts (single source of truth here):
//! - the tap `2lab-ai/tap` has `sopen-preview` (the preview prereleases, `YYYY.MM.DD.HHMM`) and,
//!   from the first `v*` release on, `sopen` (stable). Both formulas link `bin/sopen`
//!   (`link_overwrite "bin/sopen"`), so at most one may be linked;
//! - the build marker ([`crate::build_channel`]) is `preview` from preview.yml, `release` from
//!   release.yml and `dev` in any other build.
//!
//! The decisions (reversible; §4b repeats them):
//! - The channel is derived from what brew has installed — no config field. With both formulas
//!   installed the one whose keg `bin/sopen` resolves into decides, with a warning; when no link
//!   says, preview wins (llmux's rule).
//! - Only a sopen built `preview` or `release` that runs from a sopen keg in brew's own Cellar
//!   is installed by brew ([`install_from`]). Any other — a source build, a `dev` marker even in
//!   a keg's place, a copied binary — reports its
//!   build marker in `sopen channel` (a `dev` one has no channel: an error), and `sopen update` and
//!   `sopen channel <c>` refuse: brew would change another binary than the one that runs. llmux
//!   reports brew's formula whatever binary asks; sopen does not.
//! - The build marker is also the fallback when brew cannot answer.
//! - The same channel again does what llmux's same-channel plan does: nothing, a relink of
//!   `bin/sopen`, or the other formula retired. It never upgrades: that is `sopen update`.
//! - A switch refreshes the tap and looks the target up in it, so the stable channel without a
//!   release is refused before anything is installed — found from brew, not a flag, so it works
//!   the day `Formula/sopen.rb` lands. Then the target first, so a failure leaves the old channel
//!   whole: installed (upgraded when it already is); relinked only when `bin/sopen` is not its
//!   own once that ran; the other formula retired LAST (`uninstall --force`, or only `unlink` when
//!   another installed formula depends on it), when `bin/sopen` already resolves into the target —
//!   so a failure or Ctrl-C at any step leaves a sopen on PATH. llmux retires before it relinks,
//!   and `uninstall --force` unlinks `bin/sopen` while it points into the retired keg; sopen does
//!   not follow it there ([`converge_plan`]). A failed step's error line also says what is
//!   installed, whose `bin/sopen` is, and the way back (`brew link --overwrite <target>`).
//! - The tap refresh is `brew update`; when it fails (a broken third-party tap is enough), one
//!   line says so and only this tap's git is fast-forwarded — never prompting, never looking for
//!   a repository above the tap ([`git_command`]). brew always runs with
//!   `HOMEBREW_NO_AUTO_UPDATE=1`, its stdout the user's, its stderr passed on with its last line
//!   kept for the error ([`Failed`]); stderr is read for at most [`DRAIN`] past the command's exit.
//!
//! Every side effect is behind [`Host`]: [`RealHost`] runs brew and git, the tests' `FakeHost`
//! scripts them.

use anyhow::{anyhow, bail, Context, Result};
use std::cell::OnceCell;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;

#[cfg(test)]
mod tests;

/// The tap both formulas come from.
pub const TAP: &str = "2lab-ai/tap";
/// The tap's repository, fetched by its git when `brew update` fails.
pub const TAP_URL: &str = "https://github.com/2lab-ai/homebrew-tap.git";
/// The tap's branch.
pub const TAP_BRANCH: &str = "master";
/// The line after anything that replaced the installed binary.
pub const WINDOWS_KEEP_OLD: &str =
    "sopen windows already open keep running the old version until they are reopened";

/// A release channel brew installs. `dev` is not one: it is the absence of a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Channel {
    Stable,
    Preview,
}

impl Channel {
    pub const ALL: [Channel; 2] = [Channel::Stable, Channel::Preview];

    /// The formula that installs `bin/sopen` on this channel.
    pub fn formula(self) -> &'static str {
        match self {
            Channel::Stable => "sopen",
            Channel::Preview => "sopen-preview",
        }
    }

    /// `2lab-ai/tap/<formula>`: what `install`, `upgrade` and `info` are given, so no other
    /// tap's formula of the same name is taken.
    pub fn full_name(self) -> String {
        format!("{TAP}/{}", self.formula())
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::Stable => "stable",
            Channel::Preview => "preview",
        }
    }

    /// The other channel: the one a switch to `self` retires.
    pub fn other(self) -> Channel {
        match self {
            Channel::Stable => Channel::Preview,
            Channel::Preview => Channel::Stable,
        }
    }

    /// `stable` or `preview`, as `sopen channel` takes them.
    pub fn from_word(word: &str) -> Option<Channel> {
        Channel::ALL.into_iter().find(|c| c.label() == word)
    }
}

/// The channel of a build marker: `preview`, `release` (release.yml's word) or `stable`; `dev`
/// or anything else is none.
pub fn marker_channel(marker: &str) -> Option<Channel> {
    match marker {
        "preview" => Some(Channel::Preview),
        "release" | "stable" => Some(Channel::Stable),
        _ => None,
    }
}

/// One `brew` run (the words after `brew`): plain data the tests compare.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrewCmd(pub Vec<String>);

impl BrewCmd {
    fn new<const N: usize>(args: [&str; N]) -> Self {
        BrewCmd(args.iter().map(|s| (*s).to_string()).collect())
    }

    pub fn args(&self) -> Vec<&str> {
        self.0.iter().map(String::as_str).collect()
    }
}

/// A command that ran and failed: its words, how it ended (`exit 1`, a signal, not started) and
/// the last line it wrote on its standard error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failed {
    pub command: String,
    pub how: String,
    pub last_line: Option<String>,
}

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "`{}` failed ({})", self.command, self.how)?;
        match &self.last_line {
            Some(line) => write!(f, ": {line}"),
            None => Ok(()),
        }
    }
}

impl std::error::Error for Failed {}

/// Why a sopen does not count as installed by brew.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotBrew {
    /// No keg and a `dev` marker: a source build.
    DevBuild,
    /// The binary looks like a keg, but brew cannot say where its Cellar is.
    BrewUnavailable,
    /// `current_exe` could not be read.
    ExeUnknown,
    /// Not `<brew's Cellar>/<sopen|sopen-preview>/<version>/bin/sopen`.
    OutsideKeg,
}

/// Is the sopen that runs brew's?
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Install {
    /// It runs from this channel's keg.
    Keg(Channel),
    Not(NotBrew),
}

/// PURE: a keg's binary is `<cellar>/<sopen|sopen-preview>/<version>/bin/sopen`; its cellar and
/// channel, or `None`.
pub fn keg_shape(exe: &Path) -> Option<(PathBuf, Channel)> {
    if exe.file_name()? != "sopen" {
        return None;
    }
    let bin = exe.parent()?;
    if bin.file_name()? != "bin" {
        return None;
    }
    let version = bin.parent()?;
    version.file_name()?;
    let rack = version.parent()?;
    let name = rack.file_name()?.to_str()?;
    let channel = Channel::ALL.into_iter().find(|c| c.formula() == name)?;
    Some((rack.parent()?.to_path_buf(), channel))
}

/// The path with its links resolved, or as it is when it cannot be (it does not exist).
fn resolved(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Installed by brew = built `preview` or `release` (brew's binaries are; a `dev` build never
/// is, wherever it lies) and `exe` is a keg ([`keg_shape`]) in brew's own Cellar (`cellar`, asked
/// only for a binary shaped like a keg, so a source build never runs brew), both with their links
/// resolved — a prefix reached through a symlink is still brew's.
pub fn install_from(
    exe: Option<&Path>,
    cellar: impl FnOnce() -> Option<PathBuf>,
    marker: &str,
) -> Install {
    if marker_channel(marker).is_none() {
        return Install::Not(NotBrew::DevBuild);
    }
    let Some((keg_cellar, channel)) = exe.and_then(|exe| keg_shape(&resolved(exe))) else {
        return Install::Not(if exe.is_none() {
            NotBrew::ExeUnknown
        } else {
            NotBrew::OutsideKeg
        });
    };
    match cellar() {
        None => Install::Not(NotBrew::BrewUnavailable),
        Some(cellar) if resolved(&cellar) == keg_cellar => Install::Keg(channel),
        Some(_) => Install::Not(NotBrew::OutsideKeg),
    }
}

/// [`install_from`] with the host's answers.
pub fn installed(host: &dyn Host) -> Install {
    install_from(host.exe().as_deref(), || host.cellar(), &host.marker())
}

/// The one line `sopen update` and a switch refuse with. No path in it: a receipt is public. A
/// keg brew cannot be asked about may well be brew's, so that one says only that brew is not
/// there.
pub fn refusal(why: NotBrew) -> String {
    let reason = match why {
        NotBrew::BrewUnavailable => {
            return "brew is not available here (`brew --cellar` did not answer); sopen updates \
                    and switches only through brew"
                .to_string()
        }
        NotBrew::DevBuild => "it is a dev build",
        NotBrew::ExeUnknown => "its own path cannot be read",
        NotBrew::OutsideKeg => "it does not run from a sopen keg in brew's Cellar",
    };
    format!(
        "this sopen was not installed by brew ({reason}); install it with: brew install {}",
        Channel::Preview.full_name()
    )
}

/// Why the build marker answered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerWhy {
    BrewUnavailable,
    NotInstalledByBrew,
}

/// Where a channel came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Brew,
    Marker(MarkerWhy),
}

impl Source {
    /// The words after `via` in `sopen channel`'s line.
    pub fn via(self) -> &'static str {
        match self {
            Source::Brew => "brew",
            Source::Marker(MarkerWhy::BrewUnavailable) => "build marker (brew unavailable)",
            Source::Marker(MarkerWhy::NotInstalledByBrew) => "build marker (not installed by brew)",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detected {
    pub channel: Channel,
    pub source: Source,
    /// Both formulas installed: which one decided, and what each switch removes.
    pub warning: Option<String>,
}

/// PURE: the channel. A sopen brew installed ([`Install::Keg`]) takes it from brew's formulas —
/// one installed is the channel; both: the one `linked` names, else preview, with a warning
/// either way. Brew unavailable, neither formula listed, or a sopen brew did not install: the
/// build marker, whose `dev` is an error.
pub fn detect(
    install: Install,
    formulae: Option<&[String]>,
    linked: Option<&[String]>,
    marker: &str,
) -> Result<Detected> {
    let why = match (install, formulae) {
        (Install::Keg(_), Some(list)) => {
            if let Some(detected) = from_formulae(list, linked) {
                return Ok(detected);
            }
            MarkerWhy::NotInstalledByBrew
        }
        (Install::Keg(_), None) | (Install::Not(NotBrew::BrewUnavailable), _) => {
            MarkerWhy::BrewUnavailable
        }
        (Install::Not(_), _) => MarkerWhy::NotInstalledByBrew,
    };
    match marker_channel(marker) {
        Some(channel) => Ok(Detected {
            channel,
            source: Source::Marker(why),
            warning: None,
        }),
        None => {
            let said = match why {
                MarkerWhy::BrewUnavailable => "brew is not available",
                MarkerWhy::NotInstalledByBrew => "it was not installed by brew",
            };
            bail!("this sopen has no channel: {said} and its build marker is {marker}")
        }
    }
}

/// PURE: the channel `brew list --formula` names, `None` when it lists neither formula.
fn from_formulae(list: &[String], linked: Option<&[String]>) -> Option<Detected> {
    let has = |c: Channel| list.iter().any(|f| f == c.formula());
    let channel = match (has(Channel::Stable), has(Channel::Preview)) {
        (true, true) => {
            let decided = linked.and_then(sole_linked);
            return Some(Detected {
                channel: decided.unwrap_or(Channel::Preview),
                source: Source::Brew,
                warning: Some(both_installed(decided)),
            });
        }
        (true, false) => Channel::Stable,
        (false, true) => Channel::Preview,
        (false, false) => return None,
    };
    Some(Detected {
        channel,
        source: Source::Brew,
        warning: None,
    })
}

/// PURE: the channel whose formula is the only one in `linked`.
fn sole_linked(linked: &[String]) -> Option<Channel> {
    let is = |c: Channel| linked.iter().any(|f| f == c.formula());
    match (is(Channel::Stable), is(Channel::Preview)) {
        (true, false) => Some(Channel::Stable),
        (false, true) => Some(Channel::Preview),
        _ => None,
    }
}

fn both_installed(decided: Option<Channel>) -> String {
    let who = match decided {
        Some(c) => format!("bin/sopen is {}'s, so it decides", c.formula()),
        None => "bin/sopen is neither's, so preview wins".to_string(),
    };
    format!(
        "both sopen and sopen-preview are installed by brew and {who} — `sopen channel preview` \
         removes sopen, `sopen channel stable` removes sopen-preview (each only unlinks it when \
         another formula needs it)"
    )
}

/// The sopen formula whose keg `bin_sopen` (`<brew --prefix>/bin/sopen`) resolves into. No
/// `bin/sopen`, or a link to a keg that is gone or to anything else: none. A link that cannot be
/// read: `None` (unknown), never "not linked".
pub fn linked_of(bin_sopen: &Path, cellar: &Path) -> Option<Vec<String>> {
    let binary = match std::fs::canonicalize(bin_sopen) {
        Ok(binary) => binary,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Some(Vec::new()),
        Err(_) => return None,
    };
    let cellar = resolved(cellar);
    Some(
        Channel::ALL
            .into_iter()
            .filter(|c| binary.starts_with(cellar.join(c.formula())))
            .map(|c| c.formula().to_string())
            .collect(),
    )
}

/// PURE: is a formula in its tap, from `brew info --formula <full name>`: it ran fine, yes;
/// brew's "No available formula", no; any other failure, unknown.
pub fn tap_answer(success: bool, stderr: &str) -> Option<bool> {
    if success {
        Some(true)
    } else if stderr.contains("No available formula") {
        Some(false)
    } else {
        None
    }
}

/// PURE: the last line of `bytes` with text in it, colour codes taken out and trimmed; a `\r`
/// (a progress line redrawn) ends a line as `\n` does.
pub fn last_line(bytes: &[u8]) -> Option<String> {
    let text = String::from_utf8_lossy(&without_escapes(bytes)).into_owned();
    text.split(['\n', '\r'])
        .map(str::trim)
        .rfind(|line| !line.is_empty())
        .map(str::to_string)
}

/// PURE: `bytes` without ANSI escape sequences (`ESC [ … final byte`, or `ESC` and one byte).
fn without_escapes(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter().copied().peekable();
    while let Some(b) = it.next() {
        if b != 0x1b {
            out.push(b);
            continue;
        }
        if it.next() == Some(b'[') {
            for c in it.by_ref() {
                if (0x40..=0x7e).contains(&c) {
                    break;
                }
            }
        }
    }
    out
}

/// PURE: a formula's installed version — the keg its opt link resolves to (`opt_keg`, resolved)
/// when that is this formula's keg, else the newest of `brew list --versions` (run only then).
fn formula_version(
    formula: &str,
    opt_keg: Option<&Path>,
    list_versions: impl FnOnce() -> Option<String>,
) -> Option<String> {
    opt_keg
        .and_then(|keg| keg_version(formula, keg))
        .or_else(|| list_versions().as_deref().and_then(highest_listed_version))
}

/// PURE: `<Cellar>/<formula>/<version>` → `<version>`; a keg of another rack is no answer.
fn keg_version(formula: &str, keg: &Path) -> Option<String> {
    if keg.parent()?.file_name()? != formula {
        return None;
    }
    keg.file_name()?.to_str().map(str::to_string)
}

/// PURE: the newest version on a `brew list --versions <formula>` line (`<name> <version>…`,
/// in no meaningful order).
fn highest_listed_version(listing: &str) -> Option<String> {
    listing
        .lines()
        .next()?
        .split_whitespace()
        .skip(1)
        .max_by(|a, b| compare_versions(a, b))
        .map(str::to_string)
}

/// PURE: brew keg version order — segments split on `.`, `_` (brew's revision) and `-`, numbers
/// compared as numbers, the rest as text; on a common prefix the longer one is newer.
fn compare_versions(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let separator = |c: char| matches!(c, '.' | '_' | '-');
    let (mut left, mut right) = (a.split(separator), b.split(separator));
    loop {
        match (left.next(), right.next()) {
            (Some(x), Some(y)) => {
                let order = match (x.parse::<u64>(), y.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => x.cmp(y),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// PURE: what keeps a formula from being uninstalled, as short names: every installed formula
/// or cask `brew uses --installed` names (tap-prefixed, transitive).
pub fn blocking_dependents(dependents: &[String]) -> Vec<String> {
    dependents
        .iter()
        .filter_map(|name| name.trim().rsplit('/').next())
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// PURE: how a formula leaves `bin/sopen`. `uninstall --force` removes every keg (a plain one
/// leaves the rack listed, and both formulas would still be seen); with a dependent brew refuses
/// any uninstall, so it is only unlinked.
fn retire(formula: &str, dependents: &[String]) -> BrewCmd {
    if blocking_dependents(dependents).is_empty() {
        BrewCmd::new(["uninstall", "--force", formula])
    } else {
        BrewCmd::new(["unlink", formula])
    }
}

/// PURE: `bin/sopen` made the formula's for certain. A bare `link` of a keg whose link record
/// survived answers "Already linked" and does nothing; `unlink` first clears it. Run only when
/// `bin/sopen` is not the formula's ([`converge_plan`]), so this `unlink` never takes a working
/// `bin/sopen` away: brew's `unlink` removes only the links into the formula's own keg.
fn relink(formula: &str) -> [BrewCmd; 2] {
    [
        BrewCmd::new(["unlink", formula]),
        BrewCmd::new(["link", "--overwrite", formula]),
    ]
}

/// PURE: a switch's first step, after the tap refresh: install the target, or upgrade it when it
/// is already installed (`install` would leave a stale keg as it is). Nothing is unlinked or
/// removed before it, so its failure leaves the old channel whole.
pub fn install_step(target: Channel, target_installed: bool) -> BrewCmd {
    let full = target.full_name();
    if target_installed {
        BrewCmd::new(["upgrade", &full])
    } else {
        BrewCmd::new(["install", &full])
    }
}

/// PURE: what makes `bin/sopen` the target's and retires the other formula, from `linked` as it
/// is now (after the install step, or the same channel's preflight). The target is relinked
/// only when `bin/sopen` is not solely its own (or the link is unknown): `brew install` and
/// `upgrade` of a linked keg already link it over `bin/sopen`. The other formula is retired
/// LAST ([`retire`]): `bin/sopen` then resolves into the target, and `uninstall --force` or
/// `unlink` remove only the links into their own keg, so a failure or Ctrl-C at any step still
/// leaves a sopen on PATH (llmux retires first and relinks last, which can leave none).
pub fn converge_plan(
    target: Channel,
    linked: Option<&[String]>,
    other_dependents: &[String],
) -> Vec<BrewCmd> {
    let mut cmds = Vec::new();
    if linked.and_then(sole_linked) != Some(target) {
        cmds.extend(relink(target.formula()));
    }
    cmds.push(retire(target.other().formula(), other_dependents));
    cmds
}

/// The rest of the error line when a switch (or a convergence on the same channel) stopped at a
/// failed step, read from brew as it is now: what is installed, whose `bin/sopen` is, and the
/// way back to a sopen on PATH — after the install step, `brew link --overwrite <target>`.
fn where_it_stopped(host: &dyn Host, from: Channel, to: Channel, past_install: bool) -> String {
    let installed: Vec<String> = [to, from]
        .into_iter()
        .filter_map(|c| {
            let version = host.installed_version(c.formula())?;
            Some(format!("{} {version}", c.formula()))
        })
        .collect();
    let installed = if installed.is_empty() {
        "neither formula".to_string()
    } else {
        installed.join(", ")
    };
    let linked = host.linked();
    let owner = linked.as_deref().and_then(sole_linked);
    let bin = match (&linked, owner) {
        (_, Some(c)) => format!("bin/sopen is {}'s", c.formula()),
        (Some(_), None) => "nothing is linked as bin/sopen".to_string(),
        (None, None) => "bin/sopen could not be read".to_string(),
    };
    let link_back = |c: Channel| {
        format!(
            "; to make bin/sopen the {} one: brew link --overwrite {}",
            c.label(),
            c.formula()
        )
    };
    if past_install {
        format!(
            "it stopped part way — installed: {installed}; {bin}{}",
            link_back(to)
        )
    } else if owner == Some(from) || host.installed_version(from.formula()).is_none() {
        format!("nothing was removed — installed: {installed}; {bin}")
    } else {
        format!(
            "nothing was removed — installed: {installed}; {bin}{}",
            link_back(from)
        )
    }
}

/// A formula a plan only unlinked, because installed formulas depend on it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptFormula {
    pub formula: String,
    /// Short names ([`blocking_dependents`]).
    pub required_by: Vec<String>,
}

impl KeptFormula {
    /// `Some` exactly when [`retire`] chose `unlink`.
    fn from_dependents(formula: &str, dependents: &[String]) -> Option<KeptFormula> {
        let required_by = blocking_dependents(dependents);
        (!required_by.is_empty()).then(|| KeptFormula {
            formula: formula.to_string(),
            required_by,
        })
    }

    pub fn summary(&self) -> String {
        format!(
            "kept {} installed (required by {}) — unlinked",
            self.formula,
            self.required_by.join(", ")
        )
    }
}

/// What `sopen channel <c>` does when `c` is already the channel (llmux's same-channel plan).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SameChannel {
    /// Only the target is installed and `bin/sopen` is its binary (or the link is unknown).
    NothingToDo,
    /// Only the target is installed but `bin/sopen` is not its binary.
    Relink,
    /// Both installed, the other kept for its dependents, already unlinked: converged.
    KeptUnlinked(KeptFormula),
    /// Both installed: [`converge_plan`]; `kept` when the other only gets unlinked.
    Consolidate { kept: Option<KeptFormula> },
}

impl SameChannel {
    fn headline(&self, target: Channel) -> String {
        let on = format!("already on the {} channel", target.label());
        match self {
            SameChannel::NothingToDo => format!("{on} — nothing to do"),
            SameChannel::Relink => format!(
                "{on} — relinking {} (`bin/sopen` is not its binary)…",
                target.formula()
            ),
            SameChannel::KeptUnlinked(kept) => format!(
                "{on} — kept {} installed (required by {}), unlinked",
                kept.formula,
                kept.required_by.join(", ")
            ),
            SameChannel::Consolidate { .. } => format!(
                "{on} — retiring the other formula, {}…",
                target.other().formula()
            ),
        }
    }

    fn outcome(&self, target: Channel) -> Option<String> {
        match self {
            SameChannel::NothingToDo | SameChannel::KeptUnlinked(_) => None,
            SameChannel::Relink => Some(format!(
                "relinked {} — `bin/sopen` is its binary again",
                target.formula()
            )),
            SameChannel::Consolidate { kept: Some(kept) } => Some(kept.summary()),
            SameChannel::Consolidate { kept: None } => {
                Some(format!("removed {}", target.other().formula()))
            }
        }
    }
}

/// PURE: [`SameChannel`] from the read-only preflight: what is installed, the other formula's
/// dependents (empty when not asked) and the linked formulas (`None` = unknown).
pub fn same_channel_decision(
    target: Channel,
    target_installed: bool,
    other_installed: bool,
    other_dependents: &[String],
    linked: Option<&[String]>,
) -> SameChannel {
    if !target_installed {
        return SameChannel::NothingToDo;
    }
    if !other_installed {
        let target_linked = linked.map(|names| names.iter().any(|f| f == target.formula()));
        return match target_linked {
            Some(false) => SameChannel::Relink,
            _ => SameChannel::NothingToDo,
        };
    }
    let target_sole = linked.and_then(sole_linked) == Some(target);
    match KeptFormula::from_dependents(target.other().formula(), other_dependents) {
        Some(kept) if target_sole => SameChannel::KeptUnlinked(kept),
        kept => SameChannel::Consolidate { kept },
    }
}

/// Every side effect `sopen channel` and `sopen update` need. [`RealHost`] runs brew and git;
/// the tests script the answers and log the calls.
pub trait Host {
    /// `brew list --formula`; `None` when brew is unavailable or errs.
    fn formulae(&self) -> Option<Vec<String>>;
    /// The sopen formula `<brew --prefix>/bin/sopen` belongs to ([`linked_of`]); `None` unknown.
    fn linked(&self) -> Option<Vec<String>>;
    /// `brew --cellar`; `None` when brew is unavailable.
    fn cellar(&self) -> Option<PathBuf>;
    /// This process's binary (`current_exe`).
    fn exe(&self) -> Option<PathBuf>;
    /// This binary's build marker ([`crate::build_channel`]).
    fn marker(&self) -> String;
    /// A formula's installed version ([`formula_version`]); `None` when it is not installed.
    fn installed_version(&self, formula: &str) -> Option<String>;
    /// `brew uses --installed <formula>`; `None` when brew cannot answer.
    fn dependents(&self, formula: &str) -> Option<Vec<String>>;
    /// Is the formula (a full name) in its tap ([`tap_answer`]); `None` when brew cannot tell.
    fn in_tap(&self, full_name: &str) -> Option<bool>;
    /// `brew <args>` with `HOMEBREW_NO_AUTO_UPDATE=1`: its output the user's, its stderr's last
    /// line kept for the failure.
    fn run_brew(&self, args: &[&str]) -> std::result::Result<(), Failed>;
    /// `brew --repo 2lab-ai/tap`.
    fn tap_repo(&self) -> Option<PathBuf>;
    /// `git -C <repo> <args>`.
    fn run_git(&self, repo: &Path, args: &[&str]) -> std::result::Result<(), Failed>;
}

/// `sopen channel`: `channel: <c> (via …)`, and the both-installed warning on stderr.
pub fn show(host: &dyn Host, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let detected = detect_on(host, installed(host))?;
    warn(err, &detected)?;
    writeln!(
        out,
        "channel: {} (via {})",
        detected.channel.label(),
        detected.source.via()
    )?;
    Ok(())
}

/// [`detect`] with the host's answers; brew's formulas are asked only for brew's sopen.
fn detect_on(host: &dyn Host, install: Install) -> Result<Detected> {
    let (formulae, linked) = match install {
        Install::Keg(_) => (host.formulae(), host.linked()),
        Install::Not(_) => (None, None),
    };
    detect(
        install,
        formulae.as_deref(),
        linked.as_deref(),
        &host.marker(),
    )
}

fn warn(err: &mut dyn Write, detected: &Detected) -> Result<()> {
    if let Some(warning) = &detected.warning {
        writeln!(err, "sopen: {warning}")?;
    }
    Ok(())
}

/// The channel `sopen update` and a switch act on: only for a sopen brew installed, and only as
/// brew has it.
fn brew_channel(host: &dyn Host, err: &mut dyn Write) -> Result<Channel> {
    let install = installed(host);
    if let Install::Not(why) = install {
        bail!(refusal(why));
    }
    let detected = detect_on(host, install)?;
    match detected.source {
        Source::Brew => {}
        Source::Marker(MarkerWhy::BrewUnavailable) => {
            bail!("brew did not answer `brew list --formula`; nothing was changed")
        }
        Source::Marker(MarkerWhy::NotInstalledByBrew) => {
            bail!("brew lists neither sopen nor sopen-preview as installed; nothing was changed")
        }
    }
    warn(err, &detected)?;
    Ok(detected.channel)
}

/// `sopen channel <target>`.
pub fn switch(
    host: &dyn Host,
    target: Channel,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let current = brew_channel(host, err)?;
    if current == target {
        return same_channel(host, target, out);
    }
    refresh_tap(host, err)?;
    let full = target.full_name();
    match host.in_tap(&full) {
        Some(true) => {}
        Some(false) if target == Channel::Stable => bail!(
            "the stable channel has no release yet (its first v* release is the owner's step); \
             staying on {}",
            current.label()
        ),
        Some(false) => bail!("{full} is not in {TAP}; staying on {}", current.label()),
        None => bail!(
            "brew could not say whether {full} is in its tap (`brew info --formula {full}`); \
             nothing was changed"
        ),
    }
    // Read-only preflight before anything changes.
    let old_version = host.installed_version(current.formula());
    let target_installed = host.installed_version(target.formula()).is_some();
    let dependents = query_dependents(host, current.formula())?;
    writeln!(
        out,
        "switching channel: {} → {}…",
        current.label(),
        target.label()
    )?;
    let install = install_step(target, target_installed);
    host.run_brew(&install.args()).map_err(|failed| {
        anyhow!(
            "{failed}; {}",
            where_it_stopped(host, current, target, false)
        )
    })?;
    // Whose `bin/sopen` is now decides whether the target needs relinking.
    let linked = host.linked();
    run_plan(host, &converge_plan(target, linked.as_deref(), &dependents))
        .map_err(|e| anyhow!("{e:#}; {}", where_it_stopped(host, current, target, true)))?;
    if let Some(kept) = KeptFormula::from_dependents(current.formula(), &dependents) {
        writeln!(out, "{}", kept.summary())?;
    }
    let new_version = host.installed_version(target.formula());
    writeln!(
        out,
        "switched to the {} channel: {} {} → {} {}",
        target.label(),
        current.formula(),
        old_version.as_deref().unwrap_or("(unknown)"),
        target.formula(),
        new_version.as_deref().unwrap_or("(unknown)")
    )?;
    writeln!(out, "{WINDOWS_KEEP_OLD}")?;
    Ok(())
}

/// The channel chosen again: [`same_channel_decision`] from a read-only preflight (the other
/// formula's dependents asked before anything runs), announced, then run. No tap refresh: none
/// of it fetches anything.
fn same_channel(host: &dyn Host, target: Channel, out: &mut dyn Write) -> Result<()> {
    let other = target.other();
    let target_installed = host.installed_version(target.formula()).is_some();
    let other_installed = host.installed_version(other.formula()).is_some();
    let dependents = if target_installed && other_installed {
        query_dependents(host, other.formula())?
    } else {
        Vec::new()
    };
    let linked = host.linked();
    let action = same_channel_decision(
        target,
        target_installed,
        other_installed,
        &dependents,
        linked.as_deref(),
    );
    let cmds = match &action {
        SameChannel::NothingToDo | SameChannel::KeptUnlinked(_) => Vec::new(),
        SameChannel::Relink => relink(target.formula()).to_vec(),
        SameChannel::Consolidate { .. } => converge_plan(target, linked.as_deref(), &dependents),
    };
    writeln!(out, "{}", action.headline(target))?;
    run_plan(host, &cmds)
        .map_err(|e| anyhow!("{e:#}; {}", where_it_stopped(host, other, target, true)))?;
    if let Some(outcome) = action.outcome(target) {
        writeln!(out, "{outcome}")?;
    }
    Ok(())
}

/// What depends on `formula`, asked before anything changes: guessing "nothing" would stop a
/// plan half way at brew's refusal to uninstall.
fn query_dependents(host: &dyn Host, formula: &str) -> Result<Vec<String>> {
    host.dependents(formula).ok_or_else(|| {
        anyhow!(
            "could not list what depends on {formula} (`brew uses --installed {formula}`); \
             nothing was changed"
        )
    })
}

/// The plan's `brew` runs in order, stopping at the first failure.
fn run_plan(host: &dyn Host, cmds: &[BrewCmd]) -> Result<()> {
    for cmd in cmds {
        host.run_brew(&cmd.args())?;
    }
    Ok(())
}

/// `sopen update`: the tap refreshed, the channel's formula upgraded, before → after.
pub fn update(host: &dyn Host, out: &mut dyn Write, err: &mut dyn Write) -> Result<()> {
    let channel = brew_channel(host, err)?;
    let formula = channel.formula();
    writeln!(
        out,
        "updating {formula} on the {} channel…",
        channel.label()
    )?;
    refresh_tap(host, err)?;
    let before = host.installed_version(formula);
    host.run_brew(&["upgrade", &channel.full_name()])?;
    let after = host.installed_version(formula);
    match (before.as_deref(), after.as_deref()) {
        (Some(b), Some(a)) if a == b => writeln!(out, "{formula} is up to date ({a})")?,
        (b, a) => {
            writeln!(
                out,
                "{formula} {} → {}",
                b.unwrap_or("(not installed)"),
                a.unwrap_or("(unknown)")
            )?;
            writeln!(out, "{WINDOWS_KEEP_OLD}")?;
        }
    }
    Ok(())
}

/// `brew update`; when it fails, one line on stderr and this tap alone fast-forwarded by its
/// git. A tap that cannot be refreshed either way stops the command with nothing changed.
pub fn refresh_tap(host: &dyn Host, err: &mut dyn Write) -> Result<()> {
    let Err(failed) = host.run_brew(&["update"]) else {
        return Ok(());
    };
    writeln!(err, "sopen: {failed}; refreshing only {TAP} by its git")?;
    let repo = host
        .tap_repo()
        .with_context(|| format!("could not find {TAP}'s folder (`brew --repo {TAP}`)"))?;
    for args in [
        &["fetch", "-q", TAP_URL, TAP_BRANCH][..],
        &["merge", "-q", "--ff-only", "FETCH_HEAD"],
    ] {
        host.run_git(&repo, args)
            .map_err(|failed| anyhow!("could not refresh {TAP}: {failed}"))?;
    }
    Ok(())
}

/// The real host: `brew` and `git` found on PATH, as a shell finds them. Queries are captured;
/// mutating runs stream to the user.
#[derive(Default)]
pub struct RealHost {
    prefix: OnceCell<Option<PathBuf>>,
    cellar: OnceCell<Option<PathBuf>>,
}

impl RealHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// A brew query's stdout, or `None` when brew is missing or the query failed.
    fn query(args: &[&str]) -> Option<String> {
        let output = brew()
            .args(args)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
    }

    fn path_query(args: &[&str]) -> Option<PathBuf> {
        let out = Self::query(args)?;
        let path = out.trim();
        (!path.is_empty()).then(|| PathBuf::from(path))
    }

    fn prefix(&self) -> Option<PathBuf> {
        self.prefix
            .get_or_init(|| Self::path_query(&["--prefix"]))
            .clone()
    }
}

/// `brew`, never updating itself behind the command's back: sopen refreshes the tap first.
fn brew() -> Command {
    let mut command = Command::new("brew");
    command.env("HOMEBREW_NO_AUTO_UPDATE", "1");
    command
}

impl Host for RealHost {
    fn formulae(&self) -> Option<Vec<String>> {
        let out = Self::query(&["list", "--formula"])?;
        Some(out.split_whitespace().map(str::to_string).collect())
    }

    fn linked(&self) -> Option<Vec<String>> {
        let bin_sopen = self.prefix()?.join("bin").join("sopen");
        linked_of(&bin_sopen, &self.cellar()?)
    }

    fn cellar(&self) -> Option<PathBuf> {
        self.cellar
            .get_or_init(|| Self::path_query(&["--cellar"]))
            .clone()
    }

    fn exe(&self) -> Option<PathBuf> {
        std::env::current_exe().ok()
    }

    fn marker(&self) -> String {
        crate::build_channel().to_string()
    }

    fn installed_version(&self, formula: &str) -> Option<String> {
        // The opt link points at the keg in use.
        let opt_keg = self
            .prefix()
            .and_then(|prefix| std::fs::canonicalize(prefix.join("opt").join(formula)).ok());
        formula_version(formula, opt_keg.as_deref(), || {
            Self::query(&["list", "--versions", formula])
        })
    }

    fn dependents(&self, formula: &str) -> Option<Vec<String>> {
        let out = Self::query(&["uses", "--installed", formula])?;
        Some(out.split_whitespace().map(str::to_string).collect())
    }

    fn in_tap(&self, full_name: &str) -> Option<bool> {
        let output = brew()
            .args(["info", "--formula", full_name])
            .stdin(Stdio::null())
            .output()
            .ok()?;
        tap_answer(
            output.status.success(),
            &String::from_utf8_lossy(&output.stderr),
        )
    }

    fn run_brew(&self, args: &[&str]) -> std::result::Result<(), Failed> {
        let mut command = brew();
        command.args(args);
        run_passing_stderr(
            command,
            format!("brew {}", args.join(" ")),
            Box::new(std::io::stderr()),
            DRAIN,
        )
    }

    fn tap_repo(&self) -> Option<PathBuf> {
        Self::path_query(&["--repo", TAP])
    }

    fn run_git(&self, repo: &Path, args: &[&str]) -> std::result::Result<(), Failed> {
        run_passing_stderr(
            git_command(repo, args),
            format!("git -C {} {}", repo.display(), args.join(" ")),
            Box::new(std::io::stderr()),
            DRAIN,
        )
    }
}

/// `git -C <repo> <args>` for the tap's own checkout: git never asks for a password (a prompt
/// would go to the terminal past sopen's output; `GIT_TERMINAL_PROMPT=0` fails instead), and
/// never looks above the tap for a repository (`GIT_CEILING_DIRECTORIES`): a tap folder that is
/// no repository must not let `fetch` and `merge` run in Homebrew's own.
fn git_command(repo: &Path, args: &[&str]) -> Command {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo)
        .args(args)
        .stdin(Stdio::null())
        .env("GIT_TERMINAL_PROMPT", "0");
    if let Some(parent) = resolved(repo).parent() {
        command.env("GIT_CEILING_DIRECTORIES", parent);
    }
    command
}

/// How much of a command's stderr is kept to find its last line.
const STDERR_TAIL: usize = 64 * 1024;

/// How long stderr is still read once the command has ended: what it wrote is in the pipe
/// already, and a descendant that keeps the pipe open (none of brew's is known to) must not hold
/// sopen.
const DRAIN: Duration = Duration::from_secs(2);

/// Runs `command` (its stdout and stdin as they are) with its stderr copied to `copy` as it
/// comes, keeping the tail to name its last line when it fails. `shown` is how the failure
/// names it. The command's exit is waited for on its own; its stderr only for `drain` more.
fn run_passing_stderr(
    mut command: Command,
    shown: String,
    mut copy: Box<dyn Write + Send>,
    drain: Duration,
) -> std::result::Result<(), Failed> {
    let fail = |how: String, last_line: Option<String>| Failed {
        command: shown.clone(),
        how,
        last_line,
    };
    let mut child = command
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| fail(format!("could not start it: {e}"), None))?;
    let tail = Arc::new(Mutex::new(Vec::new()));
    let (ended, read_to_end) = mpsc::channel::<()>();
    if let Some(mut pipe) = child.stderr.take() {
        let tail = Arc::clone(&tail);
        std::thread::spawn(move || {
            let mut buf = [0u8; 8192];
            loop {
                match pipe.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        // A standard error whose reader is gone loses the copy, not the run.
                        let _ = copy.write_all(&buf[..n]);
                        let _ = copy.flush();
                        let mut tail = tail.lock().unwrap_or_else(|e| e.into_inner());
                        tail.extend_from_slice(&buf[..n]);
                        if tail.len() > STDERR_TAIL {
                            let excess = tail.len() - STDERR_TAIL;
                            tail.drain(..excess);
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
            let _ = ended.send(());
        });
    }
    let status = child
        .wait()
        .map_err(|e| fail(format!("could not wait for it: {e}"), None))?;
    // Its own output is read by now or within the bound; a pipe still open past it is left to
    // the reader thread, which ends with the pipe.
    let _ = read_to_end.recv_timeout(drain);
    if status.success() {
        return Ok(());
    }
    let tail = tail.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let how = match status.code() {
        Some(code) => format!("exit {code}"),
        None => {
            use std::os::unix::process::ExitStatusExt;
            format!("killed by signal {}", status.signal().unwrap_or(0))
        }
    };
    Err(fail(how, last_line(&tail)))
}
