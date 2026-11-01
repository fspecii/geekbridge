//! Windows backend.
//!
//! "Private" means: the object is owned by the current user and its DACL is
//! *protected* (no inherited entries) and contains only allow entries for the
//! current user and `LocalSystem` (`S-1-5-18`). Administrators are deliberately
//! not listed: they can still take ownership, but that is an explicit act and
//! not ambient access. Everything is checked on an open handle opened with
//! `FILE_FLAG_OPEN_REPARSE_POINT`, so a symbolic link, junction or other
//! reparse point is seen as itself and refused rather than followed.
//!
//! This code has been type-checked against the Windows target but **not run on
//! Windows hardware**; see `docs/architecture.md`.
//!
//! References: `CreateFileW`, `CreateDirectoryW`, `GetSecurityInfo`,
//! `GetSecurityDescriptorControl`, `GetAce`, `MoveFileExW`, `LockFileEx`,
//! "Security Descriptor String Format" and "Access Control Lists" on
//! learn.microsoft.com.

use std::ffi::c_void;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::MetadataExt;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::ptr::{addr_of, null, null_mut};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_ALREADY_EXISTS, ERROR_INSUFFICIENT_BUFFER, ERROR_LOCK_VIOLATION,
    ERROR_SHARING_VIOLATION, ERROR_SUCCESS, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo,
    SDDL_REVISION_1, SE_FILE_OBJECT,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION, EqualSid, GetAce,
    GetSecurityDescriptorControl, GetTokenInformation, INHERITED_ACE, IsValidSid,
    OWNER_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_GENERIC_READ, FILE_GENERIC_WRITE, FILE_LIST_DIRECTORY,
    FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TYPE_DISK,
    GetFileType, GetVolumeInformationByHandleW, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
    LockFileEx, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW, OPEN_ALWAYS,
    OPEN_EXISTING, READ_CONTROL, UnlockFileEx,
};
use windows_sys::Win32::System::IO::OVERLAPPED;
use windows_sys::Win32::System::SystemServices::{ACCESS_ALLOWED_ACE_TYPE, ACCESS_DENIED_ACE_TYPE};
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

use crate::error::{KdeError, Result};

use super::MAX_STATE_FILE;

/// `FILE_PERSISTENT_ACLS` from `GetVolumeInformation`: the volume stores and
/// enforces ACLs (NTFS/ReFS yes, FAT/exFAT no).
const FILE_PERSISTENT_ACLS: u32 = 0x0000_0008;
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x0000_0400;

/// `S-1-5-18` (LocalSystem) as a binary SID: revision 1, one sub-authority,
/// identifier authority 5 (NT), sub-authority 18. A `u32` array guarantees the
/// 4-byte alignment the SID functions expect.
static SYSTEM_SID: [u32; 3] = [
    u32::from_le_bytes([1, 1, 0, 0]),
    u32::from_le_bytes([0, 0, 0, 5]),
    18,
];
const SYSTEM_SID_STRING: &str = "S-1-5-18";

fn system_sid() -> PSID {
    SYSTEM_SID.as_ptr() as PSID
}

fn insecure(what: &str, detail: impl Into<String>) -> KdeError {
    KdeError::InsecurePermissions {
        what: what.to_owned(),
        detail: detail.into(),
    }
}

/// Null-terminated UTF-16 for a Win32 `W` call.
fn wide(path: &Path) -> io::Result<Vec<u16>> {
    let mut v: Vec<u16> = path.as_os_str().encode_wide().collect();
    if v.contains(&0) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "path contains a NUL character",
        ));
    }
    v.push(0);
    Ok(v)
}

fn wide_str(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Memory allocated by the system for us and released with `LocalFree`
/// (security descriptors from SDDL, strings from `ConvertSidToStringSidW`,
/// the descriptor behind `GetSecurityInfo` pointers).
struct LocalMem(*mut c_void);

impl Drop for LocalMem {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the pointer came from an API documented to allocate with
            // LocalAlloc and to require LocalFree, and is freed exactly once.
            unsafe { LocalFree(self.0) };
        }
    }
}

