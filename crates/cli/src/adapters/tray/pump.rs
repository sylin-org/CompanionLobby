//! The single audited unsafe module in this crate: the Win32 message pump that tray-icon
//! requires on its creating thread. It contains nothing but the pump — no tray, menu or
//! event logic lives here.
//!
//! tray-icon 0.25 documents that "an event loop must be running on the thread" that
//! created the tray icon on Windows: the crate's hidden window procedure (which delivers
//! tray and menu events into its thread-safe channels) only runs when the creating
//! thread dispatches its own message queue. There is no pump-free mode, so this one
//! module carries the crate's only `unsafe` — under `deny`, allowed here alone.
//!
//! Soundness: `PeekMessageW(&mut message, null, 0, 0, PM_REMOVE)` writes into an
//! initialized `MSG` only when it returns TRUE, and `MSG` is a plain Win32 struct of
//! copyable fields with no validity invariant beyond initialization. Both functions are
//! thread-safe Win32 entry points taking exactly the pointers shown; passing a valid
//! `*mut MSG` (resp. `*const MSG`) with a null window filter is their documented
//! contract. No handle is created or released here.

#![allow(unsafe_code)]

use std::thread::sleep;
use std::time::Duration;

use windows_sys::Win32::UI::WindowsAndMessaging::{DispatchMessageW, PeekMessageW, MSG, PM_REMOVE};

const IDLE_SLEEP: Duration = Duration::from_millis(250);

/// Pumps this thread's message queue until the process ends: drain queued messages,
/// dispatch each to its window procedure, run `on_tick`, sleep, repeat. The idle sleep
/// keeps the cost negligible while still waking for periodic work (status refresh).
pub fn run_forever(mut on_tick: impl FnMut()) -> ! {
    let mut message: MSG = MSG { hwnd: core::ptr::null_mut(), message: 0, wParam: 0, lParam: 0, time: 0, pt: Default::default() };
    loop {
        while unsafe { PeekMessageW(&mut message, core::ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe { DispatchMessageW(&message) };
        }
        on_tick();
        sleep(IDLE_SLEEP);
    }
}
