#![no_main]
// SPDX-License-Identifier: GPL-3.0-or-later
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    (bridge_fuzz::targets::by_name("clipboard").expect("target").run)(data);
});