fn sid_to_string(sid: PSID) -> Result<String> {
    let mut out: *mut u16 = null_mut();
    // SAFETY: `sid` is a valid SID pointer supplied by the caller; on success
    // `out` receives a LocalAlloc'd NUL-terminated string.
    if unsafe { ConvertSidToStringSidW(sid, &mut out) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    let _guard = LocalMem(out.cast());
    let mut len = 0;
    // SAFETY: the string is NUL-terminated, so reading up to the terminator
    // stays inside the allocation.
    while unsafe { *out.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` elements were just verified readable.
    Ok(String::from_utf16_lossy(unsafe {
        std::slice::from_raw_parts(out, len)
    }))
}

/// The user SID of the process token (not of any thread impersonation token:
/// the daemon's own identity is what owns its state).
struct CurrentUser {
    /// Backing storage of a `TOKEN_USER` followed by the SID it points to.
    /// `u64` elements give the pointer-sized alignment `TOKEN_USER` needs.
    token_user: Vec<u64>,
    sid_string: String,
}

impl CurrentUser {
    fn load() -> Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo handle that must not be
        // closed; OpenProcessToken stores a new handle in `token` on success.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: `token` is a fresh handle that we own; OwnedHandle closes it.
        let token = unsafe { OwnedHandle::from_raw_handle(token) };

        let mut needed = 0u32;
        // SAFETY: a null buffer with length 0 is the documented way to ask for
        // the required size; the call is expected to fail with
        // ERROR_INSUFFICIENT_BUFFER.
        unsafe {
            GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut needed)
        };
        let probe = io::Error::last_os_error();
        if needed == 0 || probe.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER as i32) {
            return Err(probe.into());
        }
        let mut buf = vec![0u64; (needed as usize).div_ceil(8)];
        // SAFETY: `buf` is writable for `buf.len() * 8 >= needed` bytes.
        if unsafe {
            GetTokenInformation(
                token.as_raw_handle(),
                TokenUser,
                buf.as_mut_ptr().cast(),
                (buf.len() * 8) as u32,
                &mut needed,
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let mut me = Self {
            token_user: buf,
            sid_string: String::new(),
        };
        let sid = me.sid();
        // SAFETY: `sid` points into `me.token_user`, filled in by the call above.
        if sid.is_null() || unsafe { IsValidSid(sid) } == 0 {
            return Err(KdeError::Store(
                "process token has no valid user SID".into(),
            ));
        }
        me.sid_string = sid_to_string(sid)?;
        Ok(me)
    }

    /// Pointer into `self.token_user`; valid for as long as `self` lives.
    fn sid(&self) -> PSID {
        // SAFETY: `token_user` holds a TOKEN_USER written by GetTokenInformation
        // (aligned: it is a Vec<u64>); its SID pointer targets the same buffer.
        unsafe { (*self.token_user.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }

    fn is_trusted(&self, sid: PSID) -> bool {
        // SAFETY: both SIDs are valid; callers validate `sid` with IsValidSid.
        unsafe { EqualSid(sid, self.sid()) != 0 || EqualSid(sid, system_sid()) != 0 }
    }
}

/// `SECURITY_ATTRIBUTES` carrying the private descriptor. The handle is not
/// inheritable by child processes.
struct PrivateAttrs {
    attrs: SECURITY_ATTRIBUTES,
    _descriptor: LocalMem,
}

impl PrivateAttrs {
    /// Owner = current user; DACL protected (`P`) with full control for the
    /// current user and LocalSystem only. Directories also get container and
    /// object inherit so anything created in them starts out private as well.
    fn new(user: &CurrentUser, directory: bool) -> Result<Self> {
        let inherit = if directory { "OICI" } else { "" };
        let sid = &user.sid_string;
        let mut sddl = format!("O:{sid}D:P(A;{inherit};FA;;;{sid})");
        if sid != SYSTEM_SID_STRING {
            sddl.push_str(&format!("(A;{inherit};FA;;;SY)"));
        }
        let wide_sddl = wide_str(&sddl);
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: `wide_sddl` is NUL-terminated; on success `sd` receives a
        // LocalAlloc'd self-relative descriptor.
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                wide_sddl.as_ptr(),
                SDDL_REVISION_1,
                &mut sd,
                null_mut(),
            )
        } == 0
        {
            return Err(io::Error::last_os_error().into());
        }
        let descriptor = LocalMem(sd);
        Ok(Self {
            attrs: SECURITY_ATTRIBUTES {
                nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd,
                bInheritHandle: 0,
            },
            _descriptor: descriptor,
        })
    }

    fn as_ptr(&self) -> *const SECURITY_ATTRIBUTES {
        &self.attrs
    }
}

/// Check owner and DACL of an open handle (`what` is for messages only).
fn verify_private(handle: HANDLE, what: &str, user: &CurrentUser) -> Result<()> {
    let mut owner: PSID = null_mut();
    let mut dacl: *mut ACL = null_mut();
    let mut sd: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `handle` is a live handle with READ_CONTROL access. The out
    // pointers reference our locals; `owner` and `dacl` point *into* the
    // descriptor returned in `sd`, which we free via `_sd` after the last use.
    let rc = unsafe {
        GetSecurityInfo(
            handle,
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut sd,
        )
    };
    let _sd = LocalMem(sd);
    if rc != ERROR_SUCCESS {
        return Err(io::Error::from_raw_os_error(rc as i32).into());
    }
    // SAFETY: `owner` was just returned by GetSecurityInfo (null-checked first).
    if owner.is_null() || unsafe { IsValidSid(owner) } == 0 {
        return Err(insecure(what, "has no owner"));
    }
    if !is_same(owner, user.sid()) {
        let other = sid_to_string(owner).unwrap_or_else(|_| "an unknown principal".into());
        return Err(insecure(
            what,
            format!(
                "owned by {other}, not by the current user ({})",
                user.sid_string
            ),
        ));
    }
    if dacl.is_null() {
        return Err(insecure(
            what,
            "has no DACL, which grants everyone full access",
        ));
    }
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: `sd` is the valid descriptor returned above.
    if unsafe { GetSecurityDescriptorControl(sd, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error().into());
    }
    if control & SE_DACL_PROTECTED == 0 {
        return Err(insecure(
            what,
            "DACL is not protected, so it inherits permissions from the parent folder",
        ));
    }
    // SAFETY: `dacl` is non-null and points at a valid ACL header.
    let count = u32::from(unsafe { (*dacl).AceCount });
    for index in 0..count {
        let mut ace: *mut c_void = null_mut();
        // SAFETY: `index < AceCount`; on success `ace` points at the ACE
        // inside `dacl`, which outlives this loop.
        if unsafe { GetAce(dacl, index, &mut ace) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        // SAFETY: every ACE starts with an ACE_HEADER.
        let header = unsafe { *ace.cast::<ACE_HEADER>() };
        match u32::from(header.AceType) {
            // A deny entry can only remove access, never add it.
            ACCESS_DENIED_ACE_TYPE => continue,
            ACCESS_ALLOWED_ACE_TYPE => {}
            other => {
                return Err(insecure(
                    what,
                    format!("DACL contains an entry of unsupported type {other}"),
                ));
            }
        }
        if u32::from(header.AceFlags) & INHERITED_ACE != 0 {
            return Err(insecure(what, "DACL contains an inherited entry"));
        }
        if usize::from(header.AceSize) < size_of::<ACCESS_ALLOWED_ACE>() {
            return Err(insecure(what, "DACL contains a malformed entry"));
        }
        // SAFETY: the type is ACCESS_ALLOWED_ACE and AceSize covers its fixed
        // part; the SID starts at `SidStart`.
        let ace_sid = unsafe { addr_of!((*ace.cast::<ACCESS_ALLOWED_ACE>()).SidStart) } as PSID;
        // SAFETY: as above, the SID lies within the ACE.
        if unsafe { IsValidSid(ace_sid) } == 0 {
            return Err(insecure(what, "DACL contains an entry with an invalid SID"));
        }
        if !user.is_trusted(ace_sid) {
            let who = sid_to_string(ace_sid).unwrap_or_else(|_| "an unknown principal".into());
            return Err(insecure(what, format!("DACL grants access to {who}")));
        }
    }
    Ok(())
}

fn is_same(a: PSID, b: PSID) -> bool {
    // SAFETY: both pointers are valid SIDs (validated by callers).
    unsafe { EqualSid(a, b) != 0 }
}

/// Full check of an opened object: not a reparse point, expected kind, on a
/// volume that enforces ACLs, and private by [`verify_private`].
fn verify_object(file: &File, what: &str, directory: bool, user: &CurrentUser) -> Result<()> {
    let md = file.metadata()?;
    if md.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(insecure(
            what,
            "is a reparse point (symbolic link, junction or placeholder)",
        ));
    }
    if md.is_dir() != directory {
        return Err(insecure(
            what,
            if directory {
                "not a directory"
            } else {
                "not a regular file"
            },
        ));
    }
    let handle = file.as_raw_handle();
    if !directory {
        // SAFETY: `handle` is a live file handle.
        if unsafe { GetFileType(handle) } != FILE_TYPE_DISK {
            return Err(insecure(what, "not a disk file"));
        }
    }
    let mut flags = 0u32;
    // SAFETY: only the file-system-flags out parameter is requested; the other
    // out pointers are null with zero sizes as documented.
    if unsafe {
        GetVolumeInformationByHandleW(
            handle,
            null_mut(),
            0,
            null_mut(),
            null_mut(),
            &mut flags,
            null_mut(),
            0,
        )
    } == 0
    {
        return Err(io::Error::last_os_error().into());
    }
    if flags & FILE_PERSISTENT_ACLS == 0 {
        return Err(insecure(
            what,
            "is on a volume that does not store access control lists (FAT/exFAT?)",
        ));
    }
    verify_private(handle, what, user)
}

fn open(
    path: &Path,
    access: u32,
    share: u32,
    attrs: *const SECURITY_ATTRIBUTES,
    disposition: u32,
    flags: u32,
) -> io::Result<File> {
    let path = wide(path)?;
    // SAFETY: `path` is NUL-terminated, `attrs` is null or points at a live
    // SECURITY_ATTRIBUTES for the duration of the call, no template file.
    let h = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            share,
            attrs,
            disposition,
            flags,
            null_mut(),
        )
    };
    if h == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new handle that nothing else owns.
    Ok(File::from(unsafe { OwnedHandle::from_raw_handle(h) }))
}

/// Create `dir` with the private descriptor if needed, then open it (without
/// following a reparse point) and require it to be private. The returned
/// handle denies delete/rename sharing while held (best-effort hardening for
/// the duration of a write).
fn open_private_dir(dir: &Path, user: &CurrentUser) -> Result<File> {
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        // Ancestors get ordinary default ACLs; only the state directory itself
        // is trusted, and it is verified below.
        fs::create_dir_all(parent)?;
    }
    let attrs = PrivateAttrs::new(user, true)?;
    let wide_dir = wide(dir)?;
    // SAFETY: NUL-terminated path; `attrs` outlives the call.
    if unsafe { CreateDirectoryW(wide_dir.as_ptr(), attrs.as_ptr()) } == 0 {
        let e = io::Error::last_os_error();
        if e.raw_os_error() != Some(ERROR_ALREADY_EXISTS as i32) {
            return Err(e.into());
        }
    }
    let f = open(
        dir,
        FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES | READ_CONTROL,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        null(),
        OPEN_EXISTING,
        FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
    )?;
    verify_object(
        &f,
        &format!("state directory {}", dir.display()),
        true,
        user,
    )?;
    Ok(f)
}

/// Create `dir` if needed and require that it is a real directory (not a
/// reparse point) owned by the current user with a protected DACL that grants
/// only the user and LocalSystem.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    let user = CurrentUser::load()?;
    open_private_dir(dir, &user).map(|_| ())
}

/// Read a private file, verifying type, owner and DACL on the open handle.
/// `None` if absent.
pub fn read_private_file(path: &Path) -> Result<Option<Vec<u8>>> {
    let user = CurrentUser::load()?;
    let what = format!("file {}", path.display());
    let f = match open(
        path,
        FILE_GENERIC_READ,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        null(),
        OPEN_EXISTING,
        FILE_FLAG_OPEN_REPARSE_POINT,
    ) {
        Ok(f) => f,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    verify_object(&f, &what, false, &user)?;
    let len = f.metadata()?.len();
    if len > MAX_STATE_FILE {
        return Err(KdeError::Store(format!(
            "{what} is larger than {MAX_STATE_FILE} bytes"
        )));
    }
    let mut buf = Vec::with_capacity(len as usize);
    (&f).take(MAX_STATE_FILE + 1).read_to_end(&mut buf)?;
    if buf.len() as u64 > MAX_STATE_FILE {
        return Err(KdeError::Store(format!(
            "{what} is larger than {MAX_STATE_FILE} bytes"
        )));
    }
    Ok(Some(buf))
}

/// Write `data` to `path` atomically: a new exclusive temp file in the same
/// directory is created with the private descriptor, re-verified, written and
/// flushed, then moved over the target with `MoveFileExW`
/// (`REPLACE_EXISTING | WRITE_THROUGH`). Windows has no directory fsync;
/// `WRITE_THROUGH` is the documented way to have the rename reach the disk
/// before the call returns.
pub fn atomic_write_private(path: &Path, data: &[u8]) -> Result<()> {
    if data.len() as u64 > MAX_STATE_FILE {
        return Err(KdeError::Store(format!(
            "refusing to write {} bytes: more than the {MAX_STATE_FILE} byte limit that can be read back",
            data.len()
        )));
    }
    let dir = path
        .parent()
        .ok_or_else(|| KdeError::Store("path has no parent".into()))?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| KdeError::Store("bad file name".into()))?;
    let user = CurrentUser::load()?;
    let dir_guard = open_private_dir(dir, &user)?;
    let attrs = PrivateAttrs::new(&user, false)?;
    let tmp: PathBuf = dir.join(format!(
        ".{name}.tmp.{}.{}",
        std::process::id(),
        rand_suffix()
    ));
    let result = (|| -> Result<()> {
        // CREATE_NEW never opens an existing object (including a planted
        // link); share mode 0 keeps everyone else out until we are done.
        let mut f = open(
            &tmp,
            // READ_ATTRIBUTES: FILE_GENERIC_WRITE lacks it, but verify_object
            // needs to query the handle.
            FILE_GENERIC_WRITE | FILE_READ_ATTRIBUTES,
            0,
            attrs.as_ptr(),
            CREATE_NEW,
            FILE_FLAG_OPEN_REPARSE_POINT,
        )?;
        // Prove the descriptor really applied (e.g. not silently ignored on a
        // volume without ACLs) before any secret reaches the disk.
        verify_object(&f, &format!("file {}", tmp.display()), false, &user)?;
        f.write_all(data)?;
        f.sync_all()?;
        drop(f);
        move_replace(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    drop(dir_guard);
    result
}

fn move_replace(from: &Path, to: &Path) -> Result<()> {
    let (from, to) = (wide(from)?, wide(to)?);
    let mut attempt = 0;
    loop {
        // SAFETY: both paths are NUL-terminated and live across the call.
        if unsafe {
            MoveFileExW(
                from.as_ptr(),
                to.as_ptr(),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        } != 0
        {
            return Ok(());
        }
        let err = io::Error::last_os_error();
        // A reader (antivirus, indexer, backup) can briefly hold the target
        // without FILE_SHARE_DELETE.
        let transient = matches!(
            err.raw_os_error().map(|c| c as u32),
            Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)
        );
        attempt += 1;
        if !transient || attempt >= 20 {
            return Err(err.into());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let t = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    (t as u64) ^ (u64::from(std::process::id()) << 32)
}

/// Exclusive byte-range lock (`LockFileEx`) on byte 0 of a private lock file.
/// Released on drop or by the OS when the process dies.
#[derive(Debug)]
pub struct FileLock {
    file: File,
}

impl Drop for FileLock {
    fn drop(&mut self) {
        let mut ov = OVERLAPPED::default();
        // SAFETY: the handle is live; the range matches the one locked. The
        // result is ignored because closing the handle releases the lock anyway.
        unsafe { UnlockFileEx(self.file.as_raw_handle(), 0, 1, 0, &mut ov) };
    }
}

pub fn lock_private_file(path: &Path, timeout: Duration) -> Result<FileLock> {
    let dir = path
        .parent()
        .ok_or_else(|| KdeError::Store("lock path has no parent".into()))?;
    let user = CurrentUser::load()?;
    drop(open_private_dir(dir, &user)?);
    let attrs = PrivateAttrs::new(&user, false)?;
    let file = open(
        path,
        FILE_GENERIC_READ | FILE_GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        attrs.as_ptr(),
        OPEN_ALWAYS,
        FILE_FLAG_OPEN_REPARSE_POINT,
    )?;
    verify_object(
        &file,
        &format!("lock file {}", path.display()),
        false,
        &user,
    )?;
    let deadline = Instant::now() + timeout;
    loop {
        let mut ov = OVERLAPPED::default();
        // SAFETY: the handle is live; `ov` is zeroed (offset 0) and not used
        // for overlapped I/O since the handle was opened synchronously.
        let ok = unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut ov,
            )
        };
        if ok != 0 {
            return Ok(FileLock { file });
        }
        let err = io::Error::last_os_error();
        if err.raw_os_error() != Some(ERROR_LOCK_VIOLATION as i32) {
            return Err(err.into());
        }
        if Instant::now() >= deadline {
            return Err(KdeError::Store(format!(
                "timed out after {timeout:?} waiting for another process to release {}",
                path.display()
            )));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// These run only on Windows (`cargo test -p bridge-kde` there); on this
/// project's Linux CI they are type-checked via the Windows target but not
/// executed. They exercise the real Win32 ACL paths.
#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::Security::Authorization::SetSecurityInfo;
    use windows_sys::Win32::Security::{
        GetSecurityDescriptorDacl, PROTECTED_DACL_SECURITY_INFORMATION,
        UNPROTECTED_DACL_SECURITY_INFORMATION,
    };
    use windows_sys::Win32::Storage::FileSystem::WRITE_DAC;

    fn user_sid() -> String {
        CurrentUser::load().unwrap().sid_string
    }

    /// Replace the DACL of `path` with the one described by `sddl`.
    fn set_dacl(path: &Path, sddl: &str, protected: bool) {
        let f = open(
            path,
            WRITE_DAC | READ_CONTROL,
            FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
            null(),
            OPEN_EXISTING,
            FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT,
        )
        .unwrap();
        let wide_sddl = wide_str(sddl);
        let mut sd: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: NUL-terminated SDDL; `sd` receives a LocalAlloc'd descriptor.
        assert_ne!(
            unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide_sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut sd,
                    null_mut(),
                )
            },
            0
        );
        let _sd = LocalMem(sd);
        let (mut present, mut defaulted) = (0, 0);
        let mut dacl: *mut ACL = null_mut();
        // SAFETY: `sd` is the valid descriptor from above.
        assert_ne!(
            unsafe { GetSecurityDescriptorDacl(sd, &mut present, &mut dacl, &mut defaulted) },
            0
        );
        let flag = if protected {
            PROTECTED_DACL_SECURITY_INFORMATION
        } else {
            UNPROTECTED_DACL_SECURITY_INFORMATION
        };
        // SAFETY: live handle with WRITE_DAC; `dacl` is null (NULL DACL) or
        // points into `sd`, which outlives the call.
        let rc = unsafe {
            SetSecurityInfo(
                f.as_raw_handle(),
                SE_FILE_OBJECT,
                DACL_SECURITY_INFORMATION | flag,
                null_mut(),
                null_mut(),
                dacl,
                null_mut(),
            )
        };
        assert_eq!(rc, ERROR_SUCCESS, "SetSecurityInfo failed");
    }

    fn is_insecure<T: std::fmt::Debug>(r: Result<T>, needle: &str) -> bool {
        match r {
            Err(KdeError::InsecurePermissions { detail, .. }) => {
                let ok = detail.contains(needle);
                if !ok {
                    eprintln!("unexpected detail: {detail}");
                }
                ok
            }
            other => {
                eprintln!("expected InsecurePermissions, got {other:?}");
                false
            }
        }
    }

    #[test]
    fn system_sid_constant_is_local_system() {
        // SAFETY: the static is a well-formed SID.
        assert_ne!(unsafe { IsValidSid(system_sid()) }, 0);
        assert_eq!(sid_to_string(system_sid()).unwrap(), SYSTEM_SID_STRING);
    }

    #[test]
    fn write_read_replace_roundtrip_is_private() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let file = dir.join("f");
        assert!(read_private_file(&file).unwrap().is_none());
        atomic_write_private(&file, b"one").unwrap();
        atomic_write_private(&file, b"two").unwrap();
        assert_eq!(read_private_file(&file).unwrap().unwrap(), b"two");
        ensure_private_dir(&dir).unwrap();
        // No temp files left behind.
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
    }

    #[test]
    fn broad_access_is_refused_on_files_and_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let file = dir.join("f");
        atomic_write_private(&file, b"x").unwrap();
        let me = user_sid();

        // Everyone (WD) granted read next to the legitimate entries.
        set_dacl(
            &file,
            &format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)(A;;FR;;;WD)"),
            true,
        );
        assert!(is_insecure(read_private_file(&file), "grants access to"));
        // Administrators are deliberately not trusted either.
        set_dacl(&file, &format!("D:P(A;;FA;;;{me})(A;;FA;;;BA)"), true);
        assert!(is_insecure(read_private_file(&file), "grants access to"));
        // NULL DACL = everyone.
        set_dacl(&file, "D:NO_ACCESS_CONTROL", true);
        assert!(is_insecure(read_private_file(&file), "no DACL"));
        // Right principals but inheriting from the parent.
        set_dacl(&file, &format!("D:(A;;FA;;;{me})(A;;FA;;;SY)"), false);
        assert!(is_insecure(read_private_file(&file), "inherit"));
        // Restored: accepted again, and the write path repairs nothing silently.
        set_dacl(&file, &format!("D:P(A;;FA;;;{me})(A;;FA;;;SY)"), true);
        assert!(read_private_file(&file).unwrap().is_some());

        set_dacl(
            &dir,
            &format!("D:P(A;OICI;FA;;;{me})(A;OICI;FA;;;SY)(A;OICI;FR;;;AU)"),
            true,
        );
        assert!(is_insecure(ensure_private_dir(&dir), "grants access to"));
        assert!(is_insecure(
            atomic_write_private(&file, b"y"),
            "grants access to"
        ));
    }

    #[test]
    fn symlinks_are_refused_not_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("state");
        let file = dir.join("f");
        atomic_write_private(&file, b"x").unwrap();
        let link = dir.join("link");
        if let Err(e) = std::os::windows::fs::symlink_file(&file, &link) {
            // Creating symlinks needs Developer Mode or SeCreateSymbolicLinkPrivilege.
            eprintln!("skipping: cannot create a symlink here ({e})");
            return;
        }
        assert!(is_insecure(read_private_file(&link), "reparse point"));
        let dlink = tmp.path().join("dlink");
        if std::os::windows::fs::symlink_dir(&dir, &dlink).is_ok() {
            assert!(is_insecure(ensure_private_dir(&dlink), "reparse point"));
        }
    }

    #[test]
    fn oversize_write_is_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("state").join("f");
        let big = vec![0u8; MAX_STATE_FILE as usize + 1];
        assert!(matches!(
            atomic_write_private(&file, &big),
            Err(KdeError::Store(_))
        ));
    }

    #[test]
    fn lock_excludes_second_holder_until_dropped() {
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join("state").join("x.lock");
        let first = lock_private_file(&lock, Duration::from_secs(1)).unwrap();
        let started = Instant::now();
        assert!(matches!(
            lock_private_file(&lock, Duration::from_millis(150)),
            Err(KdeError::Store(_))
        ));
        assert!(started.elapsed() >= Duration::from_millis(100));
        drop(first);
        lock_private_file(&lock, Duration::from_secs(1)).unwrap();
    }
}
