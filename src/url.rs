//! A web address as SOURCE (`sopen http://127.0.0.1:1234/`, spec §4a): it opens in the default
//! browser of the machine sopen shows things on — through an ssh tunnel ([`crate::forward`])
//! when that is another machine and the address is loopback. Only `http://` and `https://` (the
//! scheme in any case) are addresses; anything else is a path or `host:path`
//! ([`crate::target`]), so `box:/x` is never read as one and `http://…` never as host `http`.
//!
//! A small parser, not a URL library: the scheme, the host (an IPv6 one in brackets), the port
//! (80 / 443 when absent) and the rest — path, query and fragment — kept as written. Nothing is
//! normalised: the browser gets the address byte for byte, and only a tunnel that had to take
//! another port rewrites it, the port alone.

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::path::PathBuf;

/// An `http://` or `https://` address, as written, split where sopen needs it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Url {
    /// The address exactly as given.
    text: String,
    /// The scheme as written (`HTTP`, `https`).
    scheme: String,
    /// `user[:password]` before an `@`, as written.
    userinfo: Option<String>,
    /// The host as written, an IPv6 one with its brackets.
    host: String,
    /// The port written after the host (`None`: none, or `:` with nothing after it).
    port: Option<u16>,
    /// Everything from the first `/`, `?` or `#` after the host, as written.
    rest: String,
}

/// `Some` when `arg` is an address (`http://` or `https://`, any case) — `Err` saying what is
/// wrong with it when it is malformed; `None` when it is not an address at all.
pub fn parse(arg: &str) -> Option<Result<Url, String>> {
    let scheme = ["http", "https"].into_iter().find(|scheme| {
        arg.get(..scheme.len() + 3)
            .is_some_and(|head| head.eq_ignore_ascii_case(&format!("{scheme}://")))
    })?;
    Some(parse_after(arg, scheme.len()))
}

