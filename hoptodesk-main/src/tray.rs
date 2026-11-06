use crate::client::translate;
use crate::ipc::Data;
use hbb_common::tokio;
use hbb_common::log;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Beacon: terminate every other process running this exe (main UI,
/// portable-mode server helpers). The installed service process lives at a
/// different path and is also protected by permissions, so it survives.
#[cfg(target_os = "windows")]
fn exit_other_app_processes() {
    use hbb_common::sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes();
    let mut path = std::env::current_exe().unwrap_or_default();
    if let Ok(linked) = path.read_link() {
        path = linked;
    }
    let path = path.to_string_lossy().to_lowercase();
    let my_pid = std::process::id().to_string();
    for (_, p) in sys.processes().iter() {
        if p.pid().to_string() == my_pid {
            continue;
        }
        let mut cur = p.exe().to_path_buf();
        if let Ok(linked) = cur.read_link() {
            cur = linked;
        }
        if cur.to_string_lossy().to_lowercase() != path {
            continue;
        }
        log::info!("Tray exit: stopping process {}", p.pid());
        let _ = p.kill();
    }
}

pub fn start_tray() {
    #[cfg(target_os = "macos")]
    if crate::platform::is_prelogin() {
        run_main_loop_for_input();
        return;
    }
    if crate::ui_interface::get_builtin_option(hbb_common::config::keys::OPTION_HIDE_TRAY) == "Y" {
        #[cfg(not(target_os = "macos"))]
        {
            return;
        }
    }

    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    crate::server::check_zombie();
    
    if let Err(e) = make_tray() {
        log::error!("Failed to start the tray icon: {}", e);
        #[cfg(target_os = "macos")]
        run_main_loop_for_input();
    }
}

#[cfg(target_os = "macos")]
fn run_main_loop_for_input() {
    use core_foundation::runloop::{kCFRunLoopDefaultMode, CFRunLoopRunInMode};
    loop {
        unsafe {
            CFRunLoopRunInMode(kCFRunLoopDefaultMode, 3600.0, 0);
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[allow(unused_assignments)]
pub fn make_tray() -> hbb_common::ResultType<()> {
    // https://github.com/tauri-apps/tray-icon/blob/dev/examples/tao.rs
    use hbb_common::anyhow::Context;
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tray_icon::{
        menu::{IsMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem},
        TrayIcon, TrayIconBuilder, TrayIconEvent as TrayEvent,
    };
    let icon;
    #[cfg(target_os = "macos")]
    {
		icon = include_bytes!("../res/mac-tray-light-x2.png"); // use as template, so color is not important
    }
    #[cfg(not(target_os = "macos"))]
    {
        icon = include_bytes!("../res/tray-icon.ico");
    }

    let (icon_rgba, icon_width, icon_height) = {
        let image = image::load_from_memory(icon)
            .context("Failed to open icon path")?
            .into_rgba8();
        let (width, height) = image.dimensions();
        let rgba = image.into_raw();
        (rgba, width, height)
    };
    let icon = tray_icon::Icon::from_rgba(icon_rgba, icon_width, icon_height)
        .context("Failed to open icon")?;

    #[cfg(windows)]
    let event_loop = EventLoopBuilder::new().build();
    
    #[cfg(not(windows))]
    #[allow(unused_mut)]
    let mut event_loop = EventLoopBuilder::new().build();

    let tray_menu = Menu::new();
    let quit_i = MenuItem::new(translate("Exit".to_owned()), true, None);
    let open_i = MenuItem::new(translate("Open".to_owned()), true, None);
    let name_i = MenuItem::new("", false, None);
    let name_sep = PredefinedMenuItem::separator();
    let has_dashboard = crate::dashboard::is_linked() && !hbb_common::config::is_help_desk_hidden();
    let ticket_i = if has_dashboard {
        Some(MenuItem::new(translate("Submit Ticket".to_owned()), true, None))
    } else {
        None
    };
    let mut menu_items: Vec<&dyn IsMenuItem> = vec![&open_i];
    if let Some(ref ti) = ticket_i {
        menu_items.push(ti);
    }
    if !hbb_common::config::is_close_protected() {
        menu_items.push(&quit_i);
    }
    tray_menu.append_items(&menu_items).ok();
    let tooltip = |count: usize, name: &str, ai: &str| {
        let status = if count == 0 {
            format!(
                "{} {}",
                crate::get_app_name(),
                translate("Service is running".to_owned()),
            )
        } else {
            format!(
                "{} - {}\n{}",
                crate::get_app_name(),
                translate("Ready".to_owned()),
                if count == 1 {
                    translate("1 session".to_owned())
                } else {
                    translate("{} sessions".to_owned()).replacen("{}", &count.to_string(), 1)
                },
            )
        };
        let text = if name.is_empty() { status } else { format!("{}\n{}", name, status) };
        let text = if ai.is_empty() { text } else { format!("{}\n{}", text, ai_working_text(ai)) };
        let mut fitted = String::new();
        let mut units = 0;
        for c in text.chars() {
            units += c.len_utf16();
            if units > 120 {
                break;
            }
            fitted.push(c);
        }
        fitted
    };
	#[cfg_attr(not(windows), allow(unused_assignments, unused_variables))]
	let mut _tray_icon: Arc<Mutex<Option<TrayIcon>>> = Default::default();

    let menu_channel = MenuEvent::receiver();
    let tray_channel = TrayEvent::receiver();
    let (ipc_sender, ipc_receiver) = std::sync::mpsc::channel::<Data>();

    let open_func = move || {
        if cfg!(not(feature = "flutter")) {
            crate::run_me::<&str>(vec![]).ok();
            return;
        }
        // Flutter on Windows: launching the exe again hits the single-instance
        // check, which shows the existing (possibly hidden) main window.
        #[cfg(target_os = "windows")]
        {
            crate::run_me::<&str>(vec![]).ok();
        }
        #[cfg(target_os = "macos")]
        crate::platform::macos::handle_application_should_open_untitled_file();
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        {
            // Do not use "xdg-open", it won't read the config.
            if crate::dbus::invoke_new_connection(crate::get_uri_prefix()).is_err() {
                if let Ok(task) = crate::run_me::<&str>(vec![]) {
                    crate::server::CHILD_PROCESS.lock().unwrap().push(task);
                }
            }
        }
    };

    #[cfg(windows)]
    {
        let sender = ipc_sender.clone();
        std::thread::spawn(move || {
            start_query_session_count(sender);
        });
    }
    {
        let sender = ipc_sender.clone();
        std::thread::spawn(move || {
            start_query_device_name(sender);
        });
    }
    if has_dashboard {
        std::thread::spawn(move || {
            start_query_ticket_replies(ipc_sender.clone());
        });
    }
    let mut last_ticket_reply_counter: Option<u64> = None;
    #[allow(unused_mut)]
    let mut session_count = 0usize;
    let mut device_name = String::new();
    let mut ai_actor = String::new();
    let mut name_shown = false;
    #[cfg(windows)]
    let mut last_click = std::time::Instant::now();
    #[cfg(target_os = "macos")]
    {
        use tao::platform::macos::EventLoopExtMacOS;
        event_loop.set_activation_policy(tao::platform::macos::ActivationPolicy::Accessory);
    }
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::WaitUntil(
            std::time::Instant::now() + std::time::Duration::from_millis(100),
        );

        if let tao::event::Event::NewEvents(tao::event::StartCause::Init) = event {
            if crate::ui_interface::get_builtin_option(hbb_common::config::keys::OPTION_HIDE_TRAY) == "Y" {
                return;
            }
            // We create the icon once the event loop is actually running
            // to prevent issues like https://github.com/tauri-apps/tray-icon/issues/90
            let tray = TrayIconBuilder::new()
                .with_menu(Box::new(tray_menu.clone()))
                .with_tooltip(tooltip(0, "", ""))
                .with_icon(icon.clone())
                .with_icon_as_template(true) // mac only
                .build();
            match tray {
                Ok(tray) => _tray_icon = Arc::new(Mutex::new(Some(tray))),
                Err(err) => {
                    log::error!("Failed to create tray icon: {}", err);
                }
            };

            // We have to request a redraw here to have the icon actually show up.
            // Tao only exposes a redraw method on the Window so we use core-foundation directly.
            #[cfg(target_os = "macos")]
            unsafe {
                use core_foundation::runloop::{CFRunLoopGetMain, CFRunLoopWakeUp};

                let rl = CFRunLoopGetMain();
                CFRunLoopWakeUp(rl);
            }
        }

        if let Ok(event) = menu_channel.try_recv() {
            if event.id == quit_i.id() {
                    // Beacon: the tray "Exit" is the only full-quit path
                    // (closing the main window only hides it to the tray).
                    // Stop all sibling processes of this exe — the main UI
                    // and the portable-mode server thread live in them.
                    #[cfg(target_os = "windows")]
                    exit_other_app_processes();
                    *control_flow = ControlFlow::Exit;
            } else if event.id == open_i.id() {
				open_func();
            } else if let Some(ref ti) = ticket_i {
                if event.id == ti.id() {
                    crate::run_me(vec!["--ticket"]).ok();
                }
            }
        }

        if let Ok(_event) = tray_channel.try_recv() {
            #[cfg(target_os = "windows")]
            match _event {
                TrayEvent::Click {
                    button,
                    button_state,
                    ..
                } => {
                    if button == tray_icon::MouseButton::Left
                        && button_state == tray_icon::MouseButtonState::Up
                    {
                        if last_click.elapsed() < std::time::Duration::from_secs(1) {
                            return;
                        }
                        open_func();
                        last_click = std::time::Instant::now();
                    }
                }
                _ => {}
            }
        }

        if let Ok(data) = ipc_receiver.try_recv() {
            match data {
                #[cfg(windows)]
                Data::ControlledSessionCount(count) => {
                    session_count = count;
                    _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_tooltip(Some(tooltip(session_count, &device_name, &ai_actor))));
                }
                Data::Options(Some(options)) => {
                    device_name = options.get("dashboard_device_name").cloned().unwrap_or_default();
                    if device_name.is_empty() {
                        if name_shown {
                            tray_menu.remove(&name_i).ok();
                            tray_menu.remove(&name_sep).ok();
                            name_shown = false;
                        }
                    } else {
                        name_i.set_text(&device_name);
                        if !name_shown {
                            tray_menu.prepend(&name_sep).ok();
                            tray_menu.prepend(&name_i).ok();
                            name_shown = true;
                        }
                    }
                    _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_tooltip(Some(tooltip(session_count, &device_name, &ai_actor))));
                }
                Data::AiActivity(actor) => {
                    if !actor.is_empty() && actor != ai_actor {
                        show_ai_working_notification(&actor);
                    }
                    ai_actor = actor;
                    _tray_icon
                        .lock()
                        .unwrap()
                        .as_mut()
                        .map(|t| t.set_tooltip(Some(tooltip(session_count, &device_name, &ai_actor))));
                }
                Data::TicketReplyCounter(count) => {
                    if last_ticket_reply_counter.map(|v| count > v).unwrap_or(false)
                        && !crate::check_process("--ticket", true)
                    {
                        show_ticket_reply_notification();
                    }
                    last_ticket_reply_counter = Some(count);
                }
                _ => {}
            }
        }
    });
}

