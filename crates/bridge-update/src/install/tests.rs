// SPDX-License-Identifier: GPL-3.0-or-later
use super::*;
use std::{
    cell::{Cell, RefCell},
    fs,
    io::Read,
    os::unix::fs::{PermissionsExt, symlink},
    time::Duration,
};

// ---- archive builders -------------------------------------------------------------------------

fn octal(v: u64, width: usize) -> Vec<u8> {
    format!("{v:0w$o}\0", w = width - 1).into_bytes()
}

fn header(name: &str, kind: u8, mode: u64, size: u64, link: &str) -> [u8; 512] {
    let mut h = [0_u8; 512];
    h[..name.len()].copy_from_slice(name.as_bytes());
    h[100..108].copy_from_slice(&octal(mode, 8));
    h[108..116].copy_from_slice(&octal(0, 8));
    h[116..124].copy_from_slice(&octal(0, 8));
    h[124..136].copy_from_slice(&octal(size, 12));
    h[136..148].copy_from_slice(&octal(0, 12));
    h[148..156].fill(b' ');
    h[156] = kind;
    h[157..157 + link.len()].copy_from_slice(link.as_bytes());
    h[257..265].copy_from_slice(b"ustar  \0");
    let sum: u64 = h.iter().map(|b| u64::from(*b)).sum();
    h[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
    h
}

fn entry(name: &str, kind: u8, mode: u64, data: &[u8], link: &str) -> Vec<u8> {
    let mut v = header(name, kind, mode, data.len() as u64, link).to_vec();
    v.extend_from_slice(data);
    v.resize(v.len().div_ceil(512) * 512, 0);
    v
}

fn finish(mut tar: Vec<u8>) -> Vec<u8> {
    tar.extend(std::iter::repeat_n(0_u8, 1024));
    tar.resize(tar.len().div_ceil(10240) * 10240, 0);
    tar
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFF_u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
    }
    !c
}

/// gzip with stored blocks only (no compressor is needed to build test archives).
fn gzip(data: &[u8]) -> Vec<u8> {
    let mut g = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 3];
    let mut chunks: Vec<&[u8]> = data.chunks(60000).collect();
    if chunks.is_empty() {
        chunks.push(&[]);
    }
    for (i, c) in chunks.iter().enumerate() {
        g.push(u8::from(i + 1 == chunks.len()));
        g.extend((c.len() as u16).to_le_bytes());
        g.extend((!(c.len() as u16)).to_le_bytes());
        g.extend_from_slice(c);
    }
    g.extend(crc32(data).to_le_bytes());
    g.extend((data.len() as u32).to_le_bytes());
    g
}

const TOP: &str = "geekbridge-linux-x86_64";

fn fake_bin(name: &str, version: &str) -> Vec<u8> {
    if name == "geekbridge-service" {
        format!("#!/bin/sh\necho \"geekbridge-service {version}\"\n").into_bytes()
    } else {
        format!("#!/bin/sh\necho \"{name} {version}\"\n").into_bytes()
    }
}

fn good_tar(version: &str) -> Vec<u8> {
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for n in BINARIES {
        t.extend(entry(
            &format!("{TOP}/{n}"),
            b'0',
            0o755,
            &fake_bin(n, version),
            "",
        ));
    }
    t.extend(entry(
        &format!("{TOP}/README.md"),
        b'0',
        0o644,
        b"readme\n",
        "",
    ));
    finish(t)
}

// ---- sandbox ----------------------------------------------------------------------------------

struct Sandbox {
    dir: tempfile::TempDir,
    prefix: PathBuf,
}

impl Sandbox {
    /// A prefix as install.sh leaves it: regular files in bin/, running `old`.
    fn new(old: &str) -> Sandbox {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("prefix");
        fs::create_dir_all(prefix.join("bin")).unwrap();
        for n in BINARIES {
            let p = prefix.join("bin").join(n);
            fs::write(&p, fake_bin(n, old)).unwrap();
            fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
        }
        Sandbox { dir, prefix }
    }

    fn state(&self) -> PathBuf {
        self.dir.path().join("update-state.json")
    }

    fn archive(&self, tar_bytes: &[u8]) -> (PathBuf, String, u64) {
        let gz = gzip(tar_bytes);
        let p = self.dir.path().join("download.tar.gz");
        let _ = fs::remove_file(&p);
        fs::write(&p, &gz).unwrap();
        let sha = {
            use sha2::{Digest, Sha256};
            bridge_core::ipc::encode_hex(&Sha256::digest(&gz))
        };
        (p, sha, gz.len() as u64)
    }

