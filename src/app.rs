//! `--app <App>` (spec A6): the fetched copy opens in a named local app; the terminal waits for
//! "Enter = upload changes · q = discard". An external hex editor works this way too
//! (`--app ImHex`); sopen's own is the hex window (`--hex`, spec §6a).

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{BufRead, Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub const PROMPT: &str = "Enter = upload changes · q = discard: ";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    Upload,
    Discard,
}

/// macOS: `open -a <App> <file>`. Linux: `<App> <file>`, detached. Returns the command line.
pub fn open_with(app: &str, file: &Path) -> Result<String> {
    use std::os::unix::process::CommandExt;
    let (program, args): (&str, Vec<OsString>) = if cfg!(target_os = "macos") {
        (
            "open",
            vec!["-a".into(), app.into(), file.as_os_str().to_owned()],
        )
    } else {
        (app, vec![file.as_os_str().to_owned()])
    };
    let line = crate::render_command(program.as_ref(), &args);
    crate::debug!("spawn: {line}");
    // Only `open` (which exits at once) gets a stderr pipe to quote on failure. A Linux app *is*
    // the child and outlives this function; a pipe whose reader is gone would SIGPIPE it.
    let stderr = if cfg!(target_os = "macos") {
        Stdio::piped()
    } else {
        Stdio::null()
    };
    let mut child = Command::new(program)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(stderr)
        .process_group(0)
        .spawn()
        .with_context(|| format!("cannot start {program}"))?;
    // `open` returns at once; a Linux app keeps running. Either way a fast failure is reported.
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(target_os = "macos") { 10 } else { 1 });
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                let mut err = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut err);
                }
                let reason = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
                bail!(
                    "cannot open {} with {app} ({status}) {reason}",
                    file.display()
                );
            }
            return Ok(line);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Ok(line)
}

/// Reads answers until one decides: empty line = upload, `q` = discard, end of input = discard.
pub fn prompt(input: &mut dyn BufRead, out: &mut dyn Write) -> Result<Decision> {
    loop {
        write!(out, "{PROMPT}")?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            writeln!(out)?;
            return Ok(Decision::Discard);
        }
        match line.trim() {
            "" => return Ok(Decision::Upload),
            "q" | "Q" => return Ok(Decision::Discard),
            other => writeln!(out, "'{other}' is not an answer")?,
        }
    }
}

/// SHA-256 of a file — "did the app change it?".
pub fn file_hash(path: &Path) -> Result<[u8; 32]> {
    let mut file =
        std::fs::File::open(path).with_context(|| format!("cannot read {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            return Ok(hasher.finalize().into());
        }
        hasher.update(&buf[..n]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_answers() {
        let cases: [(&str, Decision); 4] = [
            ("\n", Decision::Upload),
            ("q\n", Decision::Discard),
            ("", Decision::Discard),
            ("what\n\n", Decision::Upload),
        ];
        for (typed, want) in cases {
            let mut out = Vec::new();
            let got = prompt(&mut typed.as_bytes(), &mut out).unwrap();
            assert_eq!(got, want, "{typed:?}");
            assert!(String::from_utf8(out).unwrap().starts_with(PROMPT));
        }
    }

    #[test]
    fn hash_changes_with_content() {
        let tmp = tempfile::tempdir().unwrap();
        let f = tmp.path().join("x");
        std::fs::write(&f, "a").unwrap();
        let a = file_hash(&f).unwrap();
        std::fs::write(&f, "b").unwrap();
        assert_ne!(a, file_hash(&f).unwrap());
    }
}