#[tokio::main(flavor = "current_thread")]
async fn start_query_ticket_replies(sender: std::sync::mpsc::Sender<Data>) {
    let mut last: Option<u64> = None;
    let mut last_ai: Option<String> = None;
    loop {
        if let Ok(mut c) = crate::ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(tokio::time::interval(Duration::from_secs(3)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                break;
                            }
                            Ok(Some(Data::TicketReplyCounter(count))) => {
                                if last != Some(count) {
                                    last = Some(count);
                                    sender.send(Data::TicketReplyCounter(count)).ok();
                                }
                            }
                            Ok(Some(Data::AiActivity(actor))) => {
                                if last_ai.as_ref() != Some(&actor) {
                                    last_ai = Some(actor.clone());
                                    sender.send(Data::AiActivity(actor)).ok();
                                }
                            }
                            _ => {}
                        }
                    }

                    _ = timer.tick() => {
                        c.send(&Data::TicketReplyCounter(0)).await.ok();
                        c.send(&Data::AiActivity(String::new())).await.ok();
                    }
                }
            }
        }
        hbb_common::sleep(1.).await;
    }
}

fn ai_working_text(actor: &str) -> String {
    translate("{} is working on this computer".to_owned()).replacen("{}", actor, 1)
}

fn show_ai_working_notification(actor: &str) {
    #[cfg(target_os = "macos")]
    {
        static SET_APP: std::sync::Once = std::sync::Once::new();
        SET_APP.call_once(|| {
            let _ = notify_rust::set_application("com.beacon.beacon");
        });
    }
    let _ = notify_rust::Notification::new()
        .summary(&crate::get_app_name())
        .body(&ai_working_text(actor))
        .show();
}

