use super::*;
use hbb_common::{
    anyhow::{anyhow, Context, Result},
    compress,
};
use portable_pty::Child;
#[cfg(not(target_os = "windows"))]
use portable_pty::{CommandBuilder, PtySize};
use std::{
    collections::{HashMap, VecDeque},
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

const MAX_OUTPUT_BUFFER_SIZE: usize = 1024 * 1024; // 1MB per terminal
const MAX_BUFFER_LINES: usize = 10000;
const MAX_SERVICES: usize = 100; // Maximum number of persistent terminal services
const SERVICE_IDLE_TIMEOUT: Duration = Duration::from_secs(3600); // 1 hour idle timeout
const CHANNEL_BUFFER_SIZE: usize = 100; // Number of messages to buffer in channel
const COMPRESS_THRESHOLD: usize = 512; // Compress terminal data larger than this
#[cfg(target_os = "windows")]
const HOST_CTRL_HANDLER_WAIT: Duration = Duration::from_secs(2);

#[cfg(target_os = "windows")]
static HOST_CTRL_EVENT_HANDLED: AtomicBool = AtomicBool::new(false);

lazy_static::lazy_static! {
    // Global registry of persistent terminal services indexed by service_id
    static ref TERMINAL_SERVICES: Arc<Mutex<HashMap<String, Arc<Mutex<PersistentTerminalService>>>>> =
        Arc::new(Mutex::new(HashMap::new()));

    // Cleanup task handle
    static ref CLEANUP_TASK: Arc<Mutex<Option<std::thread::JoinHandle<()>>>> = Arc::new(Mutex::new(None));

    // List of terminal child processes to check for zombies
    static ref TERMINAL_TASKS: Arc<Mutex<Vec<Box<dyn Child + Send + Sync>>>> = Arc::new(Mutex::new(Vec::new()));
}

#[cfg(target_os = "windows")]
#[derive(PartialEq, Eq)]
enum EscState {
    Normal,
    Esc,
    Csi,
}

/// Generate a new persistent service ID
pub fn generate_service_id() -> String {
    format!("ts_{}", uuid::Uuid::new_v4())
}

fn get_default_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        // Try PowerShell Core first (cross-platform version)
        // Common installation paths for PowerShell Core
        let pwsh_paths = [
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            r"C:\Program Files\PowerShell\6\pwsh.exe",
        ];

        for path in &pwsh_paths {
            if std::path::Path::new(path).exists() {
                return path.to_string();
            }
        }

        let powershell_path = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
        if crate::platform::is_win_10_or_greater() && std::path::Path::new(powershell_path).exists() {
            return powershell_path.to_string();
        }

        // Final fallback to cmd.exe
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(target_os = "windows"))]
    {
        // First try the SHELL environment variable
        if let Ok(shell) = std::env::var("SHELL") {
            if !shell.is_empty() {
                return shell;
            }
        }

        // Check for common shells in order of preference
        let shells = ["/bin/bash", "/bin/zsh", "/bin/sh"];
        for shell in &shells {
            if std::path::Path::new(shell).exists() {
                return shell.to_string();
            }
        }

        // Final fallback to /bin/sh which should exist on all POSIX systems
        "/bin/sh".to_string()
    }
}

/// Get or create a persistent terminal service
pub fn get_or_create_service(
    service_id: String,
    is_persistent: bool,
) -> Result<Arc<Mutex<PersistentTerminalService>>> {
    let mut services = TERMINAL_SERVICES.lock().unwrap();

    // Check service limit
    if !services.contains_key(&service_id) && services.len() >= MAX_SERVICES {
        return Err(anyhow!(
            "Maximum number of terminal services ({}) reached",
            MAX_SERVICES
        ));
    }

    let service = services
        .entry(service_id.clone())
        .or_insert_with(|| {
            log::info!(
                "Creating new terminal service: {} (persistent: {})",
                service_id,
                is_persistent
            );
            Arc::new(Mutex::new(PersistentTerminalService::new(
                service_id.clone(),
                is_persistent,
            )))
        })
        .clone();

    // Ensure cleanup task is running
    ensure_cleanup_task();

    service.lock().unwrap().reset_status(is_persistent);

    Ok(service)
}

/// Remove a service from the global registry
fn remove_service(service_id: &str) {
    let mut services = TERMINAL_SERVICES.lock().unwrap();
    if let Some(service) = services.remove(service_id) {
        log::info!("Removed service: {}", service_id);
        // Close all terminals in the service
        let sessions = service.lock().unwrap().sessions.clone();
        for (_, session) in sessions.iter() {
            let mut session = session.lock().unwrap();
            session.stop();
        }
    }
}

/// Get service by ID
pub fn get_service(service_id: &str) -> Option<Arc<Mutex<PersistentTerminalService>>> {
    let services = TERMINAL_SERVICES.lock().unwrap();
    services.get(service_id).cloned()
}

