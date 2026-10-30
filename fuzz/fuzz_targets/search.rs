//! SEARCH grouping: never panics, always rebuilds, checked by rebuilds().
#![no_main]
use crush_core::transforms::search;
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    if let Some(g) = search::group(&s) {
        assert!(search::rebuilds(&s, &g), "does not rebuild");
    }
});
