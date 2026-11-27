//! A web address as SOURCE (`sopen http://127.0.0.1:1234/`, spec §4a): it opens in an xtools
//! window ([`crate::gui::web::address`]) — with `--browser`, in the default browser — of the
//! machine sopen shows things on, through an ssh tunnel ([`crate::forward`]) when that is
//! another machine and the address is loopback. Only `http://` and `https://` (the
//! scheme in any case) are addresses; anything else is a path or `host:path`
//! ([`crate::target`]), so `box:/x` is never read as one and `http://…` never as host `http`.
//!
//! A small parser, not a URL library: the scheme, the host (an IPv6 one in brackets), the port
//! (80 / 443 when absent) and the rest — path, query and fragment — kept as written. Nothing is
//! normalised: the browser gets the address byte for byte, and only a tunnel that had to take
//! another port rewrites it, the port alone. A host a browser would read otherwise than it is
//! written (`127.1`, `0x7f.0.0.1`, `%31`, full-width digits, `[::ffff:127.0.0.1]`) is refused,
//! so what sopen calls loopback is what the browser reaches.
//!
//! The address goes to the browser and nowhere else whole: every line sopen prints — results,
//! errors, debug — shows it as [`redacted`] does, without a password, query or fragment.

use anyhow::{bail, Result};
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// `arg` as sopen prints it: `user:***@` for a user and password (`***@` for a user alone,
/// which may be a token), `?…` for a query, `#…` for a fragment; the scheme, host, port and
/// path as written; control characters escaped, so it stays one line. Any text works, an
/// address sopen refuses too.
pub fn redacted(arg: &str) -> String {
    let shown = match arg.find("://") {
        None => arg.to_string(),
        Some(i) => {
            let (head, after) = arg.split_at(i + 3);
            let end = after.find(['/', '?', '#']).unwrap_or(after.len());
            let (authority, rest) = after.split_at(end);
            let authority = match authority.rfind('@') {
                Some(at) => match authority[..at].split_once(':') {
                    Some((user, _)) => format!("{user}:***@{}", &authority[at + 1..]),
                    None => format!("***@{}", &authority[at + 1..]),
                },
                None => authority.to_string(),
            };
            let (path, tail) = rest.split_at(rest.find(['?', '#']).unwrap_or(rest.len()));
            let mut out = format!("{head}{authority}{path}");
            if tail.starts_with('?') {
                out.push_str("?…");
            }
            if tail.contains('#') {
                out.push_str("#…");
            }
            out
        }
    };
    shown
        .chars()
        .map(|c| match c.is_control() {
            true => c.escape_default().to_string(),
            false => c.to_string(),
        })
        .collect()
}

/// A host spelled as a browser reads it: `None`, or why not. A browser reads a host whose last
/// label is a number (digits, or `0x…`) as an IPv4 address — `127.1`, `2130706433`, `0177.0.0.1`
/// and `0x7f.0.0.1` all reach 127.0.0.1 — so only a plain dotted quad may be one; it decodes `%`
/// and maps non-ASCII (full-width digits) before that, so neither may be in a host.
fn unplain_host(host: &str) -> Option<String> {
    const PLAINLY: &str = "write the host plainly (127.0.0.1)";
    if host.contains('%') {
        return Some(format!("a '%' in the host ({PLAINLY})"));
    }
    if !host.is_ascii() {
        return Some(
            "a non-ASCII host (write it plainly in ASCII: 127.0.0.1, or xn--… for an \
             international name)"
                .to_string(),
        );
    }
    let labels: Vec<&str> = host.split('.').collect();
    let last = match labels.as_slice() {
        [.., last, ""] => last,
        [.., last] => last,
        [] => return None,
    };
    let number = |label: &str| {
        let hex = label
            .strip_prefix("0x")
            .or_else(|| label.strip_prefix("0X"))
            .map(|h| h.bytes().all(|b| b.is_ascii_hexdigit()));
        hex.unwrap_or(!label.is_empty() && label.bytes().all(|b| b.is_ascii_digit()))
    };
    if !number(last) {
        return None;
    }
    let octet = |label: &&str| {
        !label.is_empty()
            && label.len() <= 3
            && label.bytes().all(|b| b.is_ascii_digit())
            && (label.len() == 1 || !label.starts_with('0'))
            && label.parse::<u16>().is_ok_and(|n| n <= 255)
    };
    match labels.len() == 4 && labels.iter().all(octet) {
        true => None,
        false => Some(format!(
            "'{host}' is read by browsers as an IPv4 address ({PLAINLY})"
        )),
    }
}

