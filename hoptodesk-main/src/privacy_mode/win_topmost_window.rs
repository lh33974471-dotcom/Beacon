use super::{PrivacyMode, INVALID_PRIVACY_MODE_CONN_ID};
use crate::{privacy_mode::PrivacyModeState};
use hbb_common::{allow_err, bail, log, ResultType};
use std::{
    ffi::CString,
    time::{Duration, Instant},
	env,
	path::PathBuf,
	process::Command,
};
use winapi::{
    shared::{
        minwindef::{DWORD, FALSE},
        ntdef::NULL,
        windef::HWND,
    },
    um::{
        handleapi::CloseHandle,
        processthreadsapi::{OpenProcess, TerminateProcess},
        winnt::PROCESS_TERMINATE,
        winuser::{
            FindWindowA, GetSystemMetrics, GetWindowThreadProcessId, GetWindowLongPtrW,
            SetLayeredWindowAttributes, SetWindowLongPtrW, SetWindowPos, ShowWindow,
            GWL_EXSTYLE, HWND_TOPMOST, LWA_ALPHA, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN,
            SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN, SWP_SHOWWINDOW, SW_HIDE, SW_SHOW,
            WS_EX_LAYERED, WS_EX_TRANSPARENT,
        },
    },
};

#[cfg(feature = "standalone")]
use std::{fs};

#[cfg(feature = "standalone")]
use crate::ui::get_dllpm_bytes;

pub(super) const PRIVACY_MODE_IMPL: &str = "privacy_mode_impl_mag";
pub const ORIGIN_PROCESS_EXE: &'static str = "C:\\Windows\\System32\\RuntimeBroker.exe";
pub(super) const PRIVACY_WINDOW_NAME: &'static str = "HopToDeskPrivacyWindow";

struct WindowHandlers {
    hthread: u64,
    hprocess: u64,
}

impl Drop for WindowHandlers {
    fn drop(&mut self) {
        self.reset();
    }
}

impl WindowHandlers {
    fn reset(&mut self) {
        unsafe {
            if self.hprocess != 0 {
                let _res = TerminateProcess(self.hprocess as _, 0);
                CloseHandle(self.hprocess as _);
            }
            self.hprocess = 0;
            if self.hthread != 0 {
                CloseHandle(self.hthread as _);
            }
            self.hthread = 0;
        }
    }

    fn is_default(&self) -> bool {
        self.hthread == 0 && self.hprocess == 0
    }
}

pub struct PrivacyModeImpl {
    impl_key: String,
    conn_id: i32,
    handlers: WindowHandlers,
    hwnd: u64,
}

impl PrivacyMode for PrivacyModeImpl {
    fn is_async_privacy_mode(&self) -> bool {
        false
    }
    
    fn init(&self) -> ResultType<()> {
        Ok(())
    }

    fn clear(&mut self) {
        allow_err!(self.turn_off_privacy(self.conn_id, None));
    }

    fn turn_on_privacy(&mut self, conn_id: i32) -> ResultType<bool> {
        if self.check_on_conn_id(conn_id)? {
            log::debug!("Privacy mode of conn {} is already on", conn_id);
            return Ok(true);
        }

        let exe_file = std::env::current_exe()?;
        if let Some(_cur_dir) = exe_file.parent() {
			#[cfg(not(feature = "standalone"))]
			if !_cur_dir.join("PrivacyMode.dll").exists() {
				return Ok(false);
			}
			#[cfg(feature = "standalone")]
			{
				let dll_bytes = get_dllpm_bytes();
				let dll_path = env::temp_dir().join("PrivacyMode.dll");
				if fs::metadata(&dll_path).map_or(true, |m| m.len() != dll_bytes.len() as u64) {
					if let Err(e) = fs::write(&dll_path, dll_bytes) {
						log::error!("privacy mode init failed: {}", e);
					}
				}

				if !env::temp_dir().join("PrivacyMode.dll").exists() {
					return Ok(false);
				}
			}
        } else {
            bail!(
                "Invalid exe parent for {}",
                exe_file.to_string_lossy().as_ref()
            );
        }

        if self.handlers.is_default() {
            log::info!("turn_on_privacy not running, try to start");
            self.start()?;
        }

        let mut hwnd = wait_find_privacy_hwnd(5_000)?;
        if hwnd.is_null() && crate::platform::is_installed() {
            let exe_dir = exe_file.parent().unwrap();
            let tmp = env::temp_dir();
            let fallback_dirs: Vec<(std::path::PathBuf, std::path::PathBuf)> = vec![
                (exe_dir.to_path_buf(), exe_dir.join("privacyhelper.exe")),
                (tmp.clone(), tmp.join("privacyhelper.exe")),
            ];
            for (dir, helper) in &fallback_dirs {
                if helper.exists() {
                    self.handlers.reset();
                    self.start_from_dir(&dir.to_string_lossy(), helper)?;
                    hwnd = wait_find_privacy_hwnd(5_000)?;
                    if !hwnd.is_null() {
                        break;
                    }
                }
            }
        }
        if hwnd.is_null() {
            bail!("No privacy window created");
        }
		log::info!("Privacy Window hwnd: {:?}", hwnd);
		unsafe {
			#[cfg(target_arch = "x86")]
			{
				let ex_style = GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) as u32;
				let new_ex_style = ex_style | WS_EX_LAYERED | WS_EX_TRANSPARENT;
				SetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE, new_ex_style as i32);
				SetLayeredWindowAttributes(hwnd as HWND, 0, 255, LWA_ALPHA);
			}