    fn request<'a>(
        &'a self,
        a: &'a (PathBuf, String, u64),
        tag: &'a str,
        current: &'a str,
        state: &'a Path,
    ) -> Request<'a> {
        Request {
            prefix: &self.prefix,
            archive: &a.0,
            sha256: &a.1,
            size: a.2,
            tag,
            current_version: current,
            dry_run: false,
            state_file: Some(state),
            strict_binaries: false,
        }
    }

    fn install(&self, tar_bytes: &[u8], tag: &str, current: &str) -> Result<Report, Error> {
        let a = self.archive(tar_bytes);
        let st = self.state();
        install(&self.request(&a, tag, current, &st))
    }

    /// What `bin/geekbridge-service` prints, i.e. which version really runs.
    fn running(&self) -> String {
        let out = std::process::Command::new(self.prefix.join("bin/geekbridge-service"))
            .output()
            .unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_owned()
    }

    /// Everything under the prefix: relative path and, for links, their target.
    fn listing(&self) -> Vec<String> {
        fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
            let mut es: Vec<_> = fs::read_dir(dir).unwrap().flatten().collect();
            es.sort_by_key(fs::DirEntry::file_name);
            for e in es {
                let p = e.path();
                let rel = p.strip_prefix(base).unwrap().display().to_string();
                let m = fs::symlink_metadata(&p).unwrap();
                if m.is_symlink() {
                    out.push(format!("{rel} -> {}", fs::read_link(&p).unwrap().display()));
                } else if m.is_dir() {
                    out.push(format!("{rel}/"));
                    walk(&p, base, out);
                } else {
                    out.push(format!("{rel} {}", m.len()));
                }
            }
        }
        let mut v = Vec::new();
        walk(&self.prefix, &self.prefix, &mut v);
        v
    }

    fn link_name(&self, which: &str) -> Option<String> {
        Layout::new(&self.prefix).link_name(which)
    }

    fn no_debris(&self) {
        let l = self.listing();
        assert!(
            !l.iter()
                .any(|e| e.contains(".staging-") || e.contains(".tmp-")),
            "debris: {l:?}"
        );
    }
}

fn read_gz(bytes: &[u8], limit: u64) -> std::io::Result<Vec<u8>> {
    let mut out = Vec::new();
    gz::Gunzip::new(bytes, limit).read_to_end(&mut out)?;
    Ok(out)
}

// ---- inflate ----------------------------------------------------------------------------------

#[test]
fn inflate_matches_real_gzip_output_fixed_and_dynamic_blocks() {
    let small = read_gz(include_bytes!("fixtures/small.gz"), 1 << 20).unwrap();
    assert_eq!(small, b"hello hello hello\n");
    let lines = read_gz(include_bytes!("fixtures/lines.gz"), 1 << 20).unwrap();
    let want: String = (0..3000_u64)
        .map(|i| format!("line {i} {}\n", (i * i) % 97))
        .collect();
    assert_eq!(lines, want.as_bytes());
    // Stored blocks, several of them (the test archives use this path).
    let big: Vec<u8> = (0..200_000_u32).map(|i| (i % 251) as u8).collect();
    assert_eq!(read_gz(&gzip(&big), 1 << 20).unwrap(), big);
}

#[test]
fn inflate_rejects_truncated_corrupt_trailing_and_oversized_streams() {
    let good = include_bytes!("fixtures/lines.gz").to_vec();
    for cut in [5, 30, good.len() / 2, good.len() - 5, good.len() - 1] {
        assert!(read_gz(&good[..cut], 1 << 20).is_err(), "cut at {cut}");
    }
    let mut crc = good.clone();
    let n = crc.len();
    crc[n - 6] ^= 0x55;
    assert!(
        read_gz(&crc, 1 << 20)
            .unwrap_err()
            .to_string()
            .contains("checksum")
    );
    let mut tail = good.clone();
    tail.push(0);
    assert!(read_gz(&tail, 1 << 20).is_err());
    assert!(
        read_gz(&good, 1000)
            .unwrap_err()
            .to_string()
            .contains("more than the allowed")
    );
    // Damage anywhere in the compressed body never panics.
    for k in 10..good.len().min(600) {
        let mut g = good.clone();
        g[k] ^= 0xA5;
        let _ = read_gz(&g, 1 << 20);
    }
    assert!(read_gz(b"not gzip at all", 10).is_err());
}

