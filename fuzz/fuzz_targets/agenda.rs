#![no_main]
// SPDX-License-Identifier: GPL-3.0-or-later
libfuzzer_sys::fuzz_target!(|data: &[u8]| {
    (bridge_fuzz::targets::by_name("agenda").expect("target").run)(data);
});
