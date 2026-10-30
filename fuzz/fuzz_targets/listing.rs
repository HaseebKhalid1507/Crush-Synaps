//! ls -l owner/group factoring: never panics; restore() undoes factor()
//! exactly; restore() never panics on arbitrary text.
#![no_main]
use crush_core::transforms::listing;
use libfuzzer_sys::fuzz_target;

const CMDS: &[Option<&str>] = &[None, Some("ls -la"), Some("ls -lhn /usr"), Some("ls -lo"), Some("ls -l | head")];

fuzz_target!(|data: &[u8]| {
    if data.is_empty() {
        return;
    }
    let text = String::from_utf8_lossy(&data[1..]);
    let cmd = CMDS[data[0] as usize % CMDS.len()];
    if let Some(f) = listing::factor(&text, cmd) {
        assert_eq!(listing::restore(&f).as_deref(), Some(text.as_ref()), "factor does not restore");
        assert!(f.len() < text.len(), "factored text is not shorter");
    }
    let _ = listing::restore(&text);
});
