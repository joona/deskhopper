// src/main.rs

// For release builds, hide the console window
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use anyhow::{Context, Result}; 
use global_hotkey::{
    hotkey::{Code, HotKey, Modifiers},
    GlobalHotKeyManager, GlobalHotKeyEvent, HotKeyState,
};
use log::{debug, error, info, warn};
use std::{
    collections::HashMap, 
    sync::{Arc, Mutex}, // Added Arc, Mutex for shared state
    thread, 
    ffi::OsString, 
    os::windows::ffi::OsStringExt
};
use tao::{
    event::{Event, WindowEvent},
    event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy, EventLoop}, 
};
// Corrected tray_icon imports: Using MenuItem from tray_icon::menu
use tray_icon::{
    menu::{accelerator::Accelerator, Menu, MenuEvent, MenuItem, PredefinedMenuItem}, 
    TrayIconBuilder, TrayIconEvent,
};
// Removed direct muda import

use winvd::{create_desktop, get_desktop_count, switch_desktop, move_window_to_desktop, get_desktop_by_window}; 

use windows::Win32::{
    Foundation::{HWND, LPARAM, BOOL, TRUE, FALSE},
    UI::WindowsAndMessaging::{
        MessageBoxW, MB_ICONERROR, MB_ICONINFORMATION, MESSAGEBOX_STYLE, GetForegroundWindow,
        IsWindow, IsWindowVisible, GetWindowLongW, GWL_STYLE, BringWindowToTop, SetForegroundWindow, WS_CHILD,
        EnumWindows, GetWindowTextW,
    },
};
use windows::core::PCWSTR; 

#[derive(Debug, Clone, Copy)]
enum CustomEvent {
    HotkeyTriggered(u32),
}

enum HotkeyAction {
    Switch(usize),      // Target desktop index for switching
    MoveWindow(usize),  // Target desktop index for moving window
}

// Type alias for our shared map of last active windows
type LastActiveWindowMap = Arc<Mutex<HashMap<u32, HWND>>>;

const TRAY_ICON_TOOLTIP: &str = "DeskHopper";
const MENU_ID_ABOUT_STR: &str = "about"; 
const MENU_ID_EXIT_STR: &str = "exit";   
const APP_NAME: &str = "DeskHopper";

const ICON_BYTES: &[u8] = include_bytes!("../icon.ico");

struct EnumCallbackData {
    target_desktop_id: u32,
    found_hwnd: Option<HWND>,
}

fn load_tray_icon() -> Result<tray_icon::Icon> { 
    let image = image::load_from_memory_with_format(ICON_BYTES, image::ImageFormat::Ico)
        .context("Failed to load icon from memory")?
        .to_rgba8();
    let (width, height) = image.dimensions();
    let icon_data = image.into_raw();
    tray_icon::Icon::from_rgba(icon_data, width, height)
        .context("Failed to create tray icon from RGBA data")
}

