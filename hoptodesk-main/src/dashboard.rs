//! LAN-only stub of the upstream cloud dashboard module.
//! Keeps the public API surface (signatures) so callers compile unchanged;
//! every cloud behaviour is a no-op. See docs/adr/0005-feature-slimming.md.

use hbb_common::ResultType;

pub fn get_ai_activity() -> String {
    String::new()
}

pub fn is_linked() -> bool {
    false
}

pub fn set_pending_quick_connect_token(_token: &str) {}

pub fn has_pending_quick_connect_token() -> bool {
    false
}

pub fn take_pending_quick_connect_token() -> String {
    String::new()
}

pub fn get_invite_code() -> String {
    String::new()
}

pub fn get_dashboard_user_id() -> String {
    String::new()
}

pub async fn get_share_invite() -> ResultType<String> {
    Ok(String::new())
}

pub fn keep_service_owned_options(
    _incoming: &mut std::collections::HashMap<String, String>,
    _current: &std::collections::HashMap<String, String>,
) {
}

pub async fn load_service_link() {}

pub fn invite_account_name(_invite_code: &str) -> ResultType<String> {
    Ok(String::new())
}

pub fn seed_builtin_network(_network: &serde_json::Value) {}

pub async fn release_network_if_unlinked() {}

pub fn get_prefilled_enrollment_token() -> String {
    String::new()
}

pub fn set_prefilled_enrollment_token(_token: &str) {}

pub fn has_enrollment_sentinel() -> bool {
    false
}

pub fn remove_enrollment_artifacts() {}

pub fn percent_decode_path(s: &str) -> String {
    let mut result = Vec::new();
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(
                std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""),
                16,
            ) {
                result.push(byte);
                i += 3;
                continue;
            }
        }
        result.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&result).to_string()
}

pub async fn start() {}

pub fn get_ticket_reply_counter() -> u64 {
    0
}

pub fn verify_uninstall_code(_code: &str) -> bool {
    false
}

pub fn queue_tamper_attempt(_reason: &str) {}

pub fn queue_session_note(_note: String, _billable: bool, _peer_id: &str) {}

pub fn purge_old_recordings() {}

pub async fn mint_avatar_token() -> String {
    String::new()
}

#[derive(Clone)]
pub struct AvatarIdentity {
    pub verified: bool,
    pub display_name: String,
    pub org_name: String,
    pub png: String,
}

pub async fn fetch_avatar_identity(_token: &str) -> Option<AvatarIdentity> {
    None
}

pub async fn validate_invite(
    _invite_code: &str,
) -> ResultType<(String, String, String, String)> {
    Err(hbb_common::anyhow::anyhow!("dashboard disabled in LAN-only build"))
}

pub fn policy_option_key(enable_prefix_option: &str) -> String {
    enable_prefix_option.to_string()
}

pub fn log_file_transfer(
    _transfer_type: &'static str,
    _path: &str,
    _files: Vec<(String, i64)>,
    _peer_name: String,
) {
}

pub fn policy_forces_recording() -> bool {
    false
}

pub fn policy_requires_approval() -> bool {
    false
}

pub fn policy_skips_approval(_purpose: &str) -> bool {
    false
}

pub fn policy_session_timeout() -> Option<u64> {
    None
}

pub async fn verify_easy_access(_token: &str) -> Option<(String, String)> {
    None
}

pub async fn desk_link_verify(_token: &str, _peer: &str) -> bool {
    false
}

pub fn desk_link_ended(_peer: String, _seconds: u64) {}

pub fn upgrade_url() -> String {
    String::new()
}

pub fn is_device_limit_error(_e: &hbb_common::anyhow::Error) -> bool {
    false
}

pub async fn link_device() -> ResultType<()> {
    Ok(())
}

pub fn set_in_session(
    _active: bool,
    _session_type: &str,
    _remote_ip: &str,
    _issue: &str,
    _peer_name: &str,
) {
}

pub fn queue_relayed_recording(
    _r: hbb_common::message_proto::RecordingFinished,
    _technician: &str,
) {
}
