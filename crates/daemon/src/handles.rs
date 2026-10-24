//! Standard handles a started daemon must not inherit (Windows only).
//!
//! An MCP host gives the attach shim inheritable pipe handles as its standard
//! streams. Windows hands every inheritable handle to every child process,
//! whatever the child's own standard streams are, so a daemon started by the
//! shim would keep the host's pipes open: the host would never see the
//! shim's stdout close after the shim exits. Clearing the inherit flag on the
//! shim's own standard handles before the daemon starts closes that path.
//!
//! This is the daemon crate's one use of `unsafe`: the standard library has no
//! safe way to change a handle's inherit flag.
#![allow(unsafe_code)]

use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Foundation::{HANDLE_FLAG_INHERIT, SetHandleInformation};

/// Marks this process's stdin, stdout and stderr as not inheritable.
pub fn keep_std_handles_from_children() {
    let handles = [
        std::io::stdin().as_raw_handle(),
        std::io::stdout().as_raw_handle(),
        std::io::stderr().as_raw_handle(),
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