// ---- hostile archives -------------------------------------------------------------------------

fn expect_untouched(sb: &Sandbox, tar_bytes: &[u8], must_contain: &str) {
    let before = sb.listing();
    let err = sb.install(tar_bytes, "v0.2.0", "0.1.0").unwrap_err();
    assert!(
        matches!(err, Error::Archive(_) | Error::Invalid(_)),
        "{err:?}"
    );
    assert!(
        err.to_string().contains(must_contain),
        "{err} !~ {must_contain}"
    );
    assert_eq!(sb.listing(), before, "the installation changed");
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
    let outcome = last_outcome(&sb.state()).unwrap();
    assert!(
        outcome.starts_with("failed 0.2.0") && outcome.contains(must_contain),
        "{outcome}"
    );
}

fn with_extra(extra: Vec<u8>) -> Vec<u8> {
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    t.extend(extra);
    for n in BINARIES {
        t.extend(entry(
            &format!("{TOP}/{n}"),
            b'0',
            0o755,
            &fake_bin(n, "0.2.0"),
            "",
        ));
    }
    finish(t)
}

#[test]
fn path_traversal_and_absolute_names_are_refused() {
    let sb = Sandbox::new("0.1.0");
    for name in [
        format!("{TOP}/../../evil"),
        "../evil".to_owned(),
        "/etc/evil".to_owned(),
        format!("{TOP}/sub/../../x"),
        format!("{TOP}/sub/deeper/file"),
        format!("{TOP}/back\\slash"),
    ] {
        let t = with_extra(entry(&name, b'0', 0o644, b"x", ""));
        expect_untouched(&sb, &t, "entry");
    }
    assert!(!sb.dir.path().join("evil").exists() && !sb.prefix.join("evil").exists());
}

#[test]
fn links_devices_and_extension_records_are_refused() {
    let sb = Sandbox::new("0.1.0");
    for (kind, word) in [
        (b'2', "symbolic link"),
        (b'1', "hard link"),
        (b'3', "device"),
        (b'4', "device"),
        (b'x', "extension"),
        (b'L', "extension"),
    ] {
        let t = with_extra(entry(
            &format!("{TOP}/geekbridge-evil"),
            kind,
            0o777,
            &[],
            "/etc/passwd",
        ));
        expect_untouched(&sb, &t, word);
    }
    // A symlink standing in for a real binary is the classic escape: it must not even start.
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    t.extend(entry(
        &format!("{TOP}/geekbridge-service"),
        b'2',
        0o777,
        &[],
        "/usr/bin/id",
    ));
    expect_untouched(&sb, &finish(t), "symbolic link");
}

#[test]
fn shape_duplicates_and_second_top_directory_are_refused() {
    let sb = Sandbox::new("0.1.0");
    let dup = with_extra(
        entry(&format!("{TOP}/geekbridge-evil"), b'0', 0o755, b"a", "")
            .into_iter()
            .chain(entry(
                &format!("{TOP}/geekbridge-evil"),
                b'0',
                0o755,
                b"b",
                "",
            ))
            .collect(),
    );
    expect_untouched(&sb, &dup, "twice");
    let other = with_extra(entry("other-top/file", b'0', 0o644, b"x", ""));
    expect_untouched(&sb, &other, "more than one top");
    let loose = with_extra(entry("loosefile", b'0', 0o644, b"x", ""));
    expect_untouched(&sb, &loose, "more than one top");
    let slash = with_extra(entry(&format!("{TOP}/dir/"), b'0', 0o644, b"x", ""));
    expect_untouched(&sb, &slash, "unexpected shape");
}

#[test]
fn damaged_headers_truncation_and_huge_declarations_are_refused() {
    let sb = Sandbox::new("0.1.0");
    let mut bad = good_tar("0.2.0");
    bad[3] ^= 1; // a name byte: the header checksum no longer matches
    expect_untouched(&sb, &bad, "damaged");
    let good = good_tar("0.2.0");
    expect_untouched(&sb, &good[..2000], "truncated");
    expect_untouched(&sb, &good[..512 + 512 + 10], "truncated");
    // A header that promises 4 GiB: refused before any byte is written.
    let mut huge = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    huge.extend_from_slice(&header(
        &format!("{TOP}/geekbridge"),
        b'0',
        0o755,
        4 << 30,
        "",
    ));
    expect_untouched(&sb, &huge, "too large");
    // Many small entries.
    let mut many = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for i in 0..70 {
        many.extend(entry(&format!("{TOP}/f{i}"), b'0', 0o644, b"x", ""));
    }
    expect_untouched(&sb, &finish(many), "too many");
    // Data after the end-of-archive blocks.
    let mut trailing = good_tar("0.2.0");
    trailing.extend(entry(&format!("{TOP}/late"), b'0', 0o644, b"x", ""));
    expect_untouched(&sb, &trailing, "after the end");
    // A body shorter than its header says.
    let mut short = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    short.extend_from_slice(&header(
        &format!("{TOP}/geekbridge"),
        b'0',
        0o755,
        190 << 20,
        "",
    ));
    expect_untouched(&sb, &short, "truncated");
}