/// Clean up inactive services
pub fn cleanup_inactive_services() {
    let services = TERMINAL_SERVICES.lock().unwrap();
    let now = Instant::now();
    let mut to_remove = Vec::new();

    for (service_id, service) in services.iter() {
        if let Ok(svc) = service.lock() {
            // Remove non-persistent services after idle timeout
            if !svc.is_persistent && now.duration_since(svc.last_activity) > SERVICE_IDLE_TIMEOUT {
                to_remove.push(service_id.clone());
                log::info!("Cleaning up idle non-persistent service: {}", service_id);
            }
            // Remove persistent services with no active terminals after longer timeout
            else if svc.is_persistent
                && svc.sessions.is_empty()
                && now.duration_since(svc.last_activity) > SERVICE_IDLE_TIMEOUT * 2
            {
                to_remove.push(service_id.clone());
                log::info!("Cleaning up empty persistent service: {}", service_id);
            }
        }
    }

    // Remove outside of iteration to avoid deadlock
    drop(services);
    for id in to_remove {
        remove_service(&id);
    }
}

/// Add a child process to the zombie reaper
fn add_to_reaper(child: Box<dyn Child + Send + Sync>) {
    if let Ok(mut tasks) = TERMINAL_TASKS.lock() {
        tasks.push(child);
    }
}

/// Check and reap zombie terminal processes
fn check_zombie_terminals() {
    let mut tasks = match TERMINAL_TASKS.lock() {
        Ok(t) => t,
        Err(_) => return,
    };

    let mut i = 0;
    while i < tasks.len() {
        match tasks[i].try_wait() {
            Ok(Some(_)) => {
                // Process has exited, remove it
                log::info!("Process exited: {:?}", tasks[i].process_id());
                tasks.remove(i);
            }
            Ok(None) => {
                // Still running
                i += 1;
            }
            Err(err) => {
                // Error checking status, remove it
                log::info!(
                    "Process exited with error: {:?}, err: {err}",
                    tasks[i].process_id()
                );
                tasks.remove(i);
            }
        }
    }
}

/// Ensure the cleanup task is running
fn ensure_cleanup_task() {
    let mut task_handle = CLEANUP_TASK.lock().unwrap();
    if task_handle.is_none() {
        let handle = std::thread::spawn(|| {
            log::info!("Started cleanup task");
            let mut last_service_cleanup = Instant::now();
            loop {
                // Check for zombie processes every 100ms
                check_zombie_terminals();

                // Check for inactive services every 5 minutes
                if last_service_cleanup.elapsed() > Duration::from_secs(300) {
                    cleanup_inactive_services();
                    last_service_cleanup = Instant::now();
                }

                std::thread::sleep(Duration::from_millis(100));
            }
        });
        *task_handle = Some(handle);
    }
}

pub type UserToken = u64;

/// Output buffer for terminal session
struct OutputBuffer {
    lines: VecDeque<Vec<u8>>,
    total_size: usize,
    last_line_incomplete: bool,
}

impl OutputBuffer {
    fn new() -> Self {
        Self {
            lines: VecDeque::new(),
            total_size: 0,
            last_line_incomplete: false,
        }
    }

    fn append(&mut self, data: &[u8]) {
        if data.is_empty() {
            return;
        }

        // Handle incomplete lines
        let mut start = 0;
        if self.last_line_incomplete {
            if let Some(last_line) = self.lines.back_mut() {
                // Find first newline in new data
                if let Some(newline_pos) = data.iter().position(|&b| b == b'\n') {
                    last_line.extend_from_slice(&data[..=newline_pos]);
                    self.total_size += newline_pos + 1;
                    start = newline_pos + 1;
                    self.last_line_incomplete = false;
                } else {
                    // Still no newline, append all
                    last_line.extend_from_slice(data);
                    self.total_size += data.len();
                    start = data.len();
                }
            }
        }

        // Process remaining data
        let remaining = &data[start..];
        let ends_with_newline = remaining.last() == Some(&b'\n');

        // Split by lines
        let lines: Vec<&[u8]> = remaining.split(|&b| b == b'\n').collect();

        for (i, line) in lines.iter().enumerate() {
            if i == lines.len() - 1 && !ends_with_newline && !line.is_empty() {
                // Last line without newline
                self.last_line_incomplete = true;
            }

            if !line.is_empty() || i < lines.len() - 1 {
                let mut line_data = line.to_vec();
                if i < lines.len() - 1 || ends_with_newline {
                    line_data.push(b'\n');
                }

                self.total_size += line_data.len();
                self.lines.push_back(line_data);
            }
        }

        // Trim old data if buffer is too large
        while self.total_size > MAX_OUTPUT_BUFFER_SIZE || self.lines.len() > MAX_BUFFER_LINES {
            if self.lines.len() == 1 && self.last_line_incomplete {
                let line = &mut self.lines[0];
                line.drain(..line.len().saturating_sub(MAX_OUTPUT_BUFFER_SIZE / 2));
                self.total_size = line.len();
                break;
            }
            if let Some(removed) = self.lines.pop_front() {
                self.total_size -= removed.len();
            }
        }
    }

    fn get_recent(&self, max_bytes: usize) -> Vec<u8> {
        if max_bytes == 0 {
            return Vec::new();
        }
        let mut chunks: Vec<&[u8]> = Vec::new();
        let mut size = 0;
        for line in self.lines.iter().rev() {
            if size + line.len() > max_bytes {
                if size == 0 && line.len() > max_bytes {
                    let mut offset = line.len() - max_bytes;
                    let mut skipped = 0u8;
                    while skipped < 3
                        && offset < line.len()
                        && (line[offset] & 0b1100_0000) == 0b1000_0000
                    {
                        offset += 1;
                        skipped += 1;
                    }
                    if offset < line.len() {
                        chunks.push(&line[offset..]);
                        size = line.len() - offset;
                    }
                }
                break;
            }
            size += line.len();
            chunks.push(line);
        }
        chunks.reverse();
        let mut result = Vec::with_capacity(size);
        for chunk in chunks {
            result.extend_from_slice(chunk);
        }
        result
    }
}