fn main() -> Result<()> {
    env_logger::Builder::from_default_env()
        .filter_level(log::LevelFilter::Info)
        .init();

    info!("{} starting...", APP_NAME);

    // Explicitly handle Result from EventLoopBuilder::build() then apply context
    let event_loop: EventLoop<CustomEvent> = EventLoopBuilder::<CustomEvent>::with_user_event()
        .build();
    let proxy: EventLoopProxy<CustomEvent> = event_loop.create_proxy();

    let mut hotkey_manager = GlobalHotKeyManager::new().context("Failed to initialize GlobalHotKeyManager")?;

    // Initialize the map for storing the last active window on each desktop
    let last_active_windows_map: LastActiveWindowMap = Arc::new(Mutex::new(HashMap::new()));
    
    let mut hotkey_actions: HashMap<u32, HotkeyAction> = HashMap::new();
    let mut registered_hotkey_structs: Vec<HotKey> = Vec::new();

    let _tray_icon = match setup_tray_icon() {
        Ok(icon) => icon,
        Err(e) => {
            let err_msg = format!("Failed to create system tray icon: {:?}\nApplication will exit.", e);
            error!("{}", err_msg);
            show_message_box("Error", &err_msg, MB_ICONERROR);
            return Err(e); 
        }
    };
    info!("System tray icon created.");

    if let Err(e) = register_hotkeys(&mut hotkey_manager, &mut hotkey_actions, &mut registered_hotkey_structs) {
        let err_msg = format!("Failed to register one or more hotkeys: {:?}\nSome hotkeys may not work.", e);
        error!("{}", err_msg);
        show_message_box("Hotkey Registration Error", &err_msg, MB_ICONERROR);
    }

    let hotkey_event_proxy = proxy.clone();
    thread::spawn(move || {
        let receiver = GlobalHotKeyEvent::receiver();
        info!("Hotkey listener thread started.");
        loop {
            match receiver.recv() {
                Ok(event) => {
                    debug!("GlobalHotKeyEvent received: {:?}", event);
                    if event.state == HotKeyState::Pressed {
                        if hotkey_event_proxy.send_event(CustomEvent::HotkeyTriggered(event.id)).is_err() {
                            error!("Failed to send hotkey event to main loop. Main loop likely exited.");
                            break; 
                        }
                    }
                }
                Err(e) => {
                    error!("Error receiving from global_hotkey channel: {:?}", e);
                    break; 
                }
            }
        }
        info!("Hotkey listener thread finished.");
    });

    info!("Event loop starting. Application is running in the background.");

    // Clone Arc for the event loop closure
    let last_active_windows_map_for_loop = Arc::clone(&last_active_windows_map);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;

        // Poll for tray events
        match TrayIconEvent::receiver().try_recv() {
            Ok(tray_event) => {
                info!("Tray Event Received: id='{}', rect={:?}", tray_event.id.0, tray_event.icon_rect);
                match tray_event.id.0.as_str() { 
                    MENU_ID_ABOUT_STR => {
                        info!("'About DeskHopper' menu item clicked.");
                        show_about_dialog();
                    }
                    MENU_ID_EXIT_STR => {
                        info!("'Exit' menu item clicked. Shutting down.");
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {
                        debug!("Unhandled tray event ID: '{}'", tray_event.id.0);
                    }
                }
            }
            Err(_e) => {
                //error!("Error receiving tray event: {:?}", e);
            }
        }

        match MenuEvent::receiver().try_recv() {
            Ok(event) => {
                info!("menu event: {:?}", event);

                match event.id.0.as_str() {
                    MENU_ID_ABOUT_STR => {
                        info!("'About DeskHopper' menu item clicked.");
                        show_about_dialog();
                    }
                    MENU_ID_EXIT_STR => {
                        info!("'Exit' menu item clicked. Shutting down.");
                        *control_flow = ControlFlow::Exit;
                    }
                    _ => {
                        debug!("Unhandled tray event ID: '{}'", event.id.0);
                    }
                }
            }
            Err(_e) => {
                //error!("Error receiving menu event: {:?}", e);
            }
        }

        match event {
            Event::NewEvents(_) => (),
            Event::WindowEvent { event, .. } => {
                if event == WindowEvent::CloseRequested {
                    *control_flow = ControlFlow::Exit;
                }
            }
            Event::MainEventsCleared => (),
            Event::RedrawRequested(_) => (),
            Event::LoopDestroyed => {
                info!("Event loop destroyed.");
            }
            Event::UserEvent(custom_event) => {
                match custom_event {
                    // Match on the single HotkeyTriggered variant
                    CustomEvent::HotkeyTriggered(id) => {
                        if let Some(action) = hotkey_actions.get(&id) {
                            match action {
                                HotkeyAction::Switch(desktop_idx) => {
                                    info!("Switch Hotkey ID {} pressed, switching to desktop index {}", id, desktop_idx);
                                    handle_switch_to_desktop(*desktop_idx, &last_active_windows_map_for_loop);
                                }
                                HotkeyAction::MoveWindow(desktop_idx) => {
                                    info!("Move Window Hotkey ID {} pressed, moving window to desktop index {}", id, desktop_idx);
                                    handle_move_window_to_desktop(*desktop_idx);
                                }
                            }
                        } else {
                            warn!("Received unknown hotkey ID via UserEvent: {}", id);
                        }
                    }
                }
            }
            _ => (),
        }
    }); 
    
    #[allow(unreachable_code)]
    Ok(())
} 

