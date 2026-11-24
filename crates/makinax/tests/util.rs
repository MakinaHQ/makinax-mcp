//! `util::perms` and `util::redact` through their public paths: a
//! credential file leaves no group or other bit set, a loose directory is
//! tightened, and a keyed RPC URL loses its key but keeps its host.

use makinax::util::redact;

#[cfg(unix)]
#[test]
fn perms_leave_no_forbidden_bit_on_a_credential_file_or_its_directory() {
    use makinax::util::perms::{FORBIDDEN_BITS, harden_dir, write_private};
    use std::os::unix::fs::PermissionsExt;

    let mode = |p: &std::path::Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(FORBIDDEN_BITS, 0o077, "group and other, every bit");

    let dir = tempfile::tempdir().unwrap();
    let loose = dir.path().join("loose");
    std::fs::create_dir(&loose).unwrap();
    std::fs::set_permissions(&loose, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_ne!(mode(&loose) & FORBIDDEN_BITS, 0, "the fixture starts loose");
    harden_dir(&loose).unwrap();
    assert_eq!(mode(&loose) & FORBIDDEN_BITS, 0);

    let file = loose.join("key.json");
    write_private(&file, b"{}").unwrap();
    assert_eq!(mode(&file) & FORBIDDEN_BITS, 0);
    assert_eq!(std::fs::read(&file).unwrap(), b"{}");
}

#[test]
fn redact_drops_the_key_and_keeps_the_host() {
    let keyed = "https://base.example-gateway.io/v2/k3yM4t3r1alXyZ0123456789";
    let shown = redact::url(keyed);
    assert!(!shown.contains("k3yM4t3r1al"), "{shown}");
    assert!(shown.contains("base.example-gateway.io"), "{shown}");
    assert!(shown.contains(redact::MARK), "{shown}");
    // A keyless public endpoint passes through untouched.
    let public = "https://mainnet.base.org";
    assert_eq!(redact::url(public), public);
}

#[test]
fn redact_text_masks_the_key_and_keeps_the_messages_own_lines() {
    let message = "could not reach the endpoint\n  (https://base.example-gateway.io/v2/k3yM4t3r1alXyZ0123456789):\n  timed out";
    let shown = redact::text(message);
    assert!(!shown.contains("k3yM4t3r1al"), "{shown}");
    assert_eq!(
        shown,
        format!(
            "could not reach the endpoint\n  (https://base.example-gateway.io/v2/{}):\n  timed out",
            redact::MARK
        )
    );
    // The same message through `url` loses its line breaks.
    assert!(!redact::url(message).contains('\n'));
}