pub struct TerminalSession {
    pub created_at: Instant,
    last_activity: Instant,
    pty_pair: Option<portable_pty::PtyPair>,
    child: Option<Box<dyn Child + std::marker::Send + Sync>>,
    // Channel for sending input to the writer thread
    input_tx: Option<SyncSender<Vec<u8>>>,
    // Channel for receiving output from the reader thread
    output_rx: Option<Receiver<Vec<u8>>>,
    #[cfg(target_os = "windows")]
    output_tx: Option<SyncSender<Vec<u8>>>,
    exiting: Arc<AtomicBool>,
    // Thread handles
    reader_thread: Option<thread::JoinHandle<()>>,
    writer_thread: Option<thread::JoinHandle<()>>,
    output_buffer: OutputBuffer,
    pid: u32,
    rows: u16,
    cols: u16,
    // Track if we've already sent the closed message
    closed_message_sent: bool,
    is_opened: bool,
    command_buffer: String,
    #[cfg(target_os = "windows")]
    line_buffer: String,
    #[cfg(target_os = "windows")]
    esc_state: EscState,
    #[cfg(target_os = "windows")]
    shell_process: Option<std::process::Child>,
}

impl TerminalSession {
    fn new(rows: u16, cols: u16) -> Self {
        Self {
            created_at: Instant::now(),
            last_activity: Instant::now(),
            pty_pair: None,
            child: None,
            input_tx: None,
            output_rx: None,
            #[cfg(target_os = "windows")]
            output_tx: None,
            exiting: Arc::new(AtomicBool::new(false)),
            reader_thread: None,
            writer_thread: None,
            output_buffer: OutputBuffer::new(),
            pid: 0,
            rows,
            cols,
            closed_message_sent: false,
            is_opened: false,
            command_buffer: String::new(),
            #[cfg(target_os = "windows")]
            line_buffer: String::new(),
            #[cfg(target_os = "windows")]
            esc_state: EscState::Normal,
            #[cfg(target_os = "windows")]
            shell_process: None,
        }
    }

    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    fn has_live_shell(&mut self) -> bool {
        #[cfg(target_os = "windows")]
        return self
            .shell_process
            .as_mut()
            .map_or(false, |shell| !matches!(shell.try_wait(), Ok(Some(_))));
        #[cfg(not(target_os = "windows"))]
        self.child.is_some()
    }

    // This helper function is to ensure that the threads are joined before the child process is dropped.
    // Though this is not strictly necessary on macOS.
    fn stop(&mut self) {
        self.is_opened = false;
        self.exiting.store(true, Ordering::SeqCst);

        // Drop the input channel to signal writer thread to exit
        if let Some(input_tx) = self.input_tx.take() {
            // Send a final newline to ensure the reader can read some data, and then exit.
            // This is required on Windows and Linux.
            // Although `self.pty_pair = None;` is called below, we can still send a final newline here.
            if let Err(e) = input_tx.send(b"\r\n".to_vec()) {
                log::warn!("Failed to send final newline to the terminal: {}", e);
            }
            drop(input_tx);
        }
        self.output_rx = None;
        #[cfg(target_os = "windows")]
        { self.output_tx = None; }

        #[cfg(any(target_os = "windows", target_os = "linux", target_os = "freebsd"))]
        {
            self.pty_pair = None;
        }

        #[cfg(target_os = "windows")]
        if let Some(mut shell) = self.shell_process.take() {
            if !matches!(shell.try_wait(), Ok(Some(_))) {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(&["/T", "/F", "/PID", &shell.id().to_string()])
                    .creation_flags(0x08000000)
                    .output();
            }
        }

        #[cfg(target_os = "windows")]
        {
            let _ = self.reader_thread.take();
            let _ = self.writer_thread.take();
        }

        #[cfg(not(target_os = "windows"))]
        {
            if let Some(reader_thread) = self.reader_thread.take() {
                let _ = reader_thread.join();
            }
            if let Some(writer_thread) = self.writer_thread.take() {
                let _ = writer_thread.join();
            }
        }

        #[allow(unused_mut)]
        if let Some(mut child) = self.child.take() {
            // Kill entire process tree on Windows
            #[cfg(target_os = "windows")]
            {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(&["/T", "/F", "/PID", &self.pid.to_string()])
                    .creation_flags(0x08000000)
                    .output();
            }
            #[cfg(not(target_os = "windows"))]
            {
                let _ = child.kill();
            }
            add_to_reaper(child);
        }
    }
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        // Ensure child process is properly handled when session is dropped
        self.stop();
    }
}

/// Persistent terminal service that can survive connection drops
pub struct PersistentTerminalService {
    service_id: String,
    sessions: HashMap<i32, Arc<Mutex<TerminalSession>>>,
    pub created_at: Instant,
    last_activity: Instant,
    pub is_persistent: bool,
    needs_session_sync: bool,
}

impl PersistentTerminalService {
    pub fn new(service_id: String, is_persistent: bool) -> Self {
        Self {
            service_id,
            sessions: HashMap::new(),
            created_at: Instant::now(),
            last_activity: Instant::now(),
            is_persistent,
            needs_session_sync: false,
        }
    }