fn setup_tray_icon() -> Result<tray_icon::TrayIcon> {
    let icon_data = load_tray_icon().context("Failed to load icon for tray")?;
    // `menu` does not need to be mutable as `append` takes `&self` and returns `Result<&Self>`.
    let menu = Menu::new(); 

    // MenuItem::new from tray_icon::menu (which is muda::MenuItem)
    // takes (text: S, enabled: bool, accelerator: Option<Accelerator>)
    // The string used for `text` is what MenuId will wrap if not specified otherwise.
    // The TrayIconEvent.id.0 will be this string.
    
    let about_item = MenuItem::with_id(MENU_ID_ABOUT_STR, MENU_ID_ABOUT_STR, true, None::<Accelerator>); 
    menu.append(&about_item).context("Failed to append About item")?;

    menu.append(&PredefinedMenuItem::separator()).context("Failed to append separator")?;

    let exit_item = MenuItem::with_id(MENU_ID_EXIT_STR, MENU_ID_EXIT_STR, true, None::<Accelerator>);
    menu.append(&exit_item).context("Failed to append Exit item")?;

    let tray_instance = TrayIconBuilder::new()
        .with_menu(Box::new(menu)) 
        .with_tooltip(TRAY_ICON_TOOLTIP)
        .with_icon(icon_data) 
        .build()
        .context("Failed to build system tray icon")?;
    Ok(tray_instance)
}

fn register_hotkeys(
    manager: &mut GlobalHotKeyManager,
    actions: &mut HashMap<u32, HotkeyAction>, 
    registered_vec: &mut Vec<HotKey>, 
) -> Result<()> {
    info!("Registering hotkeys...");

    for i in 1..=9 {
        let code = number_to_code(i).context(format!("Invalid number for code: {}", i))?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL), code);
        manager.register(hotkey).context(format!("Failed to register RCtrl+{}", i))?;
        actions.insert(hotkey.id(), HotkeyAction::Switch((i - 1) as usize)); 
        registered_vec.push(hotkey); 
        info!("Registered SWITCH RCtrl+{} -> Desktop Index {}", i, i - 1);
    }
    let hotkey_0_switch = HotKey::new(Some(Modifiers::CONTROL), Code::Digit0);
    manager.register(hotkey_0_switch).context("Failed to register RCtrl+0 for SWITCH")?;
    actions.insert(hotkey_0_switch.id(), HotkeyAction::Switch(9)); 
    registered_vec.push(hotkey_0_switch); 
    info!("Registered SWITCH RCtrl+0 -> Desktop Index 9");

    for i in 1..=9 {
        let code = number_to_code(i).context(format!("Invalid number for code: {}", i))?;
        let hotkey = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), code);
        manager.register(hotkey).context(format!("Failed to register RCtrl+Shift+{}", i))?;
        actions.insert(hotkey.id(), HotkeyAction::MoveWindow((i - 1) as usize));
        registered_vec.push(hotkey);
        info!("Registered MOVE RCtrl+Shift+{} -> Desktop Index {}", i, i - 1);
    }
    let hotkey_0_move = HotKey::new(Some(Modifiers::CONTROL | Modifiers::SHIFT), Code::Digit0);
    manager.register(hotkey_0_move).context("Failed to register RCtrl+Shift+0 for MOVE")?;
    actions.insert(hotkey_0_move.id(), HotkeyAction::MoveWindow(9));
    registered_vec.push(hotkey_0_move);
    info!("Registered MOVE RCtrl+Shift+0 -> Desktop Index 9");

    info!("All hotkeys registration attempted.");
    Ok(())
}

fn number_to_code(num: u32) -> Result<Code> {
    match num {
        1 => Ok(Code::Digit1), 2 => Ok(Code::Digit2), 3 => Ok(Code::Digit3),
        4 => Ok(Code::Digit4), 5 => Ok(Code::Digit5), 6 => Ok(Code::Digit6),
        7 => Ok(Code::Digit7), 8 => Ok(Code::Digit8), 9 => Ok(Code::Digit9),
        0 => Ok(Code::Digit0),
        _ => Err(anyhow::anyhow!("Number out of range for hotkey code")),
    }
}

