use hbb_common::{config, log};
#[cfg(any(target_os = "windows", target_os = "macos", target_os = "linux", target_os = "freebsd"))]
use std::fs;
#[cfg(target_os = "windows")]
use std::process::{id};
					
use hbb_common::{config::{Config},};

lazy_static::lazy_static! {
    pub(crate) static ref SWITCH_SIDES_INVOKED_UUID: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
}

#[macro_export]
macro_rules! my_println{
    ($($arg:tt)*) => {
        #[cfg(not(windows))]
        println!("{}", format_args!($($arg)*));
        #[cfg(windows)]
        crate::platform::message_box(
            &format!("{}", format_args!($($arg)*))
        );
    };
}

/// shared by the flutter and desktop main functions
///
/// [Note]
/// If it returns [`None`], then the process will terminate, and flutter gui will not be started.
/// If it returns [`Some`], then the process will continue, and flutter gui will be started.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub fn core_main() -> Option<Vec<String>> {
    wireui::wait_for_relaunching_parent();
    wireui::set_state_dir(Config::path(""));
    #[cfg(target_os = "freebsd")]
    {
        if std::env::var("SSL_CERT_FILE").is_err() {
            for path in &["/usr/local/etc/ssl/cert.pem", "/usr/local/share/certs/ca-root-nss.crt", "/etc/ssl/cert.pem"] {
                if std::path::Path::new(path).exists() {
                    std::env::set_var("SSL_CERT_FILE", path);
                    break;
                }
            }
        }
    }
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    {
        let raw_args: Vec<String> = std::env::args().collect();
        if raw_args.len() >= 2 && raw_args[1] == "--cups-print-job" {
            let code = match crate::platform::linux::cups_print_job(
                raw_args.get(2).map(|s| s.as_str()),
            ) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("cups print job failed: {}", e);
                    1
                }
            };
            std::process::exit(code);
        }
    }
    #[cfg(windows)]
    crate::platform::sanitize_std_handles();
    if std::env::args().nth(1).as_deref() == Some("--import-config") {
        if let Some(path) = std::env::args().nth(2) {
            hbb_common::init_log(false, "import-config");
            import_config(&path);
        }
        return None;
    }
    #[cfg(windows)]
    if std::env::args().nth(1).as_deref() == Some("--send-file") {
        hbb_common::init_log(false, "send-file");
        crate::send_file::send(std::env::args().skip(2).collect());
        return None;
    }
    std::thread::spawn(hbb_common::fs::sweep_stale_bundles);
    hbb_common::config::migrate_peer_keyboard_mode_map_to_auto();
    crate::common::apply_builtin_custom_client();
    crate::common::apply_managed_enrollment_config();
    std::thread::spawn(crate::dashboard::purge_old_recordings);

    let mut args: Vec<String> = Vec::new();
    let mut flutter_args: Vec<String> = Vec::new();

    let mut _is_elevate = false;
    let mut _is_run_as_system = false;
    let mut _is_quick_support = false;
    let mut _is_flutter_invoke_new_connection = false;
    let mut no_server = false;
    let mut arg_exe = Default::default();

    let mut env_args_iter = std::env::args().peekable();
    let mut arg_idx = 0;

    while let Some(current_arg) = env_args_iter.next() {
        if arg_idx == 0 {
            arg_exe = current_arg.clone();
            arg_idx += 1;
            continue;
        }

        let mut arg_handled_as_switch_uuid = false;

        if current_arg == "--switch-uuid" || current_arg == "--switch_uuid" {
            if let Some(uuid_peek_val) = env_args_iter.peek() {
                if !uuid_peek_val.starts_with("--") {
                    let uuid_value = env_args_iter.next().unwrap();
                    if let Ok(mut global_uuid_lock) = SWITCH_SIDES_INVOKED_UUID.lock() {
                        *global_uuid_lock = Some(uuid_value);
                        log::info!("CLI: Parsed --switch-uuid with value: {:?}", *global_uuid_lock);
                    } else {
                        log::error!("CLI: Failed to lock SWITCH_SIDES_INVOKED_UUID for writing.");
                    }
                    arg_handled_as_switch_uuid = true;
                } else {
                    log::warn!("CLI: --switch-uuid found, but the next token ('{}') looks like another flag, not a value.", uuid_peek_val);
                }
            } else {
                log::warn!("CLI: --switch-uuid found as the last argument without a value.");
            }
        }

        if arg_handled_as_switch_uuid {
            // If --switch-uuid (and its value) was processed, it should not be pushed to `args`
            // and no further processing for this argument in the old logic path.
        } else {
            let mut is_special_bool_flag_not_pushed_to_args = false;

            #[cfg(feature = "flutter")]
            if [
                "--connect",
                "--play",
                "--file-transfer",
                "--view-camera",
                "--port-forward",
                "--rdp",
                "--select-for-print",
            ]
            .contains(&current_arg.as_str())
            {
                _is_flutter_invoke_new_connection = true;
            }

            if current_arg == "--elevate" {
                _is_elevate = true;
                is_special_bool_flag_not_pushed_to_args = true;
            } else if current_arg == "--run-as-system" {
                _is_run_as_system = true;
                is_special_bool_flag_not_pushed_to_args = true;
            } else if current_arg == "--quick_support" {
                _is_quick_support = true;
                is_special_bool_flag_not_pushed_to_args = true;
            } else if current_arg == "--no-server" {
                no_server = true;
                is_special_bool_flag_not_pushed_to_args = true;
            } else if current_arg.starts_with("-psn_") {
                is_special_bool_flag_not_pushed_to_args = true;
            }

            if !is_special_bool_flag_not_pushed_to_args {
                args.push(current_arg.clone());
            }
        }
        arg_idx += 1;
    }

    let click_setup = cfg!(windows) && args.is_empty() && crate::common::is_setup(&arg_exe);
    if click_setup && !config::is_disable_installation() {
        args.push("--install".to_owned());
        flutter_args.push("--install".to_string());
    }
    // Auto-upgrade: if running a newer version than what's installed as the service,
    // trigger the install flow to replace the old service binary.
    #[cfg(windows)]
    if args.is_empty() && !click_setup && !config::is_disable_installation() {
        if crate::platform::is_installed() {
            let (_, _, _, installed_exe) = crate::platform::get_install_info();
            let current_exe = std::env::current_exe()
                .unwrap_or_default()
                .to_string_lossy()
                .to_lowercase();
            if !current_exe.is_empty() && current_exe != installed_exe.to_lowercase() {
                let installed_version = crate::platform::get_installed_version();
                let cur = hbb_common::get_version_number(crate::VERSION);
                let inst = hbb_common::get_version_number(&installed_version);
                if cur > inst {
                    log::info!(
                        "Auto-upgrade: running version {} > installed version {}, triggering install",
                        crate::VERSION, installed_version
                    );
                    args.push("--install".to_owned());
                    flutter_args.push("--install".to_string());
                }
            }
        }
    }
    if args.contains(&"--noinstall".to_string()) {
        args.clear();
    }
    // Attach to parent console early for stdio-style flags on Windows.
    // Must happen before any Rust I/O so stdin/stdout handles are valid.
    #[cfg(windows)]
    if args.contains(&"--get-id".to_string())
        || args.contains(&"--bench-codec".to_string())
        || args.contains(&"--version".to_string())
    {
        crate::platform::attach_console_for_stdio();
    }
    if args.len() > 0 && args[0] == "--version" {
        println!("{}", crate::VERSION);
        return None;
    }
    if cfg!(any(target_os = "linux", target_os = "freebsd"))
        && args.first().map_or(false, |arg| arg.contains("://"))
    {
        args.insert(0, "--connect".to_owned());
    }
    let mut log_name = "".to_owned();
    const NO_LOG_DIR_ARGS: &[&str] = &["--check-hwcodec-config", "--get-id", "--version"];
    if args.len() > 0 && args[0].starts_with("--") && !NO_LOG_DIR_ARGS.contains(&args[0].as_str()) {
        let name = args[0].replace("--", "");
        if !name.is_empty() {
            log_name = name;
        }
    }
    hbb_common::init_log(false, &log_name);
    #[cfg(feature = "fips")]
    crate::fips::require_approved_mode();
    #[cfg(windows)]
    if crate::platform::is_installed() && !crate::platform::is_root() {
        Config::keep_options_in_memory(&SERVICE_ONLY_OPTIONS);
        std::fs::remove_file(Config::path("Enrolled.key")).ok();
    }

    // linux uni (url) go here.
    #[cfg(all(target_os = "linux", feature = "flutter"))]
    if args.len() > 0 && args[0].starts_with("hoptodesk:") {
        return try_send_by_dbus(args[0].clone());
    }

    #[cfg(windows)]
    if !crate::platform::is_installed()
        && args.is_empty()
        && _is_quick_support
        && !_is_elevate
        && !_is_run_as_system
    {
        use crate::portable_service::client;
        if let Err(e) = client::start_portable_service(client::StartPara::Direct) {
            log::error!("Failed to start portable service:{:?}", e);
        }
    }
    #[cfg(windows)]
    if !crate::platform::is_installed() && (_is_elevate || _is_run_as_system) {
        crate::platform::elevate_or_run_as_system(click_setup, _is_elevate, _is_run_as_system);
        return None;
    }
    if args.is_empty()
		|| args[0] == "--qs"
		|| crate::common::is_empty_uni_link(&args[0])
		|| (!args[0].starts_with("--")
			&& std::env::current_exe().ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().contains("-qs"))).unwrap_or(false)) {
        // Beacon: single instance — relaunching the exe while the main UI
        // is already running (possibly hidden in the tray) just brings that
        // window back instead of stacking another background copy.
        #[cfg(all(windows, feature = "flutter"))]
        if args.is_empty() && crate::check_process("", true) {
            crate::platform::windows::show_existing_main_window();
            return None;
        }
        std::thread::spawn(move || crate::start_server(false, no_server));
        // Beacon: the tray also runs in portable mode (upstream only starts
        // it for installed builds), otherwise a hidden main window leaves no
        // way to restore the UI or fully quit the app.
        #[cfg(windows)]
        if !crate::check_process("--tray", true) {
            if let Ok(exe) = std::env::current_exe() {
                std::process::Command::new(exe).arg("--tray").spawn().ok();
            }
        }
        // Beacon LAN-only: the cloud dashboard is stripped.
        if !crate::lan_only::LAN_ONLY && (crate::dashboard::is_linked() || !crate::dashboard::get_invite_code().is_empty() || !crate::dashboard::get_prefilled_enrollment_token().is_empty() || crate::dashboard::has_enrollment_sentinel()) {
            std::thread::spawn(|| crate::dashboard::start());
        }
    } else {
        #[cfg(windows)]
        {
            use crate::platform;
            if args[0] == "--uninstall" {
                if hbb_common::config::is_tamper_protected() {
                    let supplied = args
                        .iter()
                        .position(|a| a == "--uninstall-code")
                        .and_then(|i| args.get(i + 1))
                        .cloned()
                        .or_else(|| {
                            args.iter().find_map(|a| {
                                a.strip_prefix("--uninstall-code=").map(|s| s.to_owned())
                            })
                        });
                    match supplied {
                        Some(code) => {
                            if crate::dashboard::verify_uninstall_code(&code) {
                                if let Err(err) = platform::uninstall_me(true) {
                                    log::error!("Failed to uninstall: {}", err);
                                }
                            } else {
                                crate::dashboard::queue_tamper_attempt("uninstall");
                                log::error!("Uninstall refused: incorrect code");
                                std::process::exit(5);
                            }
                            return None;
                        }
                        None => {
                            return Some(vec!["--uninstall".to_owned()]);
                        }
                    }
                }
                if let Err(err) = platform::uninstall_me(true) {
                    log::error!("Failed to uninstall: {}", err);
                }
                return None;
            } else if args[0] == "--uninstall-htd-idd" {
                if let Err(err) = crate::virtual_display_manager::htd_idd::uninstall_driver() {
                    log::error!("Failed to uninstall the virtual display driver: {}", err);
                }
                return None;
            } else if args[0] == "--uninstall-send-menu" {
                if let Err(err) = crate::send_file::remove_top_menu() {
                    log::error!("Failed to remove the send menu: {}", err);
                }
                return None;
            } else if args[0] == "--after-install" {
                if let Err(err) = platform::run_after_install() {
                    log::error!("Failed to after-install: {}", err);
                }
                return None;
            } else if args[0] == "--before-uninstall" {
                #[cfg(windows)]
                if hbb_common::config::is_tamper_protected() {
                    crate::dashboard::queue_tamper_attempt("service_stop");
                    log::error!("Service removal refused: tamper protection");
                    std::process::exit(5);
                }
                if let Err(err) = platform::run_before_uninstall() {
                    log::error!("Failed to before-uninstall: {}", err);
                }
                return None;
            } else if args[0] == "--silent-install" || args[0] == "--silent-install-noshortcuts" {
                let no_shortcuts = args[0] == "--silent-install-noshortcuts";
                #[cfg(windows)]
                if !crate::platform::is_elevated(None).unwrap_or(true) {
                    std::process::exit(crate::platform::relaunch_install_elevated(&args));
                }
                let mut wait_for_enrollment: Option<u64> = None;
                let mut install_options_override: Option<String> = None;
                let mut install_path_override: Option<String> = None;
                let mut no_startup_flag = false;
                let mut launch_after = false;
                let mut proxy_url = String::new();
                let mut proxy_username = String::new();
                let mut proxy_password = String::new();
                let mut i = 1;
                while i < args.len() {
                    let arg = args[i].as_str();
                    if arg == "--enrollment-token" && i + 1 < args.len() {
                        crate::dashboard::set_prefilled_enrollment_token(&args[i + 1]);
                        i += 2;
                    } else if arg == "--invite-code" && i + 1 < args.len() {
                        let code = args[i + 1].trim().to_string();
                        if !code.is_empty() {
                            let p = Config::shared_path("InviteCode.toml");
                            if let Some(parent) = p.parent() {
                                let _ = fs::create_dir_all(parent);
                            }
                            let _ = fs::write(&p, &code);
                            Config::set_option("invite_code".to_owned(), code);
                        }
                        i += 2;
                    } else if arg == "--wait-for-enrollment" {
                        wait_for_enrollment = Some(60);
                        i += 1;
                    } else if let Some(secs_str) = arg.strip_prefix("--wait-for-enrollment=") {
                        wait_for_enrollment = Some(secs_str.parse().unwrap_or(60));
                        i += 1;
                    } else if arg == "--install-options" && i + 1 < args.len() {
                        install_options_override = Some(args[i + 1].clone());
                        i += 2;
                    } else if arg == "--install-path" && i + 1 < args.len() {
                        install_path_override = Some(args[i + 1].clone());
                        i += 2;
                    } else if arg == "--no-startup" || arg == "--nostartup" {
                        no_startup_flag = true;
                        i += 1;
                    } else if arg == "--launch-after" {
                        launch_after = true;
                        i += 1;
                    } else if arg == "--proxy-url" && i + 1 < args.len() {
                        proxy_url = args[i + 1].trim().to_string();
                        i += 2;
                    } else if arg == "--proxy-username" && i + 1 < args.len() {
                        proxy_username = args[i + 1].to_string();
                        i += 2;
                    } else if arg == "--proxy-password" && i + 1 < args.len() {
                        proxy_password = args[i + 1].to_string();
                        i += 2;
                    } else {
                        i += 1;
                    }
                }

                if !proxy_url.is_empty() {
                    let p = Config::shared_path("Proxy.json");
                    if let Some(parent) = p.parent() {
                        let _ = fs::create_dir_all(parent);
                    }
                    let payload = serde_json::json!({
                        "url": proxy_url,
                        "username": proxy_username,
                        "password": proxy_password,
                    });
                    let _ = fs::write(&p, payload.to_string());
                }

                let install_options = install_options_override
                    .or_else(|| if no_shortcuts { Some(String::new()) } else { platform::installed_shortcut_options() })
                    .unwrap_or_else(|| "desktopicon startmenu".to_owned());
                let install_result = platform::install_me(
                    &install_options,
                    install_path_override.unwrap_or_default(),
                    !launch_after,
                    args.len() > 1,
                    no_startup_flag,
                );
                if let Err(e) = &install_result {
                    log::error!("install_me failed: {}", e);
                    std::process::exit(1);
                }

                if let Some(timeout_secs) = wait_for_enrollment {
                    let sentinel = Config::shared_path("Enrolled.toml");
                    let start = std::time::Instant::now();
                    while start.elapsed().as_secs() < timeout_secs {
                        if sentinel.exists() {
                            return None;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(500));
                    }
                    std::process::exit(2);
                }
                return None;
            } else if args[0] == "--extract" {
                return None;
            } else if args[0] == "--portable-service" {
                crate::platform::elevate_or_run_as_system(
                    click_setup,
                    _is_elevate,
                    _is_run_as_system,
                );
                return None;
            }
        }
		if args[0] == "--connect" && args.len() >= 2 {
			if let Some(scheme_end) = args[1].find("://") {
				let normalized = format!("beacon://{}", &args[1][scheme_end + 3..]);
				args[1] = normalized;
			}
			let input = &args[1];

			if input != "hoptodesk:///" {

				if input.starts_with("hoptodesk://connect/") {

					let id_with_password = input.strip_prefix("hoptodesk://connect/").unwrap();
					let mut new_args = args.clone();  // Create a new vector to modify

					let mut parts = id_with_password.splitn(5, '/');
					if let Some(id) = parts.next().map(str::to_owned) {
						new_args[1] = id.to_string();
					}
					if let Some(password) = parts.next() {
						new_args.push(crate::dashboard::percent_decode_path(password));
						
					}


					if let Some(token) = parts.next() {
						if !token.is_empty() && token.len() != 16 && token.len() != 32 {
							crate::dashboard::set_pending_quick_connect_token(token);
						}
					}
					if let Some(tokenex) = parts.next() {
						if !tokenex.is_empty() {
							fs::write(&Config::path("LastToken.toml"), tokenex).expect("Failed to write tokenex to file");
						}
					}
					if let Some(name) = parts.next() {
						let decoded = crate::dashboard::percent_decode_path(name);
						let alias = decoded.trim();
						if !alias.is_empty() {
							crate::ui_interface::set_peer_option(new_args[1].clone(), "alias".to_owned(), alias.to_owned());
						}
					}					
					
					args = new_args;  // Assign the modified vector back to args
				} else if input.starts_with("hoptodesk://filetransfer/") {
                    log::info!("Staring file transfer...");
					if let Some(id) = input.strip_prefix("hoptodesk://filetransfer/").map(str::to_owned) {
						args[1] = id.to_string();
						args[0] = "--file-transfer".to_string();
					}
				} else if input.starts_with("hoptodesk://sync/") {
					log::info!("Staring sync...");
					if let Some(id) = input.strip_prefix("hoptodesk://sync/").map(|s| s.trim_end_matches('/').to_owned()) {
						if id.len() > 32 || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
							std::process::exit(1);
						}
						args[1] = id.to_string();
						if args[1].is_empty() {
							hbb_common::config::Config::set_option("custom-api-url".to_owned(), "".to_owned());
						} else {
							hbb_common::config::Config::set_option("custom-api-url".to_owned(), format!("https://api.hoptodesk.com/?n={}", args[1]));
						}
						std::process::exit(0);
					}
				} else if let Some(rest) = input.strip_prefix("hoptodesk://") {
					let candidate = rest.split('/').next().unwrap_or("");
					if !candidate.is_empty() && candidate.chars().all(|c| c.is_ascii_digit()) {
						args[1] = candidate.to_owned();
					}
				}
			}
		}
		if args[0] == "--remove" {
            if args.len() == 2 {
                // sleep a while so that process of removed exe exit
                std::thread::sleep(std::time::Duration::from_secs(1));
                std::fs::remove_file(&args[1]).ok();
                return None;
            }
        } else if args[0] == "--tray" {
            if !crate::check_process("--tray", true) {
                crate::tray::start_tray();
            }
            return None;
        } else if args[0] == "--changeid" {
			let config_path = Config::path("HopToDesk.toml");
			if let Ok(_metadata) = fs::metadata(&config_path) {
				let content = std::fs::read_to_string(&config_path).unwrap_or_else(|err| {
					log::error!(
						"Error reading file: {:?}({})",
						config_path.to_str(),
						err
					);
					String::new()
				});

				let filtered_content: String = content
					.lines()
					.filter(|line| !line.starts_with("enc_id = ") && !line.starts_with("salt = "))
					.map(|line| format!("{}\n", line))
					.collect();

				if let Err(err) = fs::write(&config_path, filtered_content) {
					log::error!("Error writing file: {:?}({})", config_path.to_str(), err);
				} else {
					log::info!("ID changed.");
				}
			}
        } else if args[0] == "--remoteupdate" {
			// Beacon: the legacy sciter UI is not compiled in flutter builds.
			#[cfg(all(windows, not(feature = "flutter")))]
			{
				let mut remoteupdate_arg = vec![String::from("--remoteupdate")];
				log::info!("Run with --remoteupdate");
				crate::ui::start(&mut remoteupdate_arg);
				std::process::exit(0);
			}
        } else if cfg!(windows) && (args[0] == "--update" || args[0] == "--updatefromremote") {
		    log::info!("Updating from --update");
			#[cfg_attr(not(windows), allow(unused_variables))]
			let exe_path = std::env::current_exe().expect("Failed to get current executable path");
		    #[cfg(windows)]
		    if let Ok(_metadata) = fs::metadata(Config::path("UpdatePath.toml")) {
		        log::info!("UpdatePath found");
				let lastpath = std::fs::read_to_string(Config::path("UpdatePath.toml")).unwrap_or_else(|err| {
		            log::error!(
		                "Error reading file: {:?}({})",
		                Config::path("UpdatePath.toml").to_str(),
		                err
		            );
		            String::new()
		        });
		
		        if crate::platform::is_installed() {
		            let (subkey, mut path, _start_menu, _) = crate::platform::windows::get_install_info();
		            path.push_str("\\HopToDesk.exe");
		            
		            for cmd in &[
		                ("sc", "stop HopToDesk"),
		                ("taskkill", &format!("/F /IM {:?}.exe", "HopToDesk")),
		                ("reg", &format!("add {} /f /v DisplayVersion /t REG_SZ /d \"{}\"", subkey, crate::VERSION)),
		                ("reg", &format!("add {} /f /v Version /t REG_SZ /d \"{}\"", subkey, crate::VERSION))
		            ] {
		                let _ = crate::platform::windows::run_uac_hide(cmd.0, cmd.1);
		            }
		
		            std::thread::sleep(std::time::Duration::from_secs(10));
		            
		            if let Err(err) = fs::remove_file(&path) {
		                log::error!("Failed to remove file: {}. Error: {}", path, err);
		            }
		
		            if let Err(err) = fs::copy(&exe_path, &path) {
		                log::error!("Failed to copy file to path: {}. Error: {}", path, err);
		            }
		
		            if let Err(err) = crate::platform::windows::write_send_menu(&path) {
		                log::error!("Failed to write the send menu: {}", err);
		            }
		            std::thread::sleep(std::time::Duration::from_secs(1));
		            let _ = crate::platform::windows::run_uac_hide("sc", "start HopToDesk");
		        } else {
					log::info!("Updating exe... not installed.");
					let current_pid = id();
					if let Err(err) = crate::platform::windows::run_uac_hide("taskkill", &format!("/F /IM {:?}.exe /FI \"PID ne {:?}\"", "HopToDesk", current_pid)) {
						log::error!("Failed to kill task: HopToDesk. Error: {}", err);
					} else {
						std::thread::sleep(std::time::Duration::from_secs(2));
						if let Err(err) = fs::remove_file(&lastpath) {
							log::error!("Failed to remove file: {}. Error: {}", lastpath, err);
						} else {
							log::info!("Removed old version {}", lastpath);
						}
		
						if let Err(err) = fs::copy(&exe_path, &lastpath) {
							log::error!("Failed to copy file to last path: {}. Error: {}", lastpath, err);
						} else {
							log::info!("Copied new version from {:?} to {:?}", exe_path, lastpath);
						}
						log::info!("Running new version {}", lastpath);
						let _ = crate::platform::windows::run_uac(&lastpath, "");
					}
				}
				
		        if crate::platform::is_installed() {		
		            let _ = crate::platform::windows::run_uac_hide("sc", "start HopToDesk");
		        }
		
		        std::thread::sleep(std::time::Duration::from_secs(5));
		    }
		    std::process::exit(0);
		} else if args[0] == "--install-service" {
            log::info!("start --install-service");
            crate::platform::install_service();
            return None;
        } else if args[0] == "--uninstall-service" {
            log::info!("start --uninstall-service");
            #[cfg(windows)]
            if hbb_common::config::is_tamper_protected() {
                crate::dashboard::queue_tamper_attempt("service_stop");
                log::error!("Service removal refused: tamper protection");
                std::process::exit(5);
            }
            crate::platform::uninstall_service(false, true);
            return None;
        } else if args[0] == "--service" {
            log::info!("start --service");
            #[cfg(windows)]
            if let Err(e) = crate::platform::secure_shared_config_dir() {
                log::warn!("Failed to secure the shared config dir: {}", e);
            }
            #[cfg(windows)]
            crate::platform::finish_safe_mode_restart();
            crate::start_os_service();
            return None;
        } else if args[0] == "--server" {
            log::info!("start --server with user {}", crate::username());
            #[cfg(target_os = "macos")]
            if crate::platform::is_root() && !crate::platform::sync_prelogin_identity() {
                log::info!("exit --server, another server holds the ipc socket");
                return None;
            }
            // Start dashboard WebSocket alongside the signal server
            // Beacon LAN-only: the cloud dashboard is stripped.
            if !crate::lan_only::LAN_ONLY {
                std::thread::spawn(|| crate::dashboard::start());
            }
            #[cfg(not(feature = "flutter"))]
            crate::kvm::link::start_runner();
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            {
                hbb_common::allow_err!(crate::platform::check_autostart_config());
                std::process::Command::new("pkill")
                    .arg("-f")
                    .arg(&format!("{} --tray", crate::get_app_name_lower()))
                    .status()
                    .ok();
                hbb_common::allow_err!(crate::platform::run_as_user(
                    vec!["--tray"],
                    None,
                    None::<(&str, &str)>,
                ));
            }
            #[cfg(windows)]
            crate::privacy_mode::restore_reg_connectivity(true, false);
            #[cfg(any(target_os = "linux", target_os = "freebsd", target_os = "windows"))]
            {
                crate::start_server(true, false);
            }
            #[cfg(target_os = "macos")]
            {
                let handler = std::thread::spawn(move || crate::start_server(true, false));
                crate::tray::start_tray();
                // prevent server exit when encountering errors from tray
                hbb_common::allow_err!(handler.join());
            }
            return None;
        } else if args[0] == "--password" {
            if args.len() == 2 {
                if crate::platform::is_installed() && is_root() {
                    if let Err(err) = crate::ipc::set_permanent_password(args[1].to_owned()) {
                        println!("{err}");
                    } else {
                        println!("Done!");
                    }
                } else {
                    println!("Installation and administrative privileges required!");
                }
            }
            return None;
        } else if args[0] == "--get-id" {
			println!("{}", crate::ipc::get_id());
			return None;
        } else if args[0] == "--check-hwcodec-config" {
            #[cfg(feature = "nativecodec")]
            crate::ipc::hwcodec_process();
            return None;
        } else if args[0] == "--bench-codec" {
            #[cfg(all(any(windows, target_os = "macos"), feature = "nativecodec"))]
            {
                let w = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(0);
                let h = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
                println!("{}", scrap::codec_bench::run(w, h));
            }
            return None;
        } else if args[0] == "--cm" {
            // call connection manager to establish connections
            // meanwhile, return true to call flutter window to show control panel
            crate::ui_interface::start_option_status_sync();
        } else if args[0] == "--cm-no-ui" {
            #[cfg(feature = "flutter")]
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                crate::flutter::connection_manager::start_cm_no_ui();
            }
            return None;
        } else if args[0] == "--whiteboard" {
            #[cfg(not(any(target_os = "android", target_os = "ios")))]
            {
                crate::whiteboard::run();
            }
            return None;
        } else if args[0] == "-gtk-sudo" {
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            if args.len() > 2 {
                crate::platform::gtk_sudo::exec();
            }
            return None;
        } else if args[0] == "--ticket" {
            // LAN-only build: the cloud dashboard is stripped, nothing to do.
        } else {
        }
    }
    #[cfg(feature = "flutter")]
    return Some(flutter_args);
    #[cfg(not(feature = "flutter"))]
    return Some(args);
}