#[test]
fn missing_binaries_and_non_executable_files_are_refused() {
    let sb = Sandbox::new("0.1.0");
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for n in &BINARIES[..8] {
        t.extend(entry(
            &format!("{TOP}/{n}"),
            b'0',
            0o755,
            &fake_bin(n, "0.2.0"),
            "",
        ));
    }
    expect_untouched(&sb, &finish(t), "lacks geekbridge-share");
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for n in BINARIES {
        let mode = if n == "geekbridge-ctl" { 0o644 } else { 0o755 };
        t.extend(entry(
            &format!("{TOP}/{n}"),
            b'0',
            mode,
            &fake_bin(n, "0.2.0"),
            "",
        ));
    }
    expect_untouched(&sb, &finish(t), "not marked executable");
}

#[test]
fn a_binary_that_is_not_an_executable_for_this_computer_is_refused_when_strict() {
    let sb = Sandbox::new("0.1.0");
    let a = sb.archive(&good_tar("0.2.0"));
    let st = sb.state();
    let mut r = sb.request(&a, "v0.2.0", "0.1.0", &st);
    r.strict_binaries = true;
    let err = install(&r).unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
}

#[test]
fn wrong_version_inside_the_archive_is_refused() {
    let sb = Sandbox::new("0.1.0");
    // Tagged v0.2.0, but the binary says 0.1.5 (a re-tagged or swapped archive).
    expect_untouched(&sb, &good_tar("0.1.5"), "says 0.1.5");
    // A service that prints nonsense.
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for n in BINARIES {
        let body: &[u8] = if n == "geekbridge-service" {
            b"#!/bin/sh\necho hello\n"
        } else {
            b"#!/bin/sh\n"
        };
        t.extend(entry(&format!("{TOP}/{n}"), b'0', 0o755, body, ""));
    }
    expect_untouched(&sb, &finish(t), "--version");
}

#[test]
fn checksum_size_tag_and_downgrade_rules() {
    let sb = Sandbox::new("0.1.0");
    let st = sb.state();
    let mut a = sb.archive(&good_tar("0.2.0"));
    let before = sb.listing();
    let good_sha = a.1.clone();
    a.1 = "0".repeat(64);
    assert!(
        matches!(install(&sb.request(&a, "v0.2.0", "0.1.0", &st)), Err(Error::Archive(m)) if m.contains("SHA-256"))
    );
    a.1 = good_sha.clone();
    a.2 += 1;
    assert!(
        matches!(install(&sb.request(&a, "v0.2.0", "0.1.0", &st)), Err(Error::Archive(m)) if m.contains("published"))
    );
    a.2 -= 2;
    assert!(install(&sb.request(&a, "v0.2.0", "0.1.0", &st)).is_err());
    a.2 += 1;
    // The upper-case form of the digest is the same digest.
    a.1 = good_sha.to_uppercase();
    assert!(
        install(&Request {
            dry_run: true,
            ..sb.request(&a, "v0.2.0", "0.1.0", &st)
        })
        .is_ok()
    );
    for (tag, current, word) in [
        ("v0.2.0-rc.1", "0.1.0", "pre-release"),
        ("v0.2.0", "0.2.0", "not newer"),
        ("v0.2.0", "0.3.0", "not newer"),
        ("v0.1.9", "0.2.0-rc.1", "not newer"),
        ("0.2.0", "0.1.0", "not a release tag"),
        ("v0.2", "0.1.0", "not a release tag"),
        ("v0.2.0+evil", "0.1.0", "not a release tag"),
        ("v../../x", "0.1.0", "not a release tag"),
    ] {
        let e = install(&sb.request(&a, tag, current, &st)).unwrap_err();
        assert!(
            matches!(e, Error::Refused(_)) && e.to_string().contains(word),
            "{tag} {current}: {e}"
        );
    }
    assert_eq!(sb.listing(), before);
    // The stable release is newer than its own release candidate.
    assert!(
        install(&Request {
            dry_run: true,
            ..sb.request(&a, "v0.2.0", "0.2.0-rc.1", &st)
        })
        .is_ok()
    );
}