fn handle_switch_to_desktop(target_desktop_idx_0_based: usize, last_active_map: &LastActiveWindowMap) {
    info!("Attempting to SWITCH to desktop index: {}", target_desktop_idx_0_based);

    // 1. Store the current foreground window for the *current* desktop before switching
    if let Ok(current_desktop_before_switch) = winvd::get_current_desktop() {
        let current_desktop_id_before_switch = current_desktop_before_switch.get_index();
        let current_fg_hwnd = unsafe { GetForegroundWindow() };
        if current_fg_hwnd.0 != std::ptr::null_mut() { // Check if HWND is not null
             info!("Remembering HWND {:?} for desktop ID {:?}", current_fg_hwnd, current_desktop_id_before_switch);
            let mut map_guard = last_active_map.lock().unwrap_or_else(|poisoned| {
                warn!("Mutex for last_active_windows_map was poisoned in handle_switch (store). Recovering.");
                poisoned.into_inner()
            });
            map_guard.insert(current_desktop_id_before_switch.unwrap(), current_fg_hwnd);
        }
    } else {
        warn!("Could not get current desktop ID before switch to store last active window.");
    }

    let mut switched_successfully = false;

    match get_desktop_count() {
        Ok(current_count_u32) => {
            let current_count = current_count_u32 as usize; 
            info!("Current virtual desktop count: {}", current_count);
            if target_desktop_idx_0_based < current_count {
                match switch_desktop(target_desktop_idx_0_based as u32) { 
                    Ok(_) => {
                        info!("Switched to desktop index {} successfully.", target_desktop_idx_0_based);
                        switched_successfully = true;
                    },
                    Err(e) => error!("Failed to switch to desktop index {}: {:?}", target_desktop_idx_0_based, e),
                }
            } else {
                info!(
                    "Target desktop index {} is out of range (current count {}). Creating new desktops.",
                    target_desktop_idx_0_based, current_count
                );
                let desktops_to_create = target_desktop_idx_0_based - current_count + 1;
                for i in 0..desktops_to_create {
                    match create_desktop() {
                        Ok(new_desktop_id) => info!(
                            "Created new desktop (iteration {}/{}), ID: {:?}",
                            i + 1, desktops_to_create, new_desktop_id
                        ),
                        Err(e) => {
                            error!("Failed to create new desktop (iteration {}): {:?}", i + 1, e);
                            show_message_box(
                                "Desktop Creation Error",
                                &format!("Failed to create a new virtual desktop: {:?}.\nPlease ensure resources and permissions.", e),
                                MB_ICONERROR);
                            return;
                        }
                    }
                }
                match switch_desktop(target_desktop_idx_0_based as u32) {
                    Ok(_) => {
                        info!("Switched to newly created desktop index {} successfully.", target_desktop_idx_0_based);
                        switched_successfully = true;
                    }
                    Err(e) => error!("Failed to switch to newly created desktop index {}: {:?}", target_desktop_idx_0_based, e),
                }
            }
        }
        Err(e) => {
            error!("Failed to get virtual desktop count: {:?}", e);
            show_message_box(
                "Virtual Desktop Error",
                &format!("Failed to get virtual desktop count: {:?}.\nSwitching aborted.", e),
                MB_ICONERROR);
        }
    }

    if switched_successfully {
        info!("Switched successfully");
        // Get the ID of the desktop we just switched to
        let new_desktop_id_for_focus = match winvd::get_current_desktop() {
            Ok(current_desktop) => Some(current_desktop.get_index().unwrap()),
            Err(e) => {
                warn!("Could not determine current desktop ID after switch to attempt focus: {:?}", e);
                None
            }
        };

        info!("New desktop {:?}", new_desktop_id_for_focus);

        if let Some(desktop_id_to_focus) = new_desktop_id_for_focus {
            std::thread::sleep(std::time::Duration::from_millis(100)); 
            if let Err(e) = focus_a_window_on_current_desktop(desktop_id_to_focus, last_active_map) {
                warn!("Could not focus a window on the new desktop {}: {}", target_desktop_idx_0_based, e);
            }
        } else {
            warn!("Could not determine current desktop ID after switch to attempt focus.");
        }
    }
}