			#[cfg(not(target_arch = "x86"))]
			{
				let ex_style = GetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE) as isize;
				let new_ex_style = ex_style | (WS_EX_LAYERED as isize) | (WS_EX_TRANSPARENT as isize);
				SetWindowLongPtrW(hwnd as HWND, GWL_EXSTYLE, new_ex_style);
				SetLayeredWindowAttributes(hwnd as HWND, 0, 255, LWA_ALPHA);
			}
		}
        super::win_input::hook()?;
		unsafe {
			SetWindowPos(
				hwnd as HWND,
				HWND_TOPMOST,
				GetSystemMetrics(SM_XVIRTUALSCREEN),
				GetSystemMetrics(SM_YVIRTUALSCREEN),
				GetSystemMetrics(SM_CXVIRTUALSCREEN),
				GetSystemMetrics(SM_CYVIRTUALSCREEN),
				SWP_SHOWWINDOW,
			);
			ShowWindow(hwnd as _, SW_SHOW);
		}

		self.conn_id = conn_id;
        self.hwnd = hwnd as _;
        unsafe {
            let mut pid: DWORD = 0;
            GetWindowThreadProcessId(hwnd as HWND, &mut pid);
            if pid != 0 {
                let handle = OpenProcess(PROCESS_TERMINATE, FALSE, pid);
                if !handle.is_null() {
                    self.handlers.hprocess = handle as u64;
                } else {
                    log::warn!("OpenProcess failed for privacy RuntimeBroker pid {}", pid);
                }
            }
        }
        Ok(true)
    }

    fn turn_off_privacy(
        &mut self,
        conn_id: i32,
        state: Option<PrivacyModeState>,
    ) -> ResultType<()> {
        self.check_off_conn_id(conn_id)?;
        super::win_input::unhook()?;

        unsafe {
            let hwnd = wait_find_privacy_hwnd(0)?;
            if !hwnd.is_null() {
                ShowWindow(hwnd, SW_HIDE);
            }
        }

        if self.conn_id != INVALID_PRIVACY_MODE_CONN_ID {
            if let Some(state) = state {
                allow_err!(super::set_privacy_mode_state(
                    conn_id,
                    state,
                    PRIVACY_MODE_IMPL.to_string(),
                    1_000
                ));
            }
            self.conn_id = INVALID_PRIVACY_MODE_CONN_ID.to_owned();
        }
		
		self.handlers.reset();

		// Clean up the scheduled task and any remaining RuntimeBroker processes
		{
			use std::os::windows::process::CommandExt;
			let _ = Command::new("schtasks")
				.args(&["/delete", "/tn", "PrivacyHelper", "/f"])
				.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
				.output();
			let rb_name = format!("RuntimeBroker_{}.exe", crate::get_app_name().replace(" ", "").to_lowercase());
			let _ = Command::new("taskkill")
				.args(&["/f", "/im", &rb_name])
				.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
				.output();
		}

        Ok(())
    }

    #[inline]
    fn pre_conn_id(&self) -> i32 {
        self.conn_id
    }

    #[inline]
    fn get_impl_key(&self) -> &str {
        &self.impl_key
    }
}

impl PrivacyModeImpl {
    pub fn new(impl_key: &str) -> Self {
        Self {
            impl_key: impl_key.to_owned(),
            conn_id: INVALID_PRIVACY_MODE_CONN_ID,
            handlers: WindowHandlers {
                hthread: 0,
                hprocess: 0,
            },
            hwnd: 0,
        }
    }

    #[inline]
    pub fn get_hwnd(&self) -> u64 {
        self.hwnd
    }

