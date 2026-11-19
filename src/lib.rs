//! sopen: open a file or folder on another machine as if it were local. Spec: `.prd/01-spec.md`.
//!
//! Module map: [`target`] parses the scp-style argument, [`forward`] decides where it opens (here,
//! or on another machine's screen — [`config`] holds the default), [`remote`] is the file layer
//! (SFTP over the system ssh, or the local disk), [`kind`] routes by type, [`zed`] / [`app`] hand
//! the file to an editor ([`raise`] brings the editor's window to the user on KDE Plasma),
//! [`ffmpeg`] / [`cut`] cut media, [`cache`] owns local paths, [`usage`] appends the run log.

pub mod app;
pub mod cache;
pub mod config;
pub mod cut;
pub mod ffmpeg;
pub mod forward;
pub mod gui;
pub mod kind;
pub mod raise;
pub mod remote;
pub mod target;
pub mod usage;
pub mod zed;

#[cfg(test)]
pub(crate) mod testutil;

/// `<ver> (<channel> <sha12>)` — `sopen --version` prints `sopen ` before it. Channel and build id
/// come from `SOPEN_BUILD_CHANNEL` / `SOPEN_BUILD_ID` at compile time (`dev` / `unknown` otherwise);
/// the build id is a commit sha, of which the first 12 characters are kept.
pub fn version_text() -> String {
    let channel = option_env!("SOPEN_BUILD_CHANNEL").unwrap_or("dev");
    let rev12: String = option_env!("SOPEN_BUILD_ID")
        .unwrap_or("unknown")
        .chars()
        .take(12)
        .collect();
    format!("{} ({channel} {rev12})", env!("CARGO_PKG_VERSION"))
}

/// A size for people: `999 B`, `1.5 MB` (powers of 1000).
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1000.0 && unit + 1 < UNITS.len() {
        value /= 1000.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}

/// True when `SOPEN_LOG=debug`: [`debug!`] lines go to stderr.
pub fn debug_enabled() -> bool {
    std::env::var("SOPEN_LOG").is_ok_and(|v| v.eq_ignore_ascii_case("debug"))
}

/// `debug!("...")` prints `sopen[debug] ...` to stderr when `SOPEN_LOG=debug`.
#[macro_export]
macro_rules! debug {
    ($($arg:tt)*) => {
        if $crate::debug_enabled() {
            eprintln!("sopen[debug] {}", format_args!($($arg)*));
        }
    };
}

/// Renders a command line for the debug log: arguments with spaces or quotes are single-quoted.
pub fn render_command(program: &std::ffi::OsStr, args: &[std::ffi::OsString]) -> String {
    let mut out = quote_arg(&program.to_string_lossy());
    for arg in args {
        out.push(' ');
        out.push_str(&quote_arg(&arg.to_string_lossy()));
    }
    out
}

fn quote_arg(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@+,%[]".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', r"'\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::{OsStr, OsString};

    #[test]
    fn version_text_shape() {
        let v = version_text();
        assert!(v.starts_with(env!("CARGO_PKG_VERSION")), "{v}");
        assert!(v.ends_with(')'), "{v}");
        assert_eq!(v.matches(' ').count(), 2, "{v}");
    }

    #[test]
    fn render_command_quotes_only_when_needed() {
        let args = [OsString::from("ssh://h/a b"), OsString::from("x")];
        assert_eq!(
            render_command(OsStr::new("/usr/bin/zed"), &args),
            "/usr/bin/zed 'ssh://h/a b' x"
        );
        assert_eq!(quote_arg("it's"), r"'it'\''s'");
    }
}