extern "system" fn enum_windows_proc_focus(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let data = unsafe { &mut *(lparam.0 as *mut EnumCallbackData) };
    if data.found_hwnd.is_some() { return FALSE; }

    if unsafe { IsWindowVisible(hwnd) }.as_bool() {
        let style = unsafe { GetWindowLongW(hwnd, GWL_STYLE) } as u32;
        if (style & WS_CHILD.0) != 0 { return TRUE; }
        
        let mut title_buffer: [u16; 128] = [0; 128];
        if unsafe { GetWindowTextW(hwnd, &mut title_buffer) } == 0 { return TRUE; }

        match get_desktop_by_window(hwnd) {
            Ok(desktop) if desktop.get_index().unwrap() == data.target_desktop_id => {
                info!("EnumWindows: Found candidate window {:?} ('{}') on target desktop {:?}.", 
                    hwnd, OsString::from_wide(&title_buffer).to_string_lossy(), desktop.get_index().unwrap());
                data.found_hwnd = Some(hwnd);
                return FALSE; 
            }
            _ => {}
        }
    }
    TRUE 
}

// Updated to accept and use the last_active_map
fn focus_a_window_on_current_desktop(current_desktop_id: u32, last_active_map: &LastActiveWindowMap) -> Result<()> {
    info!("Attempting to focus a window on desktop ID: {:?}", current_desktop_id);

    // 1. Try to focus the last active window for this desktop
    let remembered_hwnd_option: Option<HWND> = { // Scope for mutex guard
        let map_guard = last_active_map.lock().unwrap_or_else(|poisoned| {
            warn!("Mutex for last_active_windows_map was poisoned. Recovering.");
            poisoned.into_inner()
        });
        map_guard.get(&current_desktop_id).copied()
    };

    if let Some(remembered_hwnd) = remembered_hwnd_option {
        info!("Found remembered HWND: {:?} for desktop {:?}", remembered_hwnd, current_desktop_id);
        // Validate the remembered window
        let is_valid_window = unsafe { IsWindow(remembered_hwnd) }.as_bool();
        let is_visible = unsafe { IsWindowVisible(remembered_hwnd) }.as_bool();
        let style = unsafe { GetWindowLongW(remembered_hwnd, GWL_STYLE) } as u32;
        let is_not_child = (style & WS_CHILD.0) == 0;

        if is_valid_window && is_visible && is_not_child {
            match get_desktop_by_window(remembered_hwnd) {
                Ok(desktop_id_of_remembered) if desktop_id_of_remembered.get_index().unwrap() == current_desktop_id => {
                    info!("Attempting to set foreground to remembered window: {:?}", remembered_hwnd);
                    unsafe { BringWindowToTop(remembered_hwnd) };
                    if unsafe { SetForegroundWindow(remembered_hwnd) }.as_bool() {
                        info!("Successfully set foreground to remembered window {:?}", remembered_hwnd);
                        return Ok(()); // Focus successful
                    } else {
                        warn!("Failed to set foreground to remembered window {:?}.", remembered_hwnd);
                    }
                }
                Ok(other_id) => warn!("Remembered window {:?} is now on a different desktop: {:?}", remembered_hwnd, other_id),
                Err(e) => warn!("Could not get desktop ID for remembered window {:?}: {:?}", remembered_hwnd, e),
            }
        } else {
            info!("Remembered window {:?} is no longer valid/visible/suitable.", remembered_hwnd);
        }
    } else {
        info!("No remembered window for desktop ID: {:?}", current_desktop_id);
    }
    
    // 2. Fallback: Enumerate windows if remembered window focus failed or no remembered window
    info!("Falling back to EnumWindows to find a window on desktop ID: {:?}", current_desktop_id);
    let mut callback_data = EnumCallbackData {
        target_desktop_id: current_desktop_id,
        found_hwnd: None,
    };

    unsafe { EnumWindows(Some(enum_windows_proc_focus), LPARAM(&mut callback_data as *mut _ as isize)) };

    if let Some(hwnd_to_focus) = callback_data.found_hwnd {
        info!("EnumWindows found: {:?}. Attempting to set foreground.", hwnd_to_focus);
        unsafe { BringWindowToTop(hwnd_to_focus) }; 
        if unsafe { SetForegroundWindow(hwnd_to_focus) }.as_bool() {
            info!("Successfully set foreground window to {:?} via EnumWindows", hwnd_to_focus);
        } else {
            warn!("Failed to set foreground window to {:?} (found via EnumWindows).", hwnd_to_focus);
        }
    } else {
        info!("No suitable window found on desktop {:?} via EnumWindows to focus.", current_desktop_id);
    }
    Ok(())
}

