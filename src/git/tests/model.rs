//! The shared types' own rules: an [`Oid`] is only ever a full id as git prints it.

use crate::git::model::Oid;

/// 40 hex digits (SHA-1) and 64 (SHA-256) parse; anything else — a short id, uppercase, a
/// stray space or newline, a non-hex digit, nothing — does not (03 §4.1: a record whose hash
/// does not parse ends the page with an error).
#[test]
fn an_oid_is_a_full_lowercase_id_of_either_hash() {
    let sha1 = "2e084efc5801212e6ba3b4bff49b18256e6fc1d1";
    let sha256 = "4f".repeat(32);
    let oid = Oid::parse(sha1).expect("a SHA-1 id");
    assert_eq!(oid.as_str(), sha1);
    assert_eq!(oid.short(), "2e084efc");
    let long = Oid::parse(&sha256).expect("a SHA-256 id");
    assert_eq!(long.as_str(), sha256);
    assert_eq!(long.short(), "4f4f4f4f");

    for bad in [
        "",
        "2e084efc",
        &sha1[..39],
        &format!("{sha1}0"),
        &sha1.to_uppercase(),
        &format!("{} ", &sha1[..39]),
        &format!("{sha1}\n"),
        &format!("g{}", &sha1[1..]),
        &sha256[..63],
    ] {
        assert_eq!(Oid::parse(bad), None, "{bad:?}");
    }
}
