//! The command-line target, parsed exactly like scp: `[user@]host:path` is remote when a `:` appears
//! before any `/`; `[user@][ipv6]:path` brackets an IPv6 host; everything else is a local path.
//! Rules follow OpenSSH `scp.c` `colon()` (leading `:` is part of a file name) and `strrchr('@')`
//! for the user.

use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    Local(PathBuf),
    Remote(RemoteSpec),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteSpec {
    pub user: Option<String>,
    /// Host or `~/.ssh/config` alias, without IPv6 brackets.
    pub host: String,
    /// Path exactly as typed after the colon; empty = the remote home.
    pub path: String,
}

impl RemoteSpec {
    /// The destination handed to `ssh` (`user@host`, IPv6 unbracketed — ssh takes it that way).
    pub fn destination(&self) -> String {
        match &self.user {
            Some(user) => format!("{user}@{}", self.host),
            None => self.host.clone(),
        }
    }

    /// `[user@]host` with IPv6 bracketed — the cache key and what results are printed with.
    pub fn label(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        match &self.user {
            Some(user) => format!("{user}@{host}"),
            None => host,
        }
    }

    /// `label:abs` — how a remote result is printed (paste-able into scp or sopen).
    pub fn display(&self, abs: &str) -> String {
        format!("{}:{abs}", self.label())
    }
}

/// Byte index of the separating colon, or `None` when the argument is a local path.
fn colon(arg: &str) -> Option<usize> {
    let bytes = arg.as_bytes();
    if bytes.first() == Some(&b':') {
        return None;
    }
    let mut bracket = bytes.first() == Some(&b'[');
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'@' && bytes.get(i + 1) == Some(&b'[') {
            bracket = true;
        }
        if b == b']' && bytes.get(i + 1) == Some(&b':') && bracket {
            return Some(i + 1);
        }
        if b == b':' && !bracket {
            return Some(i);
        }
        if b == b'/' {
            return None;
        }
    }
    None
}

pub fn parse(arg: &str) -> Result<Target, String> {
    let Some(at) = colon(arg) else {
        return Ok(Target::Local(PathBuf::from(arg)));
    };
    let (host_part, path) = (&arg[..at], &arg[at + 1..]);
    let (user, host) = match host_part.rfind('@') {
        Some(i) => (Some(&host_part[..i]), &host_part[i + 1..]),
        None => (None, host_part),
    };
    let host = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if host.is_empty() {
        return Err(format!("no host in '{arg}'"));
    }
    if user == Some("") {
        return Err(format!("empty user name in '{arg}'"));
    }
    Ok(Target::Remote(RemoteSpec {
        user: user.map(str::to_string),
        host: host.to_string(),
        path: path.to_string(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(user: Option<&str>, host: &str, path: &str) -> Target {
        Target::Remote(RemoteSpec {
            user: user.map(str::to_string),
            host: host.to_string(),
            path: path.to_string(),
        })
    }

    fn local(p: &str) -> Target {
        Target::Local(PathBuf::from(p))
    }

    #[test]
    fn parse_table() {
        let cases = [
            ("box:/var/log/a.log", remote(None, "box", "/var/log/a.log")),
            ("box:rel/to/home", remote(None, "box", "rel/to/home")),
            ("box:~/project", remote(None, "box", "~/project")),
            ("box:", remote(None, "box", "")),
            ("me@box:/x", remote(Some("me"), "box", "/x")),
            ("a@b@box:/x", remote(Some("a@b"), "box", "/x")),
            ("box:/a:b", remote(None, "box", "/a:b")),
            ("[::1]:/x", remote(None, "::1", "/x")),
            ("[fe80::1%en0]:x", remote(None, "fe80::1%en0", "x")),
            ("me@[::1]:/x", remote(Some("me"), "::1", "/x")),
            ("[::1]:", remote(None, "::1", "")),
            ("./a:b", local("./a:b")),
            ("/x:y", local("/x:y")),
            ("dir/f:x", local("dir/f:x")),
            (":leading", local(":leading")),
            ("file.txt", local("file.txt")),
            ("[::1]", local("[::1]")),
            ("[::1]/x:y", local("[::1]/x:y")),
            ("", local("")),
        ];
        for (arg, want) in cases {
            assert_eq!(parse(arg), Ok(want), "{arg}");
        }
    }

    #[test]
    fn parse_rejects_empty_host_or_user() {
        assert!(parse("me@:x").is_err());
        assert!(parse("[]:x").is_err());
        assert!(parse("@box:x").is_err());
    }

    #[test]
    fn labels() {
        let Target::Remote(spec) = parse("me@[::1]:/x").unwrap() else {
            panic!()
        };
        assert_eq!(spec.destination(), "me@::1");
        assert_eq!(spec.label(), "me@[::1]");
        assert_eq!(spec.display("/x"), "me@[::1]:/x");
        let Target::Remote(spec) = parse("box:a").unwrap() else {
            panic!()
        };
        assert_eq!(spec.destination(), "box");
        assert_eq!(spec.display("/home/u/a"), "box:/home/u/a");
    }
}