    pub fn start(&mut self) -> ResultType<()> {
        if self.handlers.hprocess != 0 {
            return Ok(());
        }

        log::info!("Start privacy mode window broker, check_update_broker_process");
        if let Err(_e) = crate::platform::windows::check_update_broker_process() {
        }

		let program_name = "privacyhelper.exe";
		let exe_file = std::env::current_exe()?;
		let appname = crate::get_app_name().replace(" ", "").to_lowercase();

		let start_in_directory = if crate::platform::is_installed() {
			let tmp = env::temp_dir();
			if tmp.join("privacyhelper.exe").exists() {
				tmp.to_string_lossy().to_string()
			} else {
				exe_file.parent().unwrap().to_string_lossy().to_string()
			}
		} else {
			env::temp_dir().to_string_lossy().to_string()
		};

		let mut program_path = PathBuf::from(&start_in_directory).join(program_name);

		if !program_path.exists() {
			program_path = PathBuf::from(&env::temp_dir().to_string_lossy().to_string()).join(program_name);
		}

		if !program_path.exists() {
			program_path = PathBuf::from(&exe_file.parent().unwrap().to_string_lossy().to_string()).join(program_name);
		}

		if !program_path.exists() {
			let exe_path = env::current_exe()?;
			program_path = exe_path.parent().map(|p| p.join(program_name)).unwrap_or(program_path);
		}

		if let Some(helper_dir) = program_path.parent() {
			let tmp = env::temp_dir();
			let dll_name = "PrivacyMode.dll";
			let dll_in_helper_dir = helper_dir.join(dll_name);
			let dll_in_tmp = tmp.join(dll_name);
			if !dll_in_helper_dir.exists() && dll_in_tmp.exists() {
				let _ = std::fs::copy(&dll_in_tmp, &dll_in_helper_dir);
			}
			let rb_name = format!("RuntimeBroker_{}.exe", &appname);
			let rb_in_helper_dir = helper_dir.join(&rb_name);
			let rb_in_tmp = tmp.join(&rb_name);
			if !rb_in_helper_dir.exists() && rb_in_tmp.exists() {
				let _ = std::fs::copy(&rb_in_tmp, &rb_in_helper_dir);
			}
		}

		self.start_from_dir(&start_in_directory, &program_path)
    }

    fn start_from_dir(&mut self, start_in_directory: &str, program_path: &PathBuf) -> ResultType<()> {
		let appname = crate::get_app_name().replace(" ", "").to_lowercase();
		let dll_name = "PrivacyMode";
		use std::os::windows::process::CommandExt;
		match Command::new(&program_path)
			.arg(&appname)
			.arg(dll_name)
			.current_dir(&start_in_directory)
			.creation_flags(winapi::um::winbase::CREATE_NO_WINDOW)
			.spawn()
		{
			Ok(child) => {
				log::info!("Privacy Helper started with PID: {:?}", child.id());
			}
			Err(e) => {
				let error_msg = match e.raw_os_error() {
					Some(216) => format!("Architecture mismatch - Privacy Mode not compatible with this {}-bit application",
						if cfg!(target_pointer_width = "64") { "64" } else { "32" }),
					_ => format!("{}", e)
				};
				log::error!("Error executing Privacy Helper: {:?}", error_msg);
				bail!("Failed to start Privacy Helper: {}", error_msg);
			}
		}

		let task_name = "PrivacyHelper";
		let cmd = format!(
			r#"/create /tn "{}" /tr "{} {} {}" /sc once /st 00:05 /f"#,
			task_name,
			program_path.to_string_lossy(),
			appname,
			dll_name
		);

		let args = vec![cmd.as_str()];
		let _ = crate::platform::run_task_user(args.clone());

		let cmdruntask = format!(r#"/run /tn {}"#, task_name);
		let argsrun = vec![cmdruntask.as_str()];
		let _ = crate::platform::run_task_user(argsrun.clone());

        Ok(())
    }

    #[inline]
    pub fn stop(&mut self) {
        self.handlers.reset();
    }
}

impl Drop for PrivacyModeImpl {
    fn drop(&mut self) {
        if self.conn_id != INVALID_PRIVACY_MODE_CONN_ID {
            allow_err!(self.turn_off_privacy(self.conn_id, None));
        }
    }
}
pub(super) fn wait_find_privacy_hwnd(msecs: u128) -> ResultType<HWND> {
    crate::platform::try_change_desktop();

    let tm_begin = Instant::now();
    let wndname = CString::new(PRIVACY_WINDOW_NAME)?;
    loop {
        unsafe {
			let hwnd = FindWindowA(NULL as _, wndname.as_ptr() as _);
            if !hwnd.is_null() {
				return Ok(hwnd);
            }
        }

        if msecs == 0 || tm_begin.elapsed().as_millis() > msecs {
			return Ok(NULL as _);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}


