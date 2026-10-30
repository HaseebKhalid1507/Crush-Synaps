//! The shell parse and classifier on arbitrary command lines and outputs.
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let s = String::from_utf8_lossy(data);
    let (cmd, out) = s.split_once('\u{0}').unwrap_or((&s, ""));
    let _ = crush_core::shell::parse(cmd);
    let _ = crush_core::classify("bash", Some(cmd), out);
    let _ = crush_core::classify::sniff(&s);
});