// ---- install, dry run, rollback -----------------------------------------------------------------

#[test]
fn install_migrates_swaps_keeps_the_previous_version_and_records_the_outcome() {
    let sb = Sandbox::new("0.1.0");
    let report = sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    assert_eq!(
        (
            report.version.as_str(),
            report.previous.as_deref(),
            report.migrated
        ),
        ("0.2.0", Some("0.1.0"), true)
    );
    assert_eq!(sb.running(), "geekbridge-service 0.2.0");
    for n in BINARIES {
        let link = fs::read_link(sb.prefix.join("bin").join(n)).unwrap();
        assert_eq!(
            link,
            Path::new(&format!("../lib/geekbridge/current/bin/{n}"))
        );
    }
    assert_eq!(sb.link_name("current").as_deref(), Some("0.2.0"));
    assert_eq!(sb.link_name("previous").as_deref(), Some("0.1.0"));
    assert!(
        sb.prefix
            .join("lib/geekbridge/versions/0.2.0/doc/README.md")
            .is_file()
    );
    sb.no_debris();
    let outcome = last_outcome(&sb.state()).unwrap();
    assert!(outcome.starts_with("installed 0.2.0"), "{outcome}");
    // A second update: the oldest version goes, the one just left stays.
    let second = sb.install(&good_tar("0.3.0"), "v0.3.0", "0.2.0").unwrap();
    assert!(!second.migrated);
    assert_eq!(sb.running(), "geekbridge-service 0.3.0");
    assert_eq!(sb.link_name("previous").as_deref(), Some("0.2.0"));
    let versions = fs::read_dir(sb.prefix.join("lib/geekbridge/versions"))
        .unwrap()
        .count();
    assert_eq!(versions, 2);
    // The installed version again is refused by the version rule before anything happens.
    assert!(sb.install(&good_tar("0.3.0"), "v0.3.0", "0.3.0").is_err());
}

#[test]
fn the_old_files_stay_on_disk_so_a_running_process_is_not_disturbed() {
    let sb = Sandbox::new("0.1.0");
    sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    let old = sb
        .prefix
        .join("lib/geekbridge/versions/0.1.0/bin/geekbridge-service");
    assert_eq!(
        fs::read(&old).unwrap(),
        fake_bin("geekbridge-service", "0.1.0")
    );
}

#[test]
fn dry_run_verifies_and_validates_but_changes_nothing() {
    let sb = Sandbox::new("0.1.0");
    let before = sb.listing();
    let a = sb.archive(&good_tar("0.2.0"));
    let st = sb.state();
    let r = install(&Request {
        dry_run: true,
        ..sb.request(&a, "v0.2.0", "0.1.0", &st)
    })
    .unwrap();
    assert!(r.dry_run && r.version == "0.2.0");
    assert_eq!(sb.listing(), before);
    assert!(!st.exists(), "a dry run records nothing");
    // It still runs the checks: a wrong version fails the dry run too.
    let bad = sb.archive(&good_tar("0.1.5"));
    assert!(
        install(&Request {
            dry_run: true,
            ..sb.request(&bad, "v0.2.0", "0.1.0", &st)
        })
        .is_err()
    );
    assert_eq!(sb.listing(), before);
}

#[test]
fn rollback_goes_back_and_forward_and_refuses_without_a_previous_version() {
    let sb = Sandbox::new("0.1.0");
    assert!(
        matches!(rollback(&sb.prefix, None), Err(Error::Refused(_))),
        "legacy layout: nothing to go back to"
    );
    sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    let st = sb.state();
    let r = rollback(&sb.prefix, Some(&st)).unwrap();
    assert_eq!(
        (r.version.as_str(), r.previous.as_deref()),
        ("0.1.0", Some("0.2.0"))
    );
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
    assert!(last_outcome(&st).unwrap().starts_with("rolled_back 0.1.0"));
    rollback(&sb.prefix, Some(&st)).unwrap();
    assert_eq!(sb.running(), "geekbridge-service 0.2.0");
    // A previous version whose files were removed is not a rollback target.
    fs::remove_dir_all(sb.prefix.join("lib/geekbridge/versions/0.1.0")).unwrap();
    assert!(matches!(rollback(&sb.prefix, None), Err(Error::Refused(_))));
    assert_eq!(sb.running(), "geekbridge-service 0.2.0");
}