/// [`parse`] past the scheme (`scheme_len` bytes, then `://`).
fn parse_after(arg: &str, scheme_len: usize) -> Result<Url, String> {
    // Control characters shown escaped, so the message stays one line.
    let shown: String = arg
        .chars()
        .map(|c| match c.is_control() {
            true => c.escape_default().to_string(),
            false => c.to_string(),
        })
        .collect();
    let bad = |why: &str| format!("{why} in '{shown}'");
    // One argument to the browser and one line in what sopen prints: no blank in it (a space
    // is `%20` in an address), and nothing a terminal would act on.
    if arg.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(bad(
            "a space or control character (write a space as %20) cannot be in an address",
        ));
    }
    let after = &arg[scheme_len + 3..];
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    let (userinfo, hostport) = match authority.rfind('@') {
        Some(at) => (Some(&authority[..at]), &authority[at + 1..]),
        None => (None, authority),
    };
    let (host, port) = if let Some(inside) = hostport.strip_prefix('[') {
        let close = inside
            .find(']')
            .ok_or_else(|| bad("no closing ']' after an IPv6 host"))?;
        let ip = &inside[..close];
        // A zone (`%25en0`) is not part of the address itself.
        let bare = ip.split('%').next().unwrap_or_default();
        if bare.parse::<std::net::Ipv6Addr>().is_err() {
            return Err(bad(&format!("'{ip}' is not an IPv6 address")));
        }
        let after = &inside[close + 1..];
        let port = match after {
            "" => None,
            _ => Some(after.strip_prefix(':').ok_or_else(|| {
                bad(&format!("'{after}' after ']' (only ':PORT' may follow it)"))
            })?),
        };
        (&hostport[..close + 2], port)
    } else {
        match hostport.split_once(':') {
            Some((host, port)) => (host, Some(port)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(bad("no host"));
    }
    if let Some(c) = host
        .chars()
        .find(|c| !host.starts_with('[') && "[]\\<>^`{|}\"".contains(*c))
    {
        return Err(bad(&format!("'{c}' cannot be in a host")));
    }
    let port = match port {
        None | Some("") => None,
        Some(p) => Some(
            p.parse::<u16>()
                .ok()
                .filter(|&n| n != 0 && p.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| bad(&format!("'{p}' is not a port (1-65535)")))?,
        ),
    };
    Ok(Url {
        text: arg.to_string(),
        scheme: arg[..scheme_len].to_string(),
        userinfo: userinfo.map(str::to_string),
        host: host.to_string(),
        port,
        rest: rest.to_string(),
    })
}

impl Url {
    /// The address exactly as given.
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The host without IPv6 brackets.
    pub fn host(&self) -> &str {
        self.host
            .strip_prefix('[')
            .and_then(|h| h.strip_suffix(']'))
            .unwrap_or(&self.host)
    }

    /// The port: as written, else the scheme's (80 for http, 443 for https).
    pub fn port(&self) -> u16 {
        match self.port {
            Some(port) => port,
            None if self.scheme.eq_ignore_ascii_case("https") => 443,
            None => 80,
        }
    }

    /// Whoever resolves the host means itself (`localhost`, 127/8, `::1` — the rule HOST and
    /// SOURCE go by, spec §4a), so on another machine it would reach that machine, not this one.
    pub fn is_loopback(&self) -> bool {
        crate::forward::is_loopback(self.host())
    }

    /// The address with the port `port`: as written when that is its port already, else the
    /// same address with only the port changed.
    pub fn with_port(&self, port: u16) -> String {
        if port == self.port() {
            return self.text.clone();
        }
        let user = match &self.userinfo {
            Some(user) => format!("{user}@"),
            None => String::new(),
        };
        format!("{}://{user}{}:{port}{}", self.scheme, self.host, self.rest)
    }

    /// ssh's `-R` value that serves this address on port `listen` of the other machine:
    /// `listen:host:port` (an IPv6 host in brackets, as ssh takes it).
    pub fn remote_forward(&self, listen: u16) -> String {
        format!("{listen}:{}:{}", self.host, self.port())
    }
}

/// The line a run that opened `url` prints (on HOST, the one the tunnel waits for).
pub fn opened_line(url: &str) -> String {
    format!("opened {url} in the browser")
}

/// Whether `line` is [`opened_line`]'s, and of which address.
pub fn opened_in(line: &str) -> Option<&str> {
    line.strip_prefix("opened ")?
        .strip_suffix(" in the browser")
}

/// What opens an address: `SOPEN_BROWSER` (a receipt hook: a command given the address as its
/// one argument) when set and not empty, else the default browser's opener — `open` on macOS,
/// `xdg-open` elsewhere.
pub fn browser(hook: Option<OsString>) -> PathBuf {
    match hook.filter(|h| !h.is_empty()) {
        Some(command) => PathBuf::from(command),
        None if cfg!(target_os = "macos") => PathBuf::from("open"),
        None => PathBuf::from("xdg-open"),
    }
}

/// Opens `url` in this machine's browser ([`browser`]): started in its own process group with
/// no input or output (a browser that kept a session's output open would keep that session,
/// and a tunnel through it, alive), waited for briefly; a quick failure is an error.
pub fn open(url: &Url) -> Result<()> {
    let browser = browser(std::env::var_os("SOPEN_BROWSER"));
    crate::zed::open_args(&browser, &[OsString::from(url.as_str())])
        .with_context(|| format!("cannot open {} in the browser", url.as_str()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(arg: &str) -> Url {
        match parse(arg) {
            Some(Ok(url)) => url,
            other => panic!("{arg}: {other:?}"),
        }
    }

    fn malformed(arg: &str) -> String {
        match parse(arg) {
            Some(Err(why)) => why,
            other => panic!("{arg}: {other:?}"),
        }
    }

    #[test]
    fn only_http_and_https_are_addresses() {
        for not in [
            "box:/x",
            "box:",
            "http:/x",
            "http:x",
            "http",
            "./http://x",
            "/tmp/http://x",
            "ftp://box/x",
            "file:///x",
            "me@http://x",
            "x.html",
            "",
            "[::1]:/x",
        ] {
            assert_eq!(parse(not), None, "{not}");
        }
        for is in ["http://box/", "https://box/", "HTTP://box/", "Https://box"] {
            assert!(matches!(parse(is), Some(Ok(_))), "{is}");
        }
        // Never read as host `http`: the scp reading of the same text would be.
        assert!(matches!(
            crate::target::parse("http://127.0.0.1:1234/"),
            Ok(crate::target::Target::Remote(ref spec)) if spec.host == "http"
        ));
    }

    #[test]
    fn host_port_and_rest_as_written() {
        let u = url("http://127.0.0.1:1234/a/b?x=1&y=%20#frag");
        assert_eq!(u.as_str(), "http://127.0.0.1:1234/a/b?x=1&y=%20#frag");
        assert_eq!((u.host(), u.port()), ("127.0.0.1", 1234));
        assert_eq!(u.rest, "/a/b?x=1&y=%20#frag");
        // Default ports.
        assert_eq!(url("http://box").port(), 80);
        assert_eq!(url("https://box/").port(), 443);
        assert_eq!(url("HTTPS://box:/").port(), 443);
        // The rest starts at the first `/`, `?` or `#`.
        let u = url("http://box:81?q=/x");
        assert_eq!((u.host(), u.port(), u.rest.as_str()), ("box", 81, "?q=/x"));
        let u = url("http://box#a:b");
        assert_eq!((u.host(), u.port(), u.rest.as_str()), ("box", 80, "#a:b"));
        // A user before the host.
        let u = url("http://me:pw@localhost:8080/");
        assert_eq!(u.userinfo.as_deref(), Some("me:pw"));
        assert_eq!((u.host(), u.port()), ("localhost", 8080));
        // IPv6 in brackets.
        let u = url("http://[::1]:9000/x");
        assert_eq!((u.host(), u.port()), ("::1", 9000));
        assert_eq!(url("http://[fe80::1%25en0]/").host(), "fe80::1%25en0");
    }

    #[test]
    fn malformed_addresses_say_why() {
        for (arg, says) in [
            ("http://", "no host"),
            ("http:///x", "no host"),
            ("http://:80/", "no host"),
            ("http://me@/", "no host"),
            ("http://box:abc/", "not a port"),
            ("http://box:0/", "not a port"),
            ("http://box:65536/", "not a port"),
            ("http://box:-1/", "not a port"),
            ("http://[::1/", "no closing ']'"),
            ("http://[::1]x/", "after ']'"),
            ("http://[box]/", "not an IPv6 address"),
            ("http://a b/", "space"),
            ("http://box/a b", "space"),
            ("http://box/\n", "space"),
            ("http://b\\ox/", "cannot be in a host"),
            ("http://b<ox/", "cannot be in a host"),
        ] {
            let why = malformed(arg);
            assert!(why.contains(says), "{arg:?}: {why}");
            assert!(why.contains(&arg.replace('\n', "\\n")), "{arg:?}: {why}");
        }
    }

    #[test]
    fn loopback() {
        for here in [
            "http://127.0.0.1:1234/",
            "http://127.9.8.7/",
            "http://localhost:3000/",
            "http://LocalHost/",
            "http://localhost./",
            "http://[::1]:8080/",
            "https://me@127.0.0.1/",
        ] {
            assert!(url(here).is_loopback(), "{here}");
        }
        for there in [
            "http://box:1234/",
            "http://192.0.2.7/",
            "http://localhost.example.com/",
            "http://[2001:db8::1]/",
            "http://128.0.0.1/",
        ] {
            assert!(!url(there).is_loopback(), "{there}");
        }
    }

    #[test]
    fn another_port_changes_the_port_alone() {
        let u = url("http://127.0.0.1:1234/a?b#c");
        // Its own port: byte for byte.
        assert_eq!(u.with_port(1234), "http://127.0.0.1:1234/a?b#c");
        assert_eq!(u.with_port(50000), "http://127.0.0.1:50000/a?b#c");
        let u = url("HTTP://me@localhost/x");
        assert_eq!(u.with_port(80), "HTTP://me@localhost/x");
        assert_eq!(u.with_port(50001), "HTTP://me@localhost:50001/x");
        let u = url("http://[::1]:9000");
        assert_eq!(u.with_port(50002), "http://[::1]:50002");
        let u = url("https://localhost:/p");
        assert_eq!(u.with_port(443), "https://localhost:/p");
        assert_eq!(u.with_port(50003), "https://localhost:50003/p");
    }

    #[test]
    fn the_tunnel_serves_the_address_from_this_machine() {
        assert_eq!(
            url("http://127.0.0.1:1234/").remote_forward(1234),
            "1234:127.0.0.1:1234"
        );
        assert_eq!(
            url("http://localhost/").remote_forward(50000),
            "50000:localhost:80"
        );
        assert_eq!(
            url("http://[::1]:9000/").remote_forward(9000),
            "9000:[::1]:9000"
        );
    }

    #[test]
    fn the_opened_line_is_read_back() {
        let line = opened_line("http://127.0.0.1:1/");
        assert_eq!(line, "opened http://127.0.0.1:1/ in the browser");
        assert_eq!(opened_in(&line), Some("http://127.0.0.1:1/"));
        assert_eq!(opened_in("opened box:/x in VS Code"), None);
    }

    #[test]
    fn the_browser_is_the_hook_else_the_platforms_opener() {
        let opener = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        assert_eq!(browser(None), PathBuf::from(opener));
        assert_eq!(browser(Some(OsString::new())), PathBuf::from(opener));
        assert_eq!(
            browser(Some(OsString::from("/x/stand-in"))),
            PathBuf::from("/x/stand-in")
        );
    }
}