    fn update_activity(&mut self) {
        self.last_activity = Instant::now();
    }

    fn reset_status(&mut self, is_persistent: bool) {
        self.is_persistent = is_persistent;
        self.needs_session_sync = true;
        for session in self.sessions.values() {
            let mut session = session.lock().unwrap();
            session.is_opened = false;
        }
    }
}

pub struct TerminalServiceProxy {
    service_id: String,
    is_persistent: bool,
    #[cfg(target_os = "windows")]
    #[allow(dead_code)] // Will be used when portable-pty supports set_user_token
    user_token: Option<UserToken>,
}

impl TerminalServiceProxy {
    pub fn new(
        service_id: String,
        is_persistent: Option<bool>,
        _user_token: Option<UserToken>,
    ) -> Self {
        // Get persistence from the service if it exists
        let is_persistent =
            is_persistent.unwrap_or(if let Some(service) = get_service(&service_id) {
                service.lock().unwrap().is_persistent
            } else {
                false
            });
        TerminalServiceProxy {
            service_id,
            is_persistent,
            #[cfg(target_os = "windows")]
            user_token: _user_token,
        }
    }

    pub fn handle_action(&mut self, action: &TerminalAction) -> Result<Option<TerminalResponse>> {
        let service = match get_service(&self.service_id) {
            Some(s) => s,
            None => {
                let mut response = TerminalResponse::new();
                let mut error = TerminalError::new();
                error.message = format!("Terminal service {} not found", self.service_id);
                response.set_error(error);
                return Ok(Some(response));
            }
        };
        service.lock().unwrap().update_activity();
        match &action.union {
            Some(terminal_action::Union::Open(open)) => {
                self.handle_open(&mut service.lock().unwrap(), open)
            }
            Some(terminal_action::Union::Resize(resize)) => {
                let session = service
                    .lock()
                    .unwrap()
                    .sessions
                    .get(&resize.terminal_id)
                    .cloned();
                self.handle_resize(session, resize)
            }
            Some(terminal_action::Union::Data(data)) => {
                let session = service
                    .lock()
                    .unwrap()
                    .sessions
                    .get(&data.terminal_id)
                    .cloned();
                self.handle_data(session, data)
            }
            Some(terminal_action::Union::Close(close)) => {
                self.handle_close(&mut service.lock().unwrap(), close)
            }
            _ => Ok(None),
        }
    }

    fn handle_open(
        &self,
        service: &mut PersistentTerminalService,
        open: &OpenTerminal,
    ) -> Result<Option<TerminalResponse>> {
        let mut response = TerminalResponse::new();

        // Check if terminal already exists
        if let Some(session_arc) = service.sessions.get(&open.terminal_id) {
            // Check if the session's process is still alive
            let is_dead = {
                let mut session = session_arc.lock().unwrap();
                !session.has_live_shell() || session.closed_message_sent
            };
            if is_dead {
                // Remove dead session so we create a fresh one below
                service.sessions.remove(&open.terminal_id);
            }
        }
        if let Some(session_arc) = service.sessions.get(&open.terminal_id) {
            // Reconnect to existing terminal
            let mut session = session_arc.lock().unwrap();
            session.is_opened = true;
            #[cfg(target_os = "windows")]
            {
                session.line_buffer.clear();
                session.esc_state = EscState::Normal;
            }
            let mut opened = TerminalOpened::new();
            opened.terminal_id = open.terminal_id;
            opened.success = true;
            opened.message = "Reconnected to existing terminal".to_string();
            opened.pid = session.pid;
            opened.service_id = self.service_id.clone();
            if service.needs_session_sync {
                if service.sessions.len() > 1 {
                    opened.persistent_sessions = service
                        .sessions
                        .keys()
                        .filter(|&id| *id != open.terminal_id)
                        .cloned()
                        .collect();
                }
                service.needs_session_sync = false;
            }
            response.set_opened(opened);

            // Send buffered output
            let buffer = session.output_buffer.get_recent(4096);
            if !buffer.is_empty() {
                // We'll need to send this separately or extend the protocol
                // For now, just acknowledge the reconnection
            }

            return Ok(Some(response));
        }

        // Create new terminal session
        log::info!(
            "Creating new terminal {} for service: {}",
            open.terminal_id,
            service.service_id
        );
        let mut session =
            TerminalSession::new(open.rows as u16, open.cols as u16);

        // Use default shell for the platform
        let shell = get_default_shell();
        log::debug!("Using shell: {}", shell);

        // Create channels for input/output
        let (input_tx, input_rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_BUFFER_SIZE);
        let (output_tx, output_rx) = mpsc::sync_channel::<Vec<u8>>(CHANNEL_BUFFER_SIZE);

        // On Windows, ConPTY crashes when running as a service (Session 0).
        // Use piped process directly. On other platforms, use PTY for full terminal support.
        #[cfg(not(target_os = "windows"))]
        {
            let pty_size = PtySize {
                rows: open.rows as u16,
                cols: open.cols as u16,
                pixel_width: 0,
                pixel_height: 0,
            };
            log::debug!("Opening PTY with size: {}x{}", open.rows, open.cols);
            let pty_system = portable_pty::native_pty_system();
            let pty_pair = pty_system.openpty(pty_size).context("Failed to open PTY")?;
            let mut cmd = CommandBuilder::new(&shell);
            cmd.env("TERM", "xterm-256color");
            log::debug!("Spawning shell process via PTY...");
            let child = pty_pair.slave.spawn_command(cmd).context("Failed to spawn command")?;

            let writer = pty_pair
                .master
                .take_writer()
                .context("Failed to get writer")?;

            let reader = pty_pair
                .master
                .try_clone_reader()
                .context("Failed to get reader")?;

            session.pid = child.process_id().unwrap_or(0) as u32;

            let terminal_id = open.terminal_id;
            let writer_thread = thread::spawn(move || {
                let mut writer = writer;
                while let Ok(data) = input_rx.recv() {
                    if let Err(e) = writer.write_all(&data) {
                        log::error!("Terminal {} write error: {}", terminal_id, e);
                        break;
                    }
                    let _ = writer.flush();
                }
                log::debug!("Terminal {} writer thread exiting", terminal_id);
            });

            let exiting = session.exiting.clone();
            let terminal_id = open.terminal_id;
            let reader_thread = thread::spawn(move || {
                let mut reader = reader;
                let mut buf = vec![0u8; 4096];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if exiting.load(Ordering::SeqCst) {
                                break;
                            }
                            match output_tx.try_send(buf[..n].to_vec()) {
                                Ok(_) => {}
                                Err(mpsc::TrySendError::Full(_)) => {}
                                Err(mpsc::TrySendError::Disconnected(_)) => break,
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            if exiting.load(Ordering::SeqCst) {
                                break;
                            }
                            thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => {
                            log::error!("Terminal {} read error: {}", terminal_id, e);
                            break;
                        }
                    }
                }
                log::debug!("Terminal {} reader thread exiting", terminal_id);
            });

            session.pty_pair = Some(pty_pair);
            session.child = Some(child);
            session.reader_thread = Some(reader_thread);
            session.writer_thread = Some(writer_thread);
        }