#[test]
fn a_user_symlink_in_bin_is_never_overwritten() {
    let sb = Sandbox::new("0.1.0");
    let own = sb.dir.path().join("my-build");
    fs::write(&own, "mine").unwrap();
    fs::remove_file(sb.prefix.join("bin/geekbridge-calls")).unwrap();
    symlink(&own, sb.prefix.join("bin/geekbridge-calls")).unwrap();
    let e = sb
        .install(&good_tar("0.2.0"), "v0.2.0", "0.1.0")
        .unwrap_err();
    assert!(matches!(e, Error::Unsupported(_)), "{e:?}");
    sb.no_debris();
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
    assert_eq!(
        fs::read_link(sb.prefix.join("bin/geekbridge-calls")).unwrap(),
        own
    );
}

// ---- interrupted swap ----------------------------------------------------------------------------

#[test]
fn a_crash_at_any_step_leaves_a_working_install_and_the_next_start_recovers() {
    // First count the steps of an uninterrupted run, then crash at each one in turn.
    let counter = Cell::new(0);
    {
        let sb = Sandbox::new("0.1.0");
        let a = sb.archive(&good_tar("0.2.0"));
        let st = sb.state();
        run(&sb.request(&a, "v0.2.0", "0.1.0", &st), &mut |_| {
            counter.set(counter.get() + 1);
            true
        })
        .unwrap();
    }
    let total = counter.get();
    assert!(total >= 15, "{total} steps");
    for crash_at in 1..=total {
        let sb = Sandbox::new("0.1.0");
        let a = sb.archive(&good_tar("0.2.0"));
        let st = sb.state();
        let seen = Cell::new(0);
        let r = run(&sb.request(&a, "v0.2.0", "0.1.0", &st), &mut |_| {
            seen.set(seen.get() + 1);
            seen.get() != crash_at
        });
        assert_eq!(r.unwrap_err(), Error::Interrupted, "step {crash_at}");
        // Whatever the state, every executable resolves and runs, on the old or the new version.
        for n in BINARIES {
            let out = std::process::Command::new(sb.prefix.join("bin").join(n))
                .output()
                .unwrap();
            let text = String::from_utf8(out.stdout).unwrap();
            assert!(
                text.starts_with(n) || text.starts_with("geekbridge-service"),
                "step {crash_at}: {n}: {text}"
            );
            assert!(
                text.contains("0.1.0") || text.contains("0.2.0"),
                "step {crash_at}: {n}: {text}"
            );
        }
        // Recovery on the next start removes the debris and leaves a consistent state.
        recover(&sb.prefix, Some(&st));
        sb.no_debris();
        let running = sb.running();
        assert!(
            running.ends_with("0.1.0") || running.ends_with("0.2.0"),
            "{running}"
        );
        if let Some(cur) = sb.link_name("current") {
            assert!(
                sb.prefix
                    .join("lib/geekbridge/versions")
                    .join(&cur)
                    .join("bin/geekbridge-service")
                    .is_file(),
                "step {crash_at}"
            );
            assert!(
                running.ends_with(&cur),
                "step {crash_at}: {running} vs {cur}"
            );
        }
        // And simply running the update again finishes the job.
        let again = sb
            .install(&good_tar("0.2.0"), "v0.2.0", "0.1.0")
            .unwrap_or_else(|e| panic!("step {crash_at}: {e}"));
        assert_eq!(again.version, "0.2.0");
        assert_eq!(sb.running(), "geekbridge-service 0.2.0", "step {crash_at}");
        sb.no_debris();
    }
}

#[test]
fn recover_repoints_an_unusable_current_and_ignores_a_live_install() {
    let sb = Sandbox::new("0.1.0");
    sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    fs::remove_file(
        sb.prefix
            .join("lib/geekbridge/versions/0.2.0/bin/geekbridge-service"),
    )
    .unwrap();
    let st = sb.state();
    let notes = recover(&sb.prefix, Some(&st));
    assert!(
        notes.iter().any(|n| n.contains("v0.2.0 was unusable")),
        "{notes:?}"
    );
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
    assert!(last_outcome(&st).unwrap().starts_with("recovered 0.1.0"));
    // An install holding the lock (a live pid) is left alone.
    let lockfile = sb.prefix.join("lib/geekbridge/.lock");
    fs::write(&lockfile, "1").unwrap();
    fs::create_dir(sb.prefix.join("lib/geekbridge/.staging-1")).unwrap();
    assert!(recover(&sb.prefix, None).is_empty());
    assert!(sb.prefix.join("lib/geekbridge/.staging-1").exists());
    assert!(matches!(rollback(&sb.prefix, None), Err(Error::Busy)));
    // A lock left by a dead process is taken over.
    fs::write(&lockfile, "4000000000").unwrap();
    assert!(!recover(&sb.prefix, None).is_empty());
    assert!(!sb.prefix.join("lib/geekbridge/.staging-1").exists());
}

