//! The config file (spec §4a): `$SOPEN_CONFIG`, else `$XDG_CONFIG_HOME/sopen/config.toml`, else
//! `~/.config/sopen/config.toml` — the same rule on macOS and Linux. TOML with one key,
//! `target = "<host>"`: the machine `sopen SOURCE` opens on when neither `--here`, `--target` nor
//! `SOPEN_TARGET` says. A key sopen does not know, a value it cannot use or a file that is not
//! TOML is an error naming the file — never ignored.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// Where `sopen SOURCE` opens by default: `[user@]host`, as `--target` takes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// The config file's path, from the environment.
pub fn path() -> Result<PathBuf> {
    path_from(
        std::env::var_os("SOPEN_CONFIG"),
        std::env::var_os("XDG_CONFIG_HOME"),
        directories::BaseDirs::new().map(|b| b.home_dir().to_path_buf()),
    )
}

fn path_from(
    explicit: Option<OsString>,
    xdg_config_home: Option<OsString>,
    home: Option<PathBuf>,
) -> Result<PathBuf> {
    if let Some(file) = explicit.filter(|p| !p.is_empty()) {
        return Ok(PathBuf::from(file));
    }
    // The XDG rule: a relative XDG_CONFIG_HOME is invalid and ignored.
    if let Some(dir) = xdg_config_home
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
    {
        return Ok(dir.join("sopen").join("config.toml"));
    }
    let home = home.context("no home directory for the config file (set SOPEN_CONFIG)")?;
    Ok(home.join(".config").join("sopen").join("config.toml"))
}

/// Reads the config at `path`; no file is an empty config.
pub fn load(path: &Path) -> Result<Config> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Config::default()),
        Err(e) => return Err(e).with_context(|| format!("cannot read {}", path.display())),
    };
    parse(&text).map_err(|problem| anyhow!("{}: {problem}", path.display()))
}

/// The config in `text`, or what is wrong with it in one line (`line N: …`).
pub fn parse(text: &str) -> std::result::Result<Config, String> {
    let config: Config = toml::from_str(text).map_err(|e| {
        let message = e.message().split_whitespace().collect::<Vec<_>>().join(" ");
        match e.span() {
            Some(span) => {
                let before = &text.as_bytes()[..span.start.min(text.len())];
                let line = before.iter().filter(|&&b| b == b'\n').count() + 1;
                format!("line {line}: {message}")
            }
            None => message,
        }
    })?;
    if let Some(host) = &config.target {
        crate::forward::check_host(host).map_err(|problem| format!("target: {problem}"))?;
    }
    Ok(config)
}

/// Sets `target = host`. A file sopen cannot read is refused rather than replaced.
pub fn set_target(path: &Path, host: &str) -> Result<()> {
    crate::forward::check_host(host).map_err(anyhow::Error::msg)?;
    let mut config = load(path)?;
    config.target = Some(host.to_string());
    write(path, &config)
}

/// Removes `target`; returns whether it was set. The file stays — empty once nothing is left in
/// it — and no file is created when there is none.
pub fn unset_target(path: &Path) -> Result<bool> {
    let mut config = load(path)?;
    if config.target.take().is_none() {
        return Ok(false);
    }
    write(path, &config)?;
    Ok(true)
}

/// A new config file is private (it names the machines you use); a replaced one keeps its mode.
const NEW_FILE_MODE: u32 = 0o600;

