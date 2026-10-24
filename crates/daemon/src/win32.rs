//! Windows calls the standard library has no safe form for.
//!
//! This is the daemon crate's one use of `unsafe`, and it holds four things:
//!
//! * **Standard handles a started daemon must not inherit.** An MCP host
//!   gives the attach shim inheritable pipe handles as its standard streams.
//!   Windows hands every inheritable handle to every child process, whatever
//!   the child's own standard streams are, so a daemon started by the shim
//!   would keep the host's pipes open: the host would never see the shim's
//!   stdout close after the shim exits. Clearing the inherit flag on the
//!   shim's own standard handles before the daemon starts closes that path.
//! * **The owner-only data directory.** The directory gets a protected DACL
//!   with one entry, full control for the current user, inherited by
//!   everything inside it; and the daemon checks on every start that it still
//!   has exactly that and that the current user owns it, since an owner can
//!   always rewrite a DACL. Files inside are read the same way: whether one
//!   is private to the current user, and whether its access comes only from
//!   its directory.
//! * **Sealing with DPAPI.** Secrets at rest are sealed in the current user's
//!   scope, so a copy of the file is useless to another account or machine.
//! * **An exclusive listening port.** The listener is bound with
//!   `SO_EXCLUSIVEADDRUSE`, so no other socket can share the port while the
//!   daemon runs.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::io::{AsRawHandle, AsRawSocket};
use std::path::Path;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, HANDLE_FLAG_INHERIT, LocalFree, SetHandleInformation,
};
use windows_sys::Win32::Networking::WinSock::{SO_EXCLUSIVEADDRUSE, SOL_SOCKET, setsockopt};
use windows_sys::Win32::Security::Authorization::{
    GetNamedSecurityInfoW, SE_FILE_OBJECT, SetNamedSecurityInfoW,
};
use windows_sys::Win32::Security::Cryptography::{
    CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptProtectData, CryptUnprotectData,
};
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, INHERITED_ACE, InitializeAcl,
    OBJECT_INHERIT_ACE, OWNER_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
    PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED, TOKEN_INFORMATION_CLASS, TOKEN_OWNER,
    TOKEN_QUERY, TOKEN_USER, TokenOwner, TokenUser,
};
use windows_sys::Win32::Storage::FileSystem::FILE_ALL_ACCESS;
use windows_sys::Win32::System::SystemServices::ACCESS_ALLOWED_ACE_TYPE;
use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};

/// Marks this process's stdin, stdout and stderr as not inheritable.
pub fn keep_std_handles_from_children() {
    let handles = [
        io::stdin().as_raw_handle(),
        io::stdout().as_raw_handle(),
        io::stderr().as_raw_handle(),
    ];
    for handle in handles {
        if handle.is_null() {
            continue;
        }
        // SAFETY: `handle` is one of this process's standard handles, owned by
        // the standard library for the life of the process. SetHandleInformation
        // only changes its inherit flag; it neither closes nor transfers it. A
        // failure (for example on a console pseudo-handle) leaves the flag as it
        // was, which is harmless.
        unsafe {
            SetHandleInformation(handle, HANDLE_FLAG_INHERIT, 0);
        }
    }
}

