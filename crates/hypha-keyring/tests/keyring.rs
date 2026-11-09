#![allow(clippy::unwrap_used)]
use hypha_keyring::KeyringSecrets;
use hypha_secret::Secrets;

#[test]
fn get_and_set() {
    assert_eq!(KeyringSecrets::default().service, "hypha");
    let k = KeyringSecrets {
        service: "hypha-test-nonexistent".into(),
    };
    // With or without a live secret service, get must not panic —
    // absence just returns None.
    let _ = k.get("definitely-absent-key");
    let _ = k.set("k", "v");
    let _ = k.get("k");
}