        // Windows: use piped process. CREATE_NO_WINDOW gives the child its own console
        // with no window, so GenerateConsoleCtrlEvent can send Ctrl+C to it.
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;

            log::info!("Spawning piped shell process for terminal {}", open.terminal_id);
            let mut child_proc = std::process::Command::new(&shell)
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .creation_flags(CREATE_NO_WINDOW)
                .spawn()
                .context("Failed to spawn shell process")?;

            session.pid = child_proc.id();

            let stdin = child_proc.stdin.take().context("Failed to get stdin")?;
            let stdout = child_proc.stdout.take().context("Failed to get stdout")?;
            let stderr = child_proc.stderr.take().context("Failed to get stderr")?;
            session.shell_process = Some(child_proc);

            let terminal_id = open.terminal_id;
            let writer_thread = thread::spawn(move || {
                let mut stdin = stdin;
                while let Ok(data) = input_rx.recv() {
                    if let Err(e) = stdin.write_all(&data) {
                        log::error!("Terminal {} stdin write error: {}", terminal_id, e);
                        break;
                    }
                    let _ = stdin.flush();
                }
                log::debug!("Terminal {} writer thread exiting", terminal_id);
            });

            let exiting = session.exiting.clone();
            let output_tx2 = output_tx.clone();
            let output_tx_echo = output_tx.clone();
            let terminal_id = open.terminal_id;
            let reader_thread = thread::spawn(move || {
                let mut stdout = stdout;
                let mut buf = vec![0u8; 4096];
                loop {
                    match stdout.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if exiting.load(Ordering::SeqCst) {
                                break;
                            }
                            match output_tx.try_send(buf[..n].to_vec()) {
                                Ok(_) => {}
                                Err(mpsc::TrySendError::Full(_)) => {}
                                Err(mpsc::TrySendError::Disconnected(_)) => break,
                            }
                        }
                        Err(e) => {
                            log::error!("Terminal {} stdout read error: {}", terminal_id, e);
                            break;
                        }
                    }
                }
                log::debug!("Terminal {} stdout reader thread exiting", terminal_id);
            });

            let exiting2 = session.exiting.clone();
            let terminal_id = open.terminal_id;
            thread::spawn(move || {
                let mut stderr = stderr;
                let mut buf = vec![0u8; 4096];
                loop {
                    match stderr.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            if exiting2.load(Ordering::SeqCst) {
                                break;
                            }
                            let _ = output_tx2.try_send(buf[..n].to_vec());
                        }
                        Err(_) => break,
                    }
                }
                log::debug!("Terminal {} stderr reader thread exiting", terminal_id);
            });

            session.reader_thread = Some(reader_thread);
            session.writer_thread = Some(writer_thread);
            session.output_tx = Some(output_tx_echo);
        }

        session.input_tx = Some(input_tx);
        session.output_rx = Some(output_rx);
        session.is_opened = true;

        let mut opened = TerminalOpened::new();
        opened.terminal_id = open.terminal_id;
        opened.success = true;
        opened.message = "Terminal opened".to_string();
        opened.pid = session.pid;
        opened.service_id = service.service_id.clone();
        if service.needs_session_sync {
            if !service.sessions.is_empty() {
                opened.persistent_sessions = service.sessions.keys().cloned().collect();
            }
            service.needs_session_sync = false;
        }
        response.set_opened(opened);

        log::info!(
            "Terminal {} opened successfully with PID {}",
            open.terminal_id,
            session.pid
        );

        // Store the session
        service
            .sessions
            .insert(open.terminal_id, Arc::new(Mutex::new(session)));

        Ok(Some(response))
    }

    fn handle_resize(
        &self,
        session: Option<Arc<Mutex<TerminalSession>>>,
        resize: &ResizeTerminal,
    ) -> Result<Option<TerminalResponse>> {
        if let Some(session_arc) = session {
            let mut session = session_arc.lock().unwrap();
            session.update_activity();
            session.rows = resize.rows as u16;
            session.cols = resize.cols as u16;

            #[cfg(not(target_os = "windows"))]
            if let Some(pty_pair) = &session.pty_pair {
                pty_pair.master.resize(PtySize {
                    rows: resize.rows as u16,
                    cols: resize.cols as u16,
                    pixel_width: 0,
                    pixel_height: 0,
                })?;
            }
        }
        Ok(None)
    }

    #[cfg(target_os = "windows")]
    fn with_console_detached<F: FnOnce()>(f: F) {
        extern "system" {
            fn GetStdHandle(nStdHandle: u32) -> *mut std::ffi::c_void;
            fn SetStdHandle(nStdHandle: u32, handle: *mut std::ffi::c_void) -> i32;
        }
        const STD_INPUT_HANDLE: u32 = -10i32 as u32;
        const STD_OUTPUT_HANDLE: u32 = -11i32 as u32;
        const STD_ERROR_HANDLE: u32 = -12i32 as u32;
        static CONSOLE_SWAP: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _console_swap = CONSOLE_SWAP.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            let saved = [
                (STD_INPUT_HANDLE, GetStdHandle(STD_INPUT_HANDLE)),
                (STD_OUTPUT_HANDLE, GetStdHandle(STD_OUTPUT_HANDLE)),
                (STD_ERROR_HANDLE, GetStdHandle(STD_ERROR_HANDLE)),
            ];
            f();
            for (which, handle) in saved {
                SetStdHandle(which, handle);
            }
        }
    }

    /// Send Ctrl+C to a Windows process by attaching to its console
    #[cfg(target_os = "windows")]
    fn send_ctrl_c(pid: u32) {
        extern "system" {
            fn FreeConsole() -> i32;
            fn AttachConsole(dwProcessId: u32) -> i32;
            fn GenerateConsoleCtrlEvent(dwCtrlEvent: u32, dwProcessGroupId: u32) -> i32;
            fn SetConsoleCtrlHandler(
                HandlerRoutine: Option<unsafe extern "system" fn(u32) -> i32>,
                Add: i32,
            ) -> i32;
        }
        const CTRL_C_EVENT: u32 = 0;
        Self::with_console_detached(|| unsafe {
            FreeConsole();
            if AttachConsole(pid) != 0 {
                SetConsoleCtrlHandler(Some(Self::host_console_ctrl_handler), 1);
                HOST_CTRL_EVENT_HANDLED.store(false, Ordering::SeqCst);
                if GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0) != 0 {
                    let deadline = Instant::now() + HOST_CTRL_HANDLER_WAIT;
                    while !HOST_CTRL_EVENT_HANDLED.load(Ordering::SeqCst) && Instant::now() < deadline {
                        thread::sleep(Duration::from_millis(2));
                    }
                }
                FreeConsole();
            }
        });
    }

    #[cfg(target_os = "windows")]
    unsafe extern "system" fn host_console_ctrl_handler(ctrl_type: u32) -> i32 {
        const CTRL_C_EVENT: u32 = 0;
        const CTRL_BREAK_EVENT: u32 = 1;
        if ctrl_type == CTRL_C_EVENT || ctrl_type == CTRL_BREAK_EVENT {
            HOST_CTRL_EVENT_HANDLED.store(true, Ordering::SeqCst);
            1
        } else {
            0
        }
    }

    fn get_fullscreen_command_hint(cmd: &str) -> Option<String> {
        let cmd_name = cmd.trim().split_whitespace().next().unwrap_or("");
        let base = cmd_name.rsplit('/').next().unwrap_or(cmd_name);
        match base {
            "top" => Some(
                "\r\n\x1b[33m[Hint: If 'top' doesn't display correctly, try 'top -b' for scrolling output or 'top -b -n 1' for a snapshot.]\x1b[0m\r\n"
                    .to_string(),
            ),
            "htop" => Some(
                "\r\n\x1b[33m[Hint: If 'htop' doesn't display correctly, try 'top -b' or 'ps aux --sort=-%cpu | head -20'.]\x1b[0m\r\n"
                    .to_string(),
            ),
            _ => None,
        }
    }

    fn handle_data(
        &self,
        session: Option<Arc<Mutex<TerminalSession>>>,
        data: &TerminalData,
    ) -> Result<Option<TerminalResponse>> {
        let mut hint_response: Option<TerminalResponse> = None;

        if let Some(session_arc) = session {
            let mut session = session_arc.lock().unwrap();
            session.update_activity();

            let input_str = String::from_utf8_lossy(&data.data);

            // On Windows piped mode, do local line editing since the pipe doesn't handle it
            #[cfg(target_os = "windows")]
            {
                let mut echo = String::new();
                for ch in input_str.chars() {
                    if session.esc_state == EscState::Esc {
                        session.esc_state = if ch == '[' { EscState::Csi } else { EscState::Normal };
                        continue;
                    }
                    if session.esc_state == EscState::Csi {
                        if (0x40..=0x7E).contains(&(ch as u32)) {
                            session.esc_state = EscState::Normal;
                        }
                        continue;
                    }
                    if ch == '\x1b' {
                        session.esc_state = EscState::Esc;
                        continue;
                    }
                    if ch == '\r' || ch == '\n' {
                        // Send the buffered line + newline to the process
                        let line = std::mem::take(&mut session.line_buffer);
                        echo.push_str("\r\n");
                        if let Some(input_tx) = &session.input_tx {
                            let mut to_send = line.clone();
                            to_send.push_str("\r\n");
                            let _ = input_tx.send(to_send.into_bytes());
                        }
                        // Check for fullscreen command hints
                        session.command_buffer = line;
                        if let Some(hint) = Self::get_fullscreen_command_hint(&session.command_buffer) {
                            let mut response = TerminalResponse::new();
                            let mut terminal_data = TerminalData::new();
                            terminal_data.terminal_id = data.terminal_id;
                            terminal_data.data = bytes::Bytes::from(hint);
                            response.set_data(terminal_data);
                            hint_response = Some(response);
                        }
                        session.command_buffer.clear();
                    } else if ch == '\x7f' || ch == '\x08' {
                        // Backspace — remove last char from line buffer
                        if session.line_buffer.pop().is_some() {
                            // Echo: move cursor back, overwrite with space, move back again
                            echo.push_str("\x08 \x08");
                        }
                    } else if ch == '\x15' {
                        // Ctrl+U — clear line
                        let len = session.line_buffer.len();
                        session.line_buffer.clear();
                        for _ in 0..len {
                            echo.push_str("\x08 \x08");
                        }
                    } else if ch == '\x03' {
                        // Ctrl+C — send real console ctrl event to interrupt running command
                        session.line_buffer.clear();
                        Self::send_ctrl_c(session.pid);
                    } else if ch == '\t' {
                    } else if ch >= ' ' {
                        session.line_buffer.push(ch);
                        echo.push(ch);
                    }
                }
                // Echo typed characters back to the terminal display
                if !echo.is_empty() {
                    if let Some(output_tx) = &session.output_tx {
                        let _ = output_tx.try_send(echo.into_bytes());
                    }
                }
            }

            // On Unix with PTY, send raw data — the PTY handles line editing and echo
            #[cfg(not(target_os = "windows"))]
            {
                // Buffer keystrokes to detect command lines
                for ch in input_str.chars() {
                    if ch == '\r' || ch == '\n' {
                        if let Some(hint) = Self::get_fullscreen_command_hint(&session.command_buffer) {
                            let mut response = TerminalResponse::new();
                            let mut terminal_data = TerminalData::new();
                            terminal_data.terminal_id = data.terminal_id;
                            terminal_data.data = bytes::Bytes::from(hint);
                            response.set_data(terminal_data);
                            hint_response = Some(response);
                        }
                        session.command_buffer.clear();
                    } else if ch == '\x7f' || ch == '\x08' {
                        session.command_buffer.pop();
                    } else if ch == '\x15' {
                        session.command_buffer.clear();
                    } else if ch == '\x17' {
                        let pos = session.command_buffer.trim_end().rfind(' ');
                        if let Some(pos) = pos {
                            session.command_buffer.truncate(pos);
                        } else {
                            session.command_buffer.clear();
                        }
                    } else if ch >= ' ' {
                        session.command_buffer.push(ch);
                        if session.command_buffer.len() > 1024 {
                            session.command_buffer.clear();
                        }
                    }
                }

                if let Some(input_tx) = &session.input_tx {
                    if let Err(e) = input_tx.send(data.data.to_vec()) {
                        log::error!(
                            "Failed to send data to terminal {}: {}",
                            data.terminal_id,
                            e
                        );
                    }
                }
            }
        }

        Ok(hint_response)
    }

    fn handle_close(
        &self,
        service: &mut PersistentTerminalService,
        close: &CloseTerminal,
    ) -> Result<Option<TerminalResponse>> {
        let mut response = TerminalResponse::new();

        // Always close and remove the terminal
        if let Some(session_arc) = service.sessions.remove(&close.terminal_id) {
            let mut session = session_arc.lock().unwrap();
            #[allow(unused_mut)]
            let exit_code = if let Some(mut child) = session.child.take() {
                #[cfg(target_os = "windows")]
                {
                    use std::os::windows::process::CommandExt;
                    let pid = session.pid;
                    let _ = std::process::Command::new("taskkill")
                        .args(&["/T", "/F", "/PID", &pid.to_string()])
                        .creation_flags(0x08000000) // CREATE_NO_WINDOW
                        .output();
                }
                #[cfg(not(target_os = "windows"))]
                {
                    let _ = child.kill();
                }
                add_to_reaper(child);
                -1 // -1 indicates forced termination
            } else {
                0
            };

            let mut closed = TerminalClosed::new();
            closed.terminal_id = close.terminal_id;
            closed.exit_code = exit_code;
            response.set_closed(closed);
            Ok(Some(response))
        } else {
            Ok(None)
        }
    }

    pub fn read_outputs(&self) -> Vec<TerminalResponse> {
        let service = match get_service(&self.service_id) {
            Some(s) => s,
            None => {
                return vec![];
            }
        };

        // Get session references with minimal service lock time
        let sessions: Vec<(i32, Arc<Mutex<TerminalSession>>)> = {
            let service = service.lock().unwrap();
            service
                .sessions
                .iter()
                .map(|(id, session)| (*id, session.clone()))
                .collect()
        };

        let mut responses = Vec::new();
        let mut closed_terminals = Vec::new();

        // Process each session with its own lock
        for (terminal_id, session_arc) in sessions {
            if let Ok(mut session) = session_arc.try_lock() {
                // Check if reader thread is still alive and we haven't sent closed message yet
                let mut should_send_closed = false;
                if !session.closed_message_sent {
                    if let Some(thread) = &session.reader_thread {
                        if thread.is_finished() {
                            should_send_closed = true;
                        }
                    }
                    if !should_send_closed {
                        if let Some(child) = &mut session.child {
                            match child.try_wait() {
                                Ok(Some(_)) => {
                                    should_send_closed = true;
                                }
                                Ok(None) => {}
                                Err(e) => {
                                    log::warn!("Terminal {} child wait error: {}", terminal_id, e);
                                }
                            }
                        }
                    }
                    #[cfg(target_os = "windows")]
                    if !should_send_closed && !session.has_live_shell() {
                        should_send_closed = true;
                    }
                    if should_send_closed {
                        session.closed_message_sent = true;
                    }
                }
                // It's Ok to put the closed message here.
                // Because the `reader_thread` is joined in `stop()`,
                // and `stop()` is called before the session is dropped.
                if should_send_closed {
                    closed_terminals.push(terminal_id);
                }

                if !session.is_opened {
                    // Skip the session if it is not opened.
                    continue;
                }

                // Read from output channel
                let mut has_activity = false;
                let mut received_data = Vec::new();
                if let Some(output_rx) = &session.output_rx {
                    // Try to read all available data
                    while let Ok(data) = output_rx.try_recv() {
                        has_activity = true;
                        received_data.push(data);
                    }
                }

                // Update buffer after reading
                for data in &received_data {
                    session.output_buffer.append(data);
                }

                // Process received data for responses
                for data in received_data {
                    let mut response = TerminalResponse::new();
                    let mut terminal_data = TerminalData::new();
                    terminal_data.terminal_id = terminal_id;

                    // Compress data if it exceeds threshold
                    if data.len() > COMPRESS_THRESHOLD {
                        let compressed = compress::compress(&data);
                        if compressed.len() < data.len() {
                            terminal_data.data = bytes::Bytes::from(compressed);
                            terminal_data.compressed = true;
                        } else {
                            // Compression didn't help, send uncompressed
                            terminal_data.data = bytes::Bytes::from(data);
                        }
                    } else {
                        terminal_data.data = bytes::Bytes::from(data);
                    }

                    response.set_data(terminal_data);
                    responses.push(response);
                }

                if has_activity {
                    session.update_activity();
                }
            }
        }

        if !closed_terminals.is_empty() {
            for terminal_id in closed_terminals {
                let mut exit_code = 0;

                let session_arc = if !self.is_persistent {
                    service.lock().unwrap().sessions.remove(&terminal_id)
                } else {
                    service.lock().unwrap().sessions.get(&terminal_id).cloned()
                };
                if let Some(session_arc) = session_arc {
                    let mut session = session_arc.lock().unwrap();
                    if let Some(mut child) = session.child.take() {
                        if let Ok(Some(status)) = child.try_wait() {
                            exit_code = status.exit_code() as i32;
                        }
                        add_to_reaper(child);
                    }
                }

                let mut response = TerminalResponse::new();
                let mut closed = TerminalClosed::new();
                closed.terminal_id = terminal_id;
                closed.exit_code = exit_code;
                response.set_closed(closed);
                responses.push(response);
            }
        }

        responses
    }

    /// Cleanup when connection drops
    pub fn on_disconnect(&self) {
        if !self.is_persistent {
            // Remove non-persistent service
            remove_service(&self.service_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{OutputBuffer, MAX_BUFFER_LINES, MAX_OUTPUT_BUFFER_SIZE};

    #[test]
    fn output_buffer_counts_lines_split_across_reads() {
        let mut buffer = OutputBuffer::new();
        for _ in 0..(MAX_BUFFER_LINES * 2) {
            buffer.append(b"start of a long log line ");
            buffer.append(b"end of it\n");
        }
        let stored: usize = buffer.lines.iter().map(|l| l.len()).sum();
        assert_eq!(buffer.total_size, stored);
        assert!(buffer.lines.len() <= MAX_BUFFER_LINES);
    }

    #[test]
    fn output_buffer_trims_output_without_newlines() {
        let mut buffer = OutputBuffer::new();
        let chunk = vec![b'a'; 4096];
        for _ in 0..(MAX_OUTPUT_BUFFER_SIZE / chunk.len()) {
            buffer.append(&chunk);
        }
        buffer.append(b"TAIL");
        assert!(buffer.get_recent(usize::MAX).len() <= MAX_OUTPUT_BUFFER_SIZE);
        assert_eq!(buffer.get_recent(4), b"TAIL");
    }

    #[test]
    fn output_buffer_replays_the_tail_of_one_long_line() {
        let mut buffer = OutputBuffer::new();
        buffer.append(b"first line\n");
        buffer.append(&vec![b'b'; 8192]);
        let recent = buffer.get_recent(4096);
        assert_eq!(recent.len(), 4096);
        assert!(recent.iter().all(|&b| b == b'b'));
        assert_eq!(buffer.get_recent(0), b"");
    }
}