pub(crate) const SERVICE_ONLY_OPTIONS: [&str; 2] = ["dashboard-device-key", "dashboard-log-token"];

fn import_config(path: &str) {
    use hbb_common::{config::*, get_exe_time, get_modified_time};
    let path2 = path.replace(".toml", "2.toml");
    let path2 = std::path::Path::new(&path2);
    let path = std::path::Path::new(path);
    log::info!("import config from {:?} and {:?}", path, path2);
    let config: Config = load_path(path.into());
    if config.is_empty() {
        log::info!("Empty source config, skipped");
        return;
    }
    if get_modified_time(&path) > get_modified_time(&Config::file())
        && get_modified_time(&path) < get_exe_time()
    {
        if store_path(Config::file(), config).is_err() {
            log::info!("config written");
        }
    }
    let mut config2: Config2 = load_path(path2.into());
    if get_modified_time(&path2) > get_modified_time(&Config2::file()) {
        for key in SERVICE_ONLY_OPTIONS {
            let current = Config::get_option(key);
            if !current.is_empty() && !config2.options.contains_key(key) {
                config2.options.insert(key.to_owned(), current);
            }
        }
        if store_path(Config2::file(), config2).is_err() {
            log::info!("config2 written");
        }
    }
}

#[cfg(all(target_os = "linux", feature = "flutter"))]
fn try_send_by_dbus(uni_links: String) -> Option<Vec<String>> {
    use crate::dbus::invoke_new_connection;

    match invoke_new_connection(uni_links) {
        Ok(()) => {
            return None;
        }
        Err(err) => {
            log::error!("{}", err.as_ref());
            return Some(Vec::new());
        }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn is_root() -> bool {
    #[cfg(windows)]
    {
        return crate::platform::is_elevated(None).unwrap_or_default()
            || crate::platform::is_root();
    }
    #[allow(unreachable_code)]
    crate::platform::is_root()
}
