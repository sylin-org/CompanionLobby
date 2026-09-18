//! The tray spoke for the `operator` verb. Windows-only in v1, matching the already
//! windows-native keyring dependency; elsewhere `spawn` is a documented no-op.
//!
//! Threading: the tray (icon + menu) is created on the `tangent-tray` thread, which then
//! becomes the Win32 message pump — tray-icon requires its creating thread to dispatch
//! its own message queue on Windows. Menu events are delivered by tray-icon through a
//! thread-safe channel, consumed by the `tangent-tray-menu` thread. State reads cross the
//! hub under its lock; nothing here mutates anything.

use std::sync::Arc;

use crate::application::hub::ConnectorHub;
use companion_core::domain::events::DomainEvent;

/// Spawns the tray on its own named thread. Failure to build the tray is reported on
/// stderr and leaves the companion manager fully functional. `quit` runs on Quit and is
/// expected to release the data-directory lock and exit the process.
#[cfg(target_os = "windows")]
pub fn spawn(hub: Arc<ConnectorHub>, url: String, quit: Box<dyn FnOnce() + Send>) {
    let spawned = std::thread::Builder::new()
        .name("tangent-tray".into())
        .spawn(move || {
            if let Err(error) = run(hub, url, quit) {
                eprintln!("tangent-connector: tray unavailable ({error}); the companion manager remains available");
            }
        });
    if let Err(error) = spawned {
        eprintln!("tangent-connector: tray thread could not start ({error})");
    }
}

/// Documented no-op on non-Windows platforms: the tray is Windows-only in v1.
#[cfg(not(target_os = "windows"))]
pub fn spawn(_hub: Arc<ConnectorHub>, _url: String, _quit: Box<dyn FnOnce() + Send>) {}

#[cfg(target_os = "windows")]
mod pump;

#[cfg(target_os = "windows")]
fn run(hub: Arc<ConnectorHub>, url: String, quit: Box<dyn FnOnce() + Send>) -> Result<(), String> {
    use tray_icon::menu::{Menu, MenuEvent, MenuItem};
    use tray_icon::{TrayIcon, TrayIconBuilder, TrayIconEvent};

    fn build_menu(hub: &ConnectorHub) -> Menu {
        let (companions, servers) = {
            let store = hub.store().lock().expect("state lock");
            let companions = store.companions().len();
            let servers = store.enrollments().len();
            (companions, servers)
        };
        let menu = Menu::new();
        let status = MenuItem::with_id("status", format!("{companions} identitie(s) · {servers} server(s)"), false, None);
        let open = MenuItem::with_id("open", "Open companion manager", true, None);
        let quit = MenuItem::with_id("quit", "Quit", true, None);
        let _ = menu.append_items(&[&status, &open, &quit]);
        menu
    }

    // Simple geometric mark: deep indigo tile with a pale diagonal seam (a tangent).
    fn icon() -> tray_icon::Icon {
        const SIZE: u32 = 32;
        let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
        for y in 0..SIZE {
            for x in 0..SIZE {
                let seam = (x + y >= 14) && (x + y <= 18);
                let border = x == 0 || y == 0 || x == SIZE - 1 || y == SIZE - 1;
                let pixel = if seam {
                    [242, 244, 250, 255]
                } else if border {
                    [28, 25, 52, 255]
                } else {
                    [64, 56, 128, 255]
                };
                rgba.extend_from_slice(&pixel);
            }
        }
        tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("statically sized icon")
    }

    let tray: TrayIcon = TrayIconBuilder::new()
        .with_menu(Box::new(build_menu(&hub)))
        .with_tooltip("Tangent connector")
        .with_icon(icon())
        .build()
        .map_err(|error| error.to_string())?;

    let menu_hub = hub.clone();
    let mut quit = Some(quit);
    std::thread::Builder::new()
        .name("tangent-tray-menu".into())
        .spawn(move || loop {
            match MenuEvent::receiver().recv() {
                Ok(event) => match event.id.0.as_str() {
                    "open" => menu_hub.open_page(&url),
                    "quit" => {
                        menu_hub.events().publish(DomainEvent::Shutdown { reason: "tray quit".into() });
                        // The hook exits the process; take() satisfies FnOnce in a loop.
                        if let Some(quit) = quit.take() {
                            quit();
                        }
                    }
                    _ => {}
                },
                Err(_) => return,
            }
        })
        .map_err(|error| error.to_string())?;

    // The pump thread duty: dispatch tray-icon's window messages and occasionally
    // refresh the status line. Click events reset the refresh countdown.
    let mut ticks: u32 = 0;
    pump::run_forever(move || {
        while TrayIconEvent::receiver().try_recv().is_ok() {
            ticks = 0;
        }
        ticks = ticks.saturating_add(1);
        if ticks >= 60 {
            ticks = 0;
            tray.set_menu(Some(Box::new(build_menu(&hub))));
        }
    });
}