fn handle_move_window_to_desktop(target_desktop_idx_0_based: usize) {
    info!("Attempting to MOVE foreground window to desktop index: {}", target_desktop_idx_0_based);
    
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.0 == std::ptr::null_mut() {
        error!("Failed to get foreground window handle.");
        return;
    }
    info!("Foreground window HWND: {:?}", hwnd);

    match get_desktop_count() {
        Ok(current_count_u32) => {
            let current_count = current_count_u32 as usize;
            if target_desktop_idx_0_based >= current_count {
                info!(
                    "Target desktop index {} for MOVE is out of range (current count {}). Creating new desktops.",
                    target_desktop_idx_0_based, current_count
                );
                let desktops_to_create = target_desktop_idx_0_based - current_count + 1;
                for i in 0..desktops_to_create {
                    match create_desktop() {
                        Ok(_new_desktop_id) => info!(
                            "Created new desktop for MOVE (iteration {}/{})",
                             i + 1, desktops_to_create
                        ),
                        Err(e) => {
                            error!("Failed to create new desktop for MOVE (iteration {}): {:?}", i + 1, e);
                            show_message_box(
                                "Desktop Creation Error",
                                &format!("Failed to create a new virtual desktop for move: {:?}.", e),
                                MB_ICONERROR);
                            return;
                        }
                    }
                }
            }
            match move_window_to_desktop(target_desktop_idx_0_based as u32, &hwnd) {
                Ok(_) => info!("Successfully moved window {:?} to desktop index {}.", hwnd, target_desktop_idx_0_based),
                Err(e) => {
                    error!("Failed to move window {:?} to desktop index {}: {:?}", hwnd, target_desktop_idx_0_based, e);
                    show_message_box(
                        "Move Window Error",
                        &format!("Failed to move window: {:?}.\nEnsure the window is valid and not minimized/special.", e),
                        MB_ICONERROR);
                }
            }
        }
        Err(e) => {
            error!("Failed to get virtual desktop count for MOVE operation: {:?}", e);
             show_message_box(
                "Virtual Desktop Error",
                &format!("Failed to get virtual desktop count for move: {:?}.\nMove aborted.", e),
                MB_ICONERROR);
        }
    }
}

fn show_about_dialog() {
    let message = format!(
        "{}\nVersion: {}\n\n\
        Allows switching virtual desktops 1-10 using RCtrl + <Number> (RCtrl+0 for Desktop 10).\n\n\
        Author: Joona Kulmala <jmkulmala@gmail.com>.",
        APP_NAME,
        env!("CARGO_PKG_VERSION")
    );
    show_message_box("About", &message, MB_ICONINFORMATION);
}

fn show_message_box(title: &str, text: &str, flags: MESSAGEBOX_STYLE) {
    let lpcwstr_title: Vec<u16> = title.encode_utf16().chain(std::iter::once(0)).collect();
    let lpcwstr_text: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();

    unsafe {
        MessageBoxW(
            HWND(std::ptr::null_mut()),
            PCWSTR(lpcwstr_text.as_ptr()),
            PCWSTR(lpcwstr_title.as_ptr()),
            flags,
        );
    }
}