/// Atomic replace: a temp file in the same directory, flushed, renamed over the config. A config
/// that is a symlink (a dotfiles checkout) has the file it points at replaced, so the link stays.
fn write(path: &Path, config: &Config) -> Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    let text = toml::to_string(config).context("cannot encode the config")?;
    let dest = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .with_context(|| format!("{} links to a file that is missing", path.display()))?,
        _ => path.to_path_buf(),
    };
    let dir = match dest.parent() {
        Some(dir) if !dir.as_os_str().is_empty() => dir.to_path_buf(),
        _ => PathBuf::from("."),
    };
    std::fs::create_dir_all(&dir).with_context(|| format!("cannot create {}", dir.display()))?;
    let name = dest
        .file_name()
        .with_context(|| format!("{} is not a file name", dest.display()))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let mut temp_name = OsString::from(".");
    temp_name.push(name);
    temp_name.push(format!(".sopen-{}-{nanos}", std::process::id()));
    let temp = dir.join(temp_name);
    let mode = match std::fs::metadata(&dest) {
        Ok(meta) => meta.permissions().mode() & 0o7777,
        Err(_) => NEW_FILE_MODE,
    };
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(NEW_FILE_MODE)
        .open(&temp)
        .with_context(|| format!("cannot create {}", temp.display()))?;
    // From here the temp is ours, and it is removed if it does not become the config. The mode
    // is set exactly (the umask applies only at creation).
    let written = file
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .and_then(|()| file.write_all(text.as_bytes()))
        .and_then(|()| file.sync_all())
        .and_then(|()| std::fs::rename(&temp, &dest));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        return Err(e).with_context(|| format!("cannot write {}", dest.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_rule() {
        let home = Some(PathBuf::from("/h"));
        let os = |s: &str| Some(OsString::from(s));
        assert_eq!(
            path_from(os("/c/sopen.toml"), os("/x"), home.clone()).unwrap(),
            PathBuf::from("/c/sopen.toml")
        );
        assert_eq!(
            path_from(os(""), os("/x"), home.clone()).unwrap(),
            PathBuf::from("/x/sopen/config.toml")
        );
        assert_eq!(
            path_from(None, os("relative"), home.clone()).unwrap(),
            PathBuf::from("/h/.config/sopen/config.toml")
        );
        assert_eq!(
            path_from(None, None, home).unwrap(),
            PathBuf::from("/h/.config/sopen/config.toml")
        );
        assert!(path_from(None, None, None).is_err());
    }

    #[test]
    fn parse_the_one_key() {
        assert_eq!(parse("").unwrap(), Config::default());
        assert_eq!(
            parse("# where it opens\ntarget = \"me@box\"\n")
                .unwrap()
                .target,
            Some("me@box".to_string())
        );
    }

    #[test]
    fn unknown_keys_bad_toml_and_bad_hosts_are_errors() {
        let unknown = parse("target = \"box\"\ncolour = \"red\"\n").unwrap_err();
        assert!(unknown.starts_with("line 2: "), "{unknown}");
        assert!(unknown.contains("colour"), "{unknown}");
        let bad = parse("target = box\n").unwrap_err();
        assert!(bad.starts_with("line 1: "), "{bad}");
        assert!(!bad.contains('\n'), "{bad}");
        let not_a_host = parse("target = \"-oProxyCommand=x\"\n").unwrap_err();
        assert!(not_a_host.starts_with("target: "), "{not_a_host}");
        assert!(parse("target = 3\n").is_err());

        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        std::fs::write(&file, "colour = \"red\"\n").unwrap();
        let e = format!("{:#}", load(&file).unwrap_err());
        assert!(
            e.starts_with(&format!("{}: line 1: ", file.display())),
            "{e}"
        );
        assert_eq!(
            load(&tmp.path().join("none.toml")).unwrap(),
            Config::default()
        );
    }

    #[test]
    fn set_and_unset() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("a/b/config.toml");
        assert!(!unset_target(&file).unwrap(), "nothing to unset");
        assert!(!file.exists(), "unset creates no file");

        set_target(&file, "me@box").unwrap();
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "target = \"me@box\"\n"
        );
        set_target(&file, "other").unwrap();
        assert_eq!(load(&file).unwrap().target.as_deref(), Some("other"));
        assert!(set_target(&file, "has space").is_err());
        assert!(set_target(&file, "box:/x").is_err());
        assert_eq!(load(&file).unwrap().target.as_deref(), Some("other"));

        assert!(unset_target(&file).unwrap());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "");
        assert_eq!(load(&file).unwrap(), Config::default());
        let left: Vec<_> = std::fs::read_dir(file.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(left, ["config.toml"], "no temp left behind");

        // A file sopen cannot read is not replaced.
        std::fs::write(&file, "colour = \"red\"\n").unwrap();
        assert!(set_target(&file, "box").is_err());
        assert!(unset_target(&file).is_err());
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "colour = \"red\"\n"
        );
    }

    #[test]
    fn a_new_config_is_private_and_a_replaced_one_keeps_its_mode() {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o7777;
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("config.toml");
        set_target(&file, "box").unwrap();
        assert_eq!(mode(&file), 0o600);
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o640)).unwrap();
        set_target(&file, "other").unwrap();
        assert_eq!(mode(&file), 0o640);
        assert!(unset_target(&file).unwrap());
        assert_eq!(mode(&file), 0o640);
    }

    #[test]
    fn a_symlinked_config_keeps_its_link() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("dotfiles/sopen.toml");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "").unwrap();
        let link = tmp.path().join("config.toml");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        set_target(&link, "box").unwrap();
        assert!(std::fs::symlink_metadata(&link)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(
            std::fs::read_to_string(&real).unwrap(),
            "target = \"box\"\n"
        );
    }
}