fn show_ticket_reply_notification() {
    #[cfg(target_os = "macos")]
    {
        static SET_APP: std::sync::Once = std::sync::Once::new();
        SET_APP.call_once(|| {
            let _ = notify_rust::set_application("com.beacon.beacon");
        });
    }
    let _ = notify_rust::Notification::new()
        .summary(&crate::get_app_name())
        .body(&translate("Support replied to your ticket".to_owned()))
        .show();
}

#[tokio::main(flavor = "current_thread")]
async fn start_query_device_name(sender: std::sync::mpsc::Sender<Data>) {
    let mut last: Option<String> = None;
    loop {
        if let Ok(mut c) = crate::ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(tokio::time::interval(Duration::from_secs(3)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                break;
                            }
                            Ok(Some(Data::Options(Some(options)))) => {
                                let name: String = if options.get("hide-device-name").map(|v| v == "Y").unwrap_or(false) {
                                    String::new()
                                } else {
                                    options.get("dashboard_device_name").map(|v| v.chars().take(60).collect()).unwrap_or_default()
                                };
                                if last.as_ref() != Some(&name) {
                                    last = Some(name.clone());
                                    let update = std::collections::HashMap::from([("dashboard_device_name".to_owned(), name)]);
                                    sender.send(Data::Options(Some(update))).ok();
                                }
                            }
                            _ => {}
                        }
                    }

                    _ = timer.tick() => {
                        c.send(&Data::Options(None)).await.ok();
                    }
                }
            }
        }
        hbb_common::sleep(1.).await;
    }
}

#[cfg(windows)]
#[tokio::main(flavor = "current_thread")]
async fn start_query_session_count(sender: std::sync::mpsc::Sender<Data>) {
    let mut last_count = 0;
    loop {
        if let Ok(mut c) = crate::ipc::connect(1000, "").await {
            let mut timer = crate::rustdesk_interval(tokio::time::interval(Duration::from_secs(1)));
            loop {
                tokio::select! {
                    res = c.next() => {
                        match res {
                            Err(err) => {
                                log::error!("ipc connection closed: {}", err);
                                break;
                            }

                            Ok(Some(Data::ControlledSessionCount(count))) => {
                                if count != last_count {
                                    last_count = count;
                                    sender.send(Data::ControlledSessionCount(count)).ok();
                                }
                            }
                            _ => {}
                        }
                    }

                    _ = timer.tick() => {
                        c.send(&Data::ControlledSessionCount(0)).await.ok();
                    }
                }
            }
        }
        hbb_common::sleep(1.).await;
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tray_icon_decodes() {
        let ico = include_bytes!("../res/tray-icon.ico");
        image::load_from_memory(ico)
            .expect("tray-icon.ico must decode; the tray silently vanishes if it cannot");
    }
}