/// [`parse`] past the scheme (`scheme_len` bytes, then `://`).
fn parse_after(arg: &str, scheme_len: usize) -> Result<Url, String> {
    let shown = redacted(arg);
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
        if ip.contains('%') {
            return Err(bad(
                "a zone ('%…') in an IPv6 host (browsers do not take one)",
            ));
        }
        let Ok(v6) = ip.parse::<std::net::Ipv6Addr>() else {
            return Err(bad(&format!("'{ip}' is not an IPv6 address")));
        };
        // `::ffff:127.0.0.1` (and the old `::127.0.0.1`) reach an IPv4 address.
        let embedded = v6.segments()[..6].iter().all(|&s| s == 0) && !v6.is_loopback();
        if v6.to_ipv4_mapped().is_some() || (embedded && !v6.is_unspecified()) {
            return Err(bad(&format!(
                "'{ip}' is an IPv4 address in IPv6 (write the host plainly: 127.0.0.1)"
            )));
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
    if !host.starts_with('[') {
        if let Some(why) = unplain_host(host) {
            return Err(bad(&why));
        }
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
    /// The address exactly as given — for the browser only; print [`Url::shown`].
    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The address as sopen prints it ([`redacted`]).
    pub fn shown(&self) -> String {
        redacted(&self.text)
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

    /// ssh's `-R` value that serves this address on port `listen` of the other machine, bound
    /// there to loopback by name: `127.0.0.1:listen:host:port`, or `[::1]:listen:host:port` for
    /// an IPv6 address (an IPv6 host in brackets, as ssh takes it). sshd keeps an explicit
    /// loopback address under `GatewayPorts no` and `clientspecified`; only `yes` overrides it,
    /// which HOST's sopen refuses ([`crate::forward::tunnel_port_is_private`]).
    pub fn remote_forward(&self, listen: u16) -> String {
        let bind = match self.host.starts_with('[') {
            true => "[::1]",
            false => "127.0.0.1",
        };
        format!("{bind}:{listen}:{}:{}", self.host, self.port())
    }
}

/// The line a run that opened an address prints, `shown` being [`Url::shown`].
pub fn opened_line(shown: &str) -> String {
    format!("opened {shown} in the browser")
}

/// The line HOST's sopen prints once a tunnelled address is open in its browser — the cue the
/// other side waits for. Fixed: it carries nothing of the address.
pub const TUNNEL_OPEN: &str = "sopen-tunnel-open";

/// The line HOST's sopen prints before [`TUNNEL_OPEN`] once the address's window there has
/// started (09 §7): the other side then says that closing the window closes the tunnel. A HOST
/// that opened the system browser instead, or an older one, never prints it. Fixed, as
/// [`TUNNEL_OPEN`].
pub const TUNNEL_WINDOW: &str = "sopen-tunnel-window";

/// The line HOST's sopen prints when the address's window there was closed (09 §7): the other
/// side closes the tunnel at once and exits 0. Fixed, as [`TUNNEL_OPEN`].
pub const TUNNEL_CLOSED: &str = "sopen-tunnel-closed";

/// Says `cue` ([`TUNNEL_OPEN`], [`TUNNEL_WINDOW`], [`TUNNEL_CLOSED`]) on `out` as one line,
/// flushed, ignoring errors: the other side may be gone and its end of the output with it, and
/// HOST's half goes on all the same — `println!` would panic there (09 §7).
pub fn write_cue(out: &mut dyn std::io::Write, cue: &str) {
    let _ = writeln!(out, "{cue}");
    let _ = out.flush();
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

/// How long [`open`] waits for the browser's opener to exit: `SOPEN_EDITOR_WAIT` seconds as
/// an editor CLI is waited for (at most 120), else 3 s.
fn opener_wait(value: Option<&std::ffi::OsStr>) -> Duration {
    value
        .and_then(|v| v.to_str())
        .and_then(|v| v.trim().parse::<f64>().ok())
        .filter(|s| s.is_finite() && *s > 0.0)
        .map(|s| Duration::from_secs_f64(s.min(120.0)))
        .unwrap_or(Duration::from_secs(3))
}

/// Opens `url` in this machine's browser ([`browser`]): started in its own process group with
/// no input or output (a browser that kept a session's output open would keep that session,
/// and a tunnel through it, alive), waited for briefly ([`opener_wait`]); a quick failure is an
/// error. Only the browser gets the address whole.
pub fn open(url: &Url) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let browser = browser(std::env::var_os("SOPEN_BROWSER"));
    let shown = url.shown();
    crate::debug!("spawn: {} {shown}", browser.display());
    let spawned = Command::new(&browser)
        .arg(url.as_str())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => bail!(
            "cannot open {shown} in the browser: cannot start {}: {e}",
            browser.display()
        ),
    };
    let deadline = Instant::now() + opener_wait(std::env::var_os("SOPEN_EDITOR_WAIT").as_deref());
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            if !status.success() {
                bail!(
                    "cannot open {shown} in the browser: {} exited with {status}",
                    browser.display()
                );
            }
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
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
        assert_eq!(url("http://[0:0:0:0:0:0:0:1]/").host(), "0:0:0:0:0:0:0:1");
        assert!(url("http://[0:0:0:0:0:0:0:1]/").is_loopback());
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
            assert!(why.contains(&redacted(arg)), "{arg:?}: {why}");
            assert!(!why.contains('\n'), "{arg:?}: {why}");
        }
    }

    /// A host a browser would read as another address — loopback by another spelling — is
    /// refused, so sopen's loopback test cannot be passed by: `127.1` is 127.0.0.1 to a browser.
    #[test]
    fn a_host_must_be_spelled_plainly() {
        for (arg, says) in [
            ("http://127.1:1234/", "read by browsers as an IPv4 address"),
            ("http://2130706433/", "read by browsers as an IPv4 address"),
            ("http://0177.0.0.1/", "read by browsers as an IPv4 address"),
            ("http://0x7f.0.0.1/", "read by browsers as an IPv4 address"),
            ("http://127.0.0.0x1/", "read by browsers as an IPv4 address"),
            ("http://127.0.0.1./", "read by browsers as an IPv4 address"),
            ("http://127.000.0.1/", "read by browsers as an IPv4 address"),
            ("http://1.2.3.256/", "read by browsers as an IPv4 address"),
            ("http://foo.0x10/", "read by browsers as an IPv4 address"),
            ("http://127.0.0.%31/", "a '%' in the host"),
            ("http://local%68ost/", "a '%' in the host"),
            ("http://１２７.０.０.１/", "non-ASCII host"),
            ("http://ｌｏｃａｌｈｏｓｔ/", "non-ASCII host"),
            ("http://[::1%25lo0]/", "a zone"),
            ("http://[::ffff:127.0.0.1]/", "an IPv4 address in IPv6"),
            ("http://[::127.0.0.1]/", "an IPv4 address in IPv6"),
        ] {
            let why = malformed(arg);
            assert!(why.contains(says), "{arg}: {why}");
        }
        for plain in [
            "http://127.0.0.1/",
            "http://0.0.0.0/",
            "http://10.0.0.255/",
            "http://box/",
            "http://box.lan/",
            "http://1box/",
            "http://box1.example.com/",
            "http://localhost./",
            "http://[::1]/",
            "http://[::]/",
            "http://[2001:db8::1]/",
        ] {
            assert!(matches!(parse(plain), Some(Ok(_))), "{plain}");
        }
    }

    /// What sopen prints of an address: no password, query or fragment — they can be secrets.
    #[test]
    fn printed_addresses_carry_no_secrets() {
        for (arg, shown) in [
            ("http://127.0.0.1:1234/a", "http://127.0.0.1:1234/a"),
            (
                "https://me:s3cret@box:8443/p/q?token=abc&x=1#frag",
                "https://me:***@box:8443/p/q?…#…",
            ),
            ("http://tok3n@box/", "http://***@box/"),
            ("http://a:b@c@box/", "http://a:***@box/"),
            ("http://box?x", "http://box?…"),
            ("http://box/#x?y", "http://box/#…"),
            ("http://box/a?b#c", "http://box/a?…#…"),
            ("HTTP://[::1]:9/", "HTTP://[::1]:9/"),
            ("http://box/\nx", "http://box/\\nx"),
            ("not an address", "not an address"),
        ] {
            assert_eq!(redacted(arg), shown, "{arg}");
        }
        let u = url("http://me:s3cret@localhost:3000/x?token=abc#frag");
        assert_eq!(u.shown(), "http://me:***@localhost:3000/x?…#…");
        // The browser still gets it whole.
        assert_eq!(
            u.as_str(),
            "http://me:s3cret@localhost:3000/x?token=abc#frag"
        );
        // An address sopen refuses is said without its secrets too.
        for arg in [
            "http://me:s3cret@box:99999/?token=abc#frag",
            "http://me:s3cret@127.1/?token=abc",
            "http://me:s3cret@box/a b?token=abc",
        ] {
            let why = malformed(arg);
            for secret in ["s3cret", "token=abc", "frag"] {
                assert!(!why.contains(secret), "{arg}: {why}");
            }
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
        // Bound to loopback by address on the other machine, by family.
        assert_eq!(
            url("http://127.0.0.1:1234/").remote_forward(1234),
            "127.0.0.1:1234:127.0.0.1:1234"
        );
        assert_eq!(
            url("http://localhost/").remote_forward(50000),
            "127.0.0.1:50000:localhost:80"
        );
        assert_eq!(
            url("http://[::1]:9000/").remote_forward(9000),
            "[::1]:9000:[::1]:9000"
        );
    }

    #[test]
    fn the_opened_lines() {
        let u = url("http://me:pw@127.0.0.1:1/?t=1");
        assert_eq!(
            opened_line(&u.shown()),
            "opened http://me:***@127.0.0.1:1/?… in the browser"
        );
        // The tunnel's cues carry nothing of the address, and are three.
        for cue in [TUNNEL_OPEN, TUNNEL_WINDOW, TUNNEL_CLOSED] {
            assert!(!cue.contains("http"), "{cue}");
            assert!(cue.starts_with("sopen-tunnel-"), "{cue}");
        }
        assert_ne!(TUNNEL_WINDOW, TUNNEL_OPEN);
        assert_ne!(TUNNEL_CLOSED, TUNNEL_OPEN);
        assert_ne!(TUNNEL_WINDOW, TUNNEL_CLOSED);
    }

    /// A cue is one flushed line; an output that refuses it (the other side gone) is no error
    /// and no panic.
    #[test]
    fn a_cue_is_one_line_and_a_closed_output_is_ignored() {
        let mut out = Vec::new();
        write_cue(&mut out, TUNNEL_WINDOW);
        write_cue(&mut out, TUNNEL_OPEN);
        assert_eq!(out, b"sopen-tunnel-window\nsopen-tunnel-open\n");
        struct Gone(usize);
        impl std::io::Write for Gone {
            fn write(&mut self, _: &[u8]) -> std::io::Result<usize> {
                self.0 += 1;
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.0 += 1;
                Err(std::io::ErrorKind::BrokenPipe.into())
            }
        }
        let mut gone = Gone(0);
        write_cue(&mut gone, TUNNEL_CLOSED);
        // Tried, and the flush too.
        assert!(gone.0 >= 2, "{}", gone.0);
    }

    #[test]
    fn the_opener_wait_is_bounded() {
        let wait = |v: &str| opener_wait(Some(std::ffi::OsStr::new(v)));
        assert_eq!(opener_wait(None), Duration::from_secs(3));
        assert_eq!(wait("60"), Duration::from_secs(60));
        assert_eq!(wait("1e9"), Duration::from_secs(120));
        for bad in ["", "0", "-1", "x", "NaN", "inf"] {
            assert_eq!(wait(bad), Duration::from_secs(3), "{bad}");
        }
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
