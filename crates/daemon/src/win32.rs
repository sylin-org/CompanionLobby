//! Windows calls the standard library has no safe form for.
//!
//! This is the daemon crate's one use of `unsafe`, and it holds three things:
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
//!   has exactly that.
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
use windows_sys::Win32::Security::{
    ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, ACL_REVISION, AddAccessAllowedAceEx,
    CONTAINER_INHERIT_ACE, DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetLengthSid,
    GetSecurityDescriptorControl, GetTokenInformation, InitializeAcl, OBJECT_INHERIT_ACE,
    PROTECTED_DACL_SECURITY_INFORMATION, PSECURITY_DESCRIPTOR, PSID, SE_DACL_PROTECTED,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
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

/// Whether `path` has a protected DACL whose every entry allows the current
/// user and no one else.
pub fn is_owner_only(path: &Path) -> io::Result<bool> {
    let user = CurrentUser::query()?;
    let wide = wide(path);
    let mut dacl: *mut ACL = null_mut();
    let mut descriptor: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: every out pointer is a live local; on success `descriptor` is a
    // LocalAlloc'd block that `dacl` points into, freed below.
    let status = unsafe {
        GetNamedSecurityInfoW(
            wide.as_ptr(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if status != 0 {
        return Err(io::Error::from_raw_os_error(status as i32));
    }
    // SAFETY: `descriptor` and `dacl` were just returned by the system and stay
    // valid until the LocalFree below; `user` outlives the comparison.
    let verdict = unsafe { dacl_is_owner_only(descriptor, dacl, user.sid()) };
    // SAFETY: `descriptor` was allocated by GetNamedSecurityInfoW for us to free.
    unsafe {
        LocalFree(descriptor);
    }
    verdict
}

/// # Safety
///
/// `descriptor` must be a valid security descriptor, `dacl` null or a valid
/// ACL inside it, and `sid` a valid SID.
unsafe fn dacl_is_owner_only(
    descriptor: PSECURITY_DESCRIPTOR,
    dacl: *const ACL,
    sid: PSID,
) -> io::Result<bool> {
    let mut control = 0u16;
    let mut revision = 0u32;
    // SAFETY: this function's contract guarantees `descriptor`; the out pointers are locals.
    if unsafe { GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // A null DACL grants everyone everything.
    if control & SE_DACL_PROTECTED == 0 || dacl.is_null() {
        return Ok(false);
    }
    // SAFETY: this function's contract guarantees `dacl` is a valid ACL.
    let count = unsafe { (*dacl).AceCount };
    if count == 0 {
        return Ok(false);
    }
    for index in 0..u32::from(count) {
        let mut entry: *mut c_void = null_mut();
        // SAFETY: `index` is below the ACL's entry count.
        if unsafe { GetAce(dacl, index, &mut entry) } == 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: every entry starts with an ACE_HEADER.
        let header = unsafe { entry.cast::<ACE_HEADER>().read_unaligned() };
        if u32::from(header.AceType) != ACCESS_ALLOWED_ACE_TYPE {
            return Ok(false);
        }
        // SAFETY: an access-allowed entry is an ACCESS_ALLOWED_ACE whose SID
        // starts at `SidStart`.
        let entry_sid = unsafe { (&raw mut (*entry.cast::<ACCESS_ALLOWED_ACE>()).SidStart) };
        // SAFETY: both SIDs are valid for the duration of the call.
        if unsafe { EqualSid(entry_sid.cast(), sid) } == 0 {
            return Ok(false);
        }
    }
    Ok(true)
}

/// The current process's user, read from its token.
struct CurrentUser {
    /// A TOKEN_USER followed by the SID it points to, 8-byte aligned.
    buffer: Vec<u64>,
}

impl CurrentUser {
    fn query() -> io::Result<Self> {
        let mut token: HANDLE = null_mut();
        // SAFETY: GetCurrentProcess returns a pseudo-handle that needs no
        // closing; `token` receives a real handle closed below.
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let mut needed = 0u32;
        // SAFETY: a size query with no buffer; `needed` is a local.
        unsafe {
            GetTokenInformation(token, TokenUser, null_mut(), 0, &mut needed);
        }
        let mut buffer = vec![0u64; (needed as usize).div_ceil(size_of::<u64>())];
        // SAFETY: `buffer` holds at least `needed` writable bytes.
        let filled = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
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

    fn sid(&self) -> PSID {
        // SAFETY: the buffer was filled by GetTokenInformation(TokenUser), so it
        // starts with an aligned TOKEN_USER whose SID points into the buffer.
        unsafe { (*self.buffer.as_ptr().cast::<TOKEN_USER>()).User.Sid }
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