/// Sets `SO_EXCLUSIVEADDRUSE` on a socket that is not bound yet.
pub fn use_address_exclusively(socket: &impl AsRawSocket) -> io::Result<()> {
    let enabled: i32 = 1;
    // SAFETY: the socket is open for as long as `socket` is borrowed, and the
    // option value is a live i32 whose size is passed alongside it.
    let status = unsafe {
        setsockopt(
            socket.as_raw_socket() as usize,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const enabled).cast(),
            size_of::<i32>() as i32,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Gives `path` a protected DACL with one entry: full control for the current
/// user, inherited by every file and folder inside it.
pub fn restrict_to_owner(path: &Path) -> io::Result<()> {
    let user = CurrentUser::query()?;
    let sid = user.sid();
    // SAFETY: `sid` points into `user`, a valid SID for as long as `user` lives.
    let sid_length = unsafe { GetLengthSid(sid) } as usize;
    // An ACL header and one access-allowed entry whose trailing SID replaces
    // its `SidStart` field.
    let size = size_of::<ACL>() + size_of::<ACCESS_ALLOWED_ACE>() - size_of::<u32>() + sid_length;
    let mut buffer = vec![0u64; size.div_ceil(size_of::<u64>())];
    let acl = buffer.as_mut_ptr().cast::<ACL>();
    // SAFETY: `acl` points to `size` writable, 8-byte aligned bytes that
    // outlive every call below, and `sid` stays valid while `user` lives.
    unsafe {
        if InitializeAcl(acl, size as u32, ACL_REVISION) == 0 {
            return Err(io::Error::last_os_error());
        }
        if AddAccessAllowedAceEx(
            acl,
            ACL_REVISION,
            OBJECT_INHERIT_ACE | CONTAINER_INHERIT_ACE,
            FILE_ALL_ACCESS,
            sid,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
    }
    let wide = wide(path);
    // SAFETY: `wide` is a NUL-terminated UTF-16 path and `acl` a valid ACL;
    // the owner, group and SACL are not touched.
    let status = unsafe {
        SetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            acl,
            null(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(status as i32))
    }
}

/// Whether `path` is owned by the current user (or by the default owner its
/// token gives new objects, which differs when the process runs elevated) and
/// has a protected DACL whose every entry allows the current user and no one
/// else.
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    let user = CurrentUser::query()?;
    let security = Security::read(path)?;
    Ok(security.owned_by_current_user()?
        && security.is_protected()?
        && security.allows_only(user.sid())?)
}

/// Whether the file at `path` is owned by the current user and every entry of
/// its DACL, inherited or not, allows the current user and no one else.
pub fn is_private_file(path: &Path) -> io::Result<bool> {
    let user = CurrentUser::query()?;
    let security = Security::read(path)?;
    Ok(security.owned_by_current_user()? && security.allows_only(user.sid())?)
}

/// Whether `path` is owned by the current user and every entry of its DACL
/// is inherited from its directory, so narrowing the directory narrows it too.
pub fn has_only_inherited_entries(path: &Path) -> io::Result<bool> {
    let security = Security::read(path)?;
    Ok(security.owned_by_current_user()? && security.only_inherited()?)
}

/// What one entry of a DACL says.
struct Entry {
    allows: bool,
    inherited: bool,
    names_sid: bool,
}

/// The owner and DACL of one file or directory, read together.
struct Security {
    descriptor: PSECURITY_DESCRIPTOR,
    owner: PSID,
    dacl: *mut ACL,
}

impl Security {
    fn read(path: &Path) -> io::Result<Self> {
        let wide = wide(path);
        let mut owner: PSID = null_mut();
        let mut dacl: *mut ACL = null_mut();
        let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
        // SAFETY: every out pointer is a live local; on success `descriptor` is
        // a LocalAlloc'd block that `owner` and `dacl` point into, freed on drop.
        let status = unsafe {
            GetNamedSecurityInfoW(
                wide.as_ptr(),
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            )
        };
        if status != 0 {
            return Err(io::Error::from_raw_os_error(status as i32));
        }
        Ok(Self {
            descriptor,
            owner,
            dacl,
        })
    }

    /// Whether the current user, or the default owner of its token, owns it.
    fn owned_by_current_user(&self) -> io::Result<bool> {
        let user = CurrentUser::query()?;
        let default_owner = DefaultOwner::query()?;
        // SAFETY: `owner` is null or a valid SID inside `descriptor`, alive
        // while `self` is; both token SIDs live as long as their owners.
        Ok(!self.owner.is_null()
            && unsafe {
                EqualSid(self.owner, user.sid()) != 0
                    || EqualSid(self.owner, default_owner.sid()) != 0
            })
    }

    fn is_protected(&self) -> io::Result<bool> {
        let mut control = 0u16;
        let mut revision = 0u32;
        // SAFETY: `descriptor` was returned by the system and lives while
        // `self` does; the out pointers are locals.
        if unsafe { GetSecurityDescriptorControl(self.descriptor, &mut control, &mut revision) }
            == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(control & SE_DACL_PROTECTED != 0)
    }

    /// The DACL's entries, or `None` for a null DACL, which grants everyone
    /// everything. Each entry's SID is compared with `sid`.
    fn entries(&self, sid: PSID) -> io::Result<Option<Vec<Entry>>> {
        if self.dacl.is_null() {
            return Ok(None);
        }
        // SAFETY: `dacl` is a valid ACL inside `descriptor`, alive while `self` is.
        let count = unsafe { (*self.dacl).AceCount };
        let mut entries = Vec::with_capacity(usize::from(count));
        for index in 0..u32::from(count) {
            let mut entry: *mut c_void = null_mut();
            // SAFETY: `index` is below the ACL's entry count.
            if unsafe { GetAce(self.dacl, index, &mut entry) } == 0 {
                return Err(io::Error::last_os_error());
            }
            // SAFETY: every entry starts with an ACE_HEADER.
            let header = unsafe { entry.cast::<ACE_HEADER>().read_unaligned() };
            let allows = u32::from(header.AceType) == ACCESS_ALLOWED_ACE_TYPE;
            let names_sid = allows
                && !sid.is_null()
                // SAFETY: an access-allowed entry is an ACCESS_ALLOWED_ACE
                // whose SID starts at `SidStart`; both SIDs are valid here.
                && unsafe {
                    let entry_sid = &raw mut (*entry.cast::<ACCESS_ALLOWED_ACE>()).SidStart;
                    EqualSid(entry_sid.cast(), sid) != 0
                };
            entries.push(Entry {
                allows,
                inherited: u32::from(header.AceFlags) & INHERITED_ACE != 0,
                names_sid,
            });
        }
        Ok(Some(entries))
    }

    /// Whether the DACL has entries and each allows `sid` and no one else.
    fn allows_only(&self, sid: PSID) -> io::Result<bool> {
        Ok(self.entries(sid)?.is_some_and(|entries| {
            !entries.is_empty() && entries.iter().all(|entry| entry.allows && entry.names_sid)
        }))
    }

    /// Whether the DACL exists and every entry in it was inherited.
    fn only_inherited(&self) -> io::Result<bool> {
        Ok(self
            .entries(null_mut())?
            .is_some_and(|entries| entries.iter().all(|entry| entry.inherited)))
    }
}

impl Drop for Security {
    fn drop(&mut self) {
        // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW for us to
        // free, and is freed exactly once.
        unsafe {
            LocalFree(self.descriptor);
        }
    }
}

/// Seals `bytes` with DPAPI in the current user's scope: only this user, on
/// this machine, can unseal them. `purpose` is mixed in, so a blob sealed for
/// one purpose does not unseal for another.
pub fn protect(bytes: &[u8], purpose: &[u8]) -> io::Result<Vec<u8>> {
    crypt(bytes, purpose, true)
}

/// Unseals what [`protect`] sealed for `purpose`. Anything else, including a
/// blob another user or machine sealed, is refused as invalid data.
pub fn unprotect(blob: &[u8], purpose: &[u8]) -> io::Result<Vec<u8>> {
    crypt(blob, purpose, false)
}

fn crypt(input: &[u8], purpose: &[u8], seal: bool) -> io::Result<Vec<u8>> {
    let length = |bytes: &[u8]| {
        u32::try_from(bytes.len())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "too large to seal"))
    };
    let input_blob = CRYPT_INTEGER_BLOB {
        cbData: length(input)?,
        pbData: input.as_ptr().cast_mut(),
    };
    let entropy = CRYPT_INTEGER_BLOB {
        cbData: length(purpose)?,
        pbData: purpose.as_ptr().cast_mut(),
    };
    let mut output = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: null_mut(),
    };
    // SAFETY: both input blobs point at live slices of the stated lengths that
    // DPAPI only reads; `output` receives a LocalAlloc'd buffer freed below.
    let succeeded = unsafe {
        if seal {
            CryptProtectData(
                &input_blob,
                null(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        } else {
            CryptUnprotectData(
                &input_blob,
                null_mut(),
                &entropy,
                null(),
                null(),
                CRYPTPROTECT_UI_FORBIDDEN,
                &mut output,
            )
        }
    };
    if succeeded == 0 {
        let error = io::Error::last_os_error();
        return Err(if seal {
            error
        } else {
            io::Error::new(io::ErrorKind::InvalidData, error)
        });
    }
    // SAFETY: on success `output` describes `cbData` readable bytes at `pbData`.
    let bytes =
        unsafe { std::slice::from_raw_parts(output.pbData, output.cbData as usize) }.to_vec();
    // SAFETY: DPAPI allocated `pbData` with LocalAlloc for us to free, once.
    unsafe {
        LocalFree(output.pbData.cast());
    }
    Ok(bytes)
}

/// One piece of the current process's token, read into an aligned buffer.
struct TokenInformation {
    /// The structure the class names, followed by the SID it points to, 8-byte aligned.
    buffer: Vec<u64>,
}

impl TokenInformation {
    fn query(class: TOKEN_INFORMATION_CLASS) -> io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `token` receives a real handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0u32;
        // SAFETY: a size query with no buffer; `needed` is a local.
        unsafe {
            GetTokenInformation(token, class, null_mut(), 0, &mut needed);
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        // SAFETY: `buffer` holds at least `needed` writable bytes.
        let filled = unsafe {
            GetTokenInformation(
                token,
                class,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        let error = io::Error::last_os_error();
        // SAFETY: `token` was opened above and is closed exactly once.
        unsafe {
            CloseHandle(token);
        }
        if filled == 0 || buffer.is_empty() {
            return Err(error);
        }
        Ok(Self { buffer })
    }
}

/// The current process's user, read from its token.
struct CurrentUser(TokenInformation);

impl CurrentUser {
    fn query() -> io::Result<Self> {
        TokenInformation::query(TokenUser).map(Self)
    }

    fn sid(&self) -> PSID {
        // SAFETY: the buffer was filled by GetTokenInformation(TokenUser), so it
        // starts with an aligned TOKEN_USER whose SID points into the buffer.
        unsafe { (*self.0.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
    }
}

/// The owner the current process's token gives the objects it creates.
struct DefaultOwner(TokenInformation);

impl DefaultOwner {
    fn query() -> io::Result<Self> {
        TokenInformation::query(TokenOwner).map(Self)
    }

    fn sid(&self) -> PSID {
        // SAFETY: the buffer was filled by GetTokenInformation(TokenOwner), so it
        // starts with an aligned TOKEN_OWNER whose SID points into the buffer.
        unsafe { (*self.0.buffer.as_ptr().cast::<TOKEN_OWNER>()).Owner }
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_restricted_directory_and_what_is_created_in_it_belong_to_the_owner_alone() {
        let dir = std::env::temp_dir().join(format!(
            "lobby-acl-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        assert!(
            !is_owner_only(&dir).expect("read the DACL"),
            "a fresh folder inherits its parent's entries"
        );
        restrict_to_owner(&dir).expect("restrict");
        assert!(is_owner_only(&dir).expect("read the DACL"));
        let inner = dir.join("inner");
        std::fs::create_dir(&inner).expect("inner");
        std::fs::write(inner.join("file"), b"x").expect("file");
        assert!(std::fs::read(inner.join("file")).is_ok());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn sealed_bytes_open_only_for_their_purpose() {
        let sealed = protect(b"0123", b"one purpose").expect("seal");
        assert!(!sealed.windows(4).any(|window| window == b"0123"));
        assert_eq!(unprotect(&sealed, b"one purpose").expect("unseal"), b"0123");
        let refused = unprotect(&sealed, b"another purpose").expect_err("refused");
        assert_eq!(refused.kind(), io::ErrorKind::InvalidData);
        let plain = unprotect(b"not sealed at all", b"one purpose").expect_err("refused");
        assert_eq!(plain.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn a_file_is_private_when_its_access_comes_from_an_owner_only_directory() {
        let dir = std::env::temp_dir().join(format!(
            "lobby-file-acl-{}-{}",
            std::process::id(),
            uuid::Uuid::now_v7().simple()
        ));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let file = dir.join("secret");
        std::fs::write(&file, b"x").expect("file");
        assert!(
            !is_private_file(&file).expect("read"),
            "the temp folder is shared"
        );
        assert!(has_only_inherited_entries(&file).expect("read"));
        restrict_to_owner(&dir).expect("restrict");
        assert!(
            is_private_file(&file).expect("read"),
            "the narrowing reached the file"
        );
        assert!(has_only_inherited_entries(&file).expect("read"));
        let granted = std::process::Command::new("icacls")
            .arg(&file)
            .args(["/grant", "*S-1-1-0:R"])
            .stdout(std::process::Stdio::null())
            .status()
            .expect("run icacls");
        assert!(granted.success());
        assert!(!is_private_file(&file).expect("read"));
        assert!(
            !has_only_inherited_entries(&file).expect("read"),
            "an explicit entry"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_listener_bound_for_exclusive_use_refuses_a_second_socket() {
        let socket = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = socket.local_addr().expect("address").port();
        drop(socket);
        let first = tokio::net::TcpSocket::new_v4().expect("socket");
        use_address_exclusively(&first).expect("exclusive");
        first
            .bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
            .expect("first bind");
        let second = tokio::net::TcpSocket::new_v4().expect("socket");
        second.set_reuseaddr(true).expect("reuse");
        assert!(
            second
                .bind(std::net::SocketAddr::from(([127, 0, 0, 1], port)))
                .is_err()
        );
    }
}