// ---- who may update ------------------------------------------------------------------------------

#[test]
fn prefix_of_exe_understands_both_layouts() {
    let p = |s: &str| prefix_of_exe(Path::new(s));
    assert_eq!(
        p("/home/u/.local/bin/geekbridge-ctl"),
        Some("/home/u/.local".into())
    );
    assert_eq!(
        p("/home/u/.local/lib/geekbridge/versions/0.2.0/bin/geekbridge-service"),
        Some("/home/u/.local".into())
    );
    assert_eq!(p("/usr/bin/geekbridge"), Some("/usr".into()));
    assert_eq!(p("/geekbridge-ctl"), None);
}

#[test]
fn user_install_is_detected_and_packages_and_other_places_are_not_touched() {
    let sb = Sandbox::new("0.1.0");
    let home = sb.dir.path();
    assert!(matches!(
        support_for(&sb.prefix, Some(home)),
        Support::UserInstall(_)
    ));
    assert!(matches!(
        support_for(&sb.prefix, None),
        Support::UserInstall(_)
    ));
    // Outside the home directory (a /usr or /usr/local install): not ours to change.
    let other = tempfile::tempdir().unwrap();
    let s = support_for(&sb.prefix, Some(other.path()));
    assert!(
        matches!(&s, Support::PackageManaged(w) if w.contains("outside your home")),
        "{s:?}"
    );
    assert!(s.message().contains("package manager") && !s.can_install());
    assert!(matches!(
        support_for(Path::new("/usr"), Some(home)),
        Support::PackageManaged(_)
    ));
    // The package's own marker wins even inside the home directory and when writable.
    fs::create_dir_all(sb.prefix.join("share/geekbridge")).unwrap();
    fs::write(sb.prefix.join("share/geekbridge/package-managed"), "deb\n").unwrap();
    let s = support_for(&sb.prefix, Some(home));
    assert!(
        matches!(&s, Support::PackageManaged(w) if w.contains("package")),
        "{s:?}"
    );
    fs::remove_file(sb.prefix.join("share/geekbridge/package-managed")).unwrap();
    // A bin directory the user cannot write.
    fs::set_permissions(sb.prefix.join("bin"), fs::Permissions::from_mode(0o555)).unwrap();
    let root = fs::read_to_string("/proc/self/status")
        .unwrap()
        .lines()
        .any(|l| l.starts_with("Uid:\t0\t"));
    let s = support_for(&sb.prefix, Some(home));
    fs::set_permissions(sb.prefix.join("bin"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(
        root || matches!(&s, Support::PackageManaged(w) if w.contains("not writable")),
        "{s:?}"
    );
    // A prefix with no bin directory at all.
    assert!(matches!(
        support_for(&sb.dir.path().join("nope"), None),
        Support::PackageManaged(_)
    ));
    assert!(
        Support::DownloadOnly("x".into())
            .message()
            .starts_with("Download only")
    );
}

// ---- restart and calls --------------------------------------------------------------------------

struct Fake {
    running: RefCell<Option<String>>,
    after_restart: Option<String>,
    calls: Result<bool, String>,
    restarts: Cell<u32>,
}
impl Fake {
    fn new(running: Option<&str>, after: Option<&str>, calls: Result<bool, String>) -> Fake {
        Fake {
            running: RefCell::new(running.map(str::to_owned)),
            after_restart: after.map(str::to_owned),
            calls,
            restarts: Cell::new(0),
        }
    }
}
impl Probe for Fake {
    fn calls_active(&self) -> Result<bool, String> {
        self.calls.clone()
    }
    fn running_version(&self) -> Option<String> {
        self.running.borrow().clone()
    }
    fn restart(&self) -> Result<(), String> {
        self.restarts.set(self.restarts.get() + 1);
        *self.running.borrow_mut() = self.after_restart.clone();
        Ok(())
    }
}

#[test]
fn restart_is_refused_during_a_call_and_when_the_call_state_is_unknown() {
    let sb = Sandbox::new("0.1.0");
    sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    let w = Duration::from_millis(100);
    let busy = Fake::new(Some("0.1.0"), Some("0.2.0"), Ok(true));
    let e = restart(&busy, &sb.prefix, "0.2.0", w, true, None).unwrap_err();
    assert!(matches!(e, Error::CallInProgress(_)) && e.to_string().contains("call is in progress"));
    assert_eq!(
        busy.restarts.get(),
        0,
        "the service must not be restarted during a call"
    );
    let unknown = Fake::new(Some("0.1.0"), Some("0.2.0"), Err("timeout".into()));
    assert!(matches!(
        restart(&unknown, &sb.prefix, "0.2.0", w, true, None),
        Err(Error::CallInProgress(_))
    ));
    assert_eq!(unknown.restarts.get(), 0);
    // The installed files are untouched by a refused restart.
    assert_eq!(sb.running(), "geekbridge-service 0.2.0");
}

#[test]
fn restart_when_idle_checks_the_new_version_and_rolls_back_an_unhealthy_one() {
    let sb = Sandbox::new("0.1.0");
    sb.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    let w = Duration::from_millis(200);
    let idle = Fake::new(Some("0.1.0"), Some("0.2.0"), Ok(false));
    assert_eq!(
        restart(&idle, &sb.prefix, "0.2.0", w, true, None),
        Ok(Restarted::Restarted)
    );
    assert_eq!(idle.restarts.get(), 1);
    // Not running at all: nothing to restart, no call can be active.
    let off = Fake::new(None, None, Err("down".into()));
    assert_eq!(
        restart(&off, &sb.prefix, "0.2.0", w, true, None),
        Ok(Restarted::NotRunning)
    );
    assert_eq!(off.restarts.get(), 0);
    // The new version does not come up: back to the old files, restarted again.
    let st = sb.state();
    let broken = Fake::new(Some("0.1.0"), None, Ok(false));
    let e = restart(&broken, &sb.prefix, "0.2.0", w, true, Some(&st)).unwrap_err();
    assert!(
        matches!(e, Error::Unhealthy(_)) && e.to_string().contains("went back to v0.1.0"),
        "{e}"
    );
    assert_eq!(broken.restarts.get(), 2);
    assert_eq!(sb.running(), "geekbridge-service 0.1.0");
    assert!(
        last_outcome(&st)
            .unwrap()
            .starts_with("rolled_back_unhealthy")
    );
    // Without auto-rollback (the rollback command itself) it only reports.
    let sb2 = Sandbox::new("0.1.0");
    sb2.install(&good_tar("0.2.0"), "v0.2.0", "0.1.0").unwrap();
    let broken = Fake::new(Some("0.1.0"), None, Ok(false));
    assert!(restart(&broken, &sb2.prefix, "0.2.0", w, false, None).is_err());
    assert_eq!(broken.restarts.get(), 1);
    assert_eq!(sb2.running(), "geekbridge-service 0.2.0");
}

/// With real release binaries: `GB_REAL_BIN_DIR=target/release cargo test -p bridge-update real_binaries -- --ignored`.
/// Uses strict validation (ELF, size, the staged `geekbridge-service --version`), in a temporary prefix.
#[test]
#[ignore = "needs GB_REAL_BIN_DIR with built release binaries"]
fn real_binaries_pass_strict_validation_and_install() {
    let Some(dir) = std::env::var_os("GB_REAL_BIN_DIR").map(PathBuf::from) else {
        return;
    };
    let version = env!("CARGO_PKG_VERSION");
    let real = |n: &str| {
        fs::read(dir.join(n)).unwrap_or_else(|_| fs::read(dir.join("geekbridge-service")).unwrap())
    };
    let sb = Sandbox::new("0.0.1");
    let mut t = entry(&format!("{TOP}/"), b'5', 0o755, &[], "");
    for n in BINARIES {
        t.extend(entry(&format!("{TOP}/{n}"), b'0', 0o755, &real(n), ""));
    }
    let a = sb.archive(&finish(t));
    let st = sb.state();
    let tag = format!("v{version}");
    let mut r = sb.request(&a, &tag, "0.0.1", &st);
    r.strict_binaries = true;
    let report = install(&r).unwrap();
    assert_eq!(report.version, version);
    let out = std::process::Command::new(sb.prefix.join("bin/geekbridge-service"))
        .arg("--version")
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("geekbridge-service {version}")
    );
}
