use futures::{SinkExt, StreamExt};
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex as StdMutex,
    },
    time::{Duration as StdDuration, Instant},
};
use tokio::net::TcpStream;
use tokio_tungstenite::{tungstenite::Message as WsMessage, MaybeTlsStream, WebSocketStream};

use hbb_common::{
    allow_err,
    anyhow::bail,
    config::{Config, CONNECT_TIMEOUT, RENDEZVOUS_PORT},
    futures::future::join_all,
    log,
    rendezvous_proto::*,
    sleep, socket_client,
    tokio::{
        self, select,
        time::{interval, Duration},
    },
    ResultType,
};

use crate::{
    rendezvous_messages::{self, ToJson},
    server::{check_zombie, new as new_server, ServerPtr},
    turn_client, ui_interface,
};

static SHOULD_EXIT: AtomicBool = AtomicBool::new(false);
static MANUAL_RESTARTED: AtomicBool = AtomicBool::new(false);

static RELAY_HANDSHAKE_FAILS: StdMutex<Vec<Instant>> = StdMutex::new(Vec::new());
static PROCESS_START: StdMutex<Option<Instant>> = StdMutex::new(None);
const RELAY_FAIL_THRESHOLD: usize = 5;
const RELAY_FAIL_WINDOW_SECS: u64 = 120;
const MIN_UPTIME_BEFORE_RESTART_SECS: u64 = 300;
const SIGNAL_PREFER_TLS: &str = "signal-prefer-tls";

pub fn mark_process_start() {
    if let Ok(mut start) = PROCESS_START.lock() {
        if start.is_none() {
            *start = Some(Instant::now());
        }
    }
}

pub fn note_relay_handshake_success() {
    if let Ok(mut fails) = RELAY_HANDSHAKE_FAILS.lock() {
        fails.clear();
    }
}

#[derive(Debug, PartialEq)]
pub enum WedgeAction {
    Wait,
    NotSupervised,
    DeferActiveSessions(usize),
    DeferYoungProcess(u64),
    Restart,
}

pub fn decide_wedge_action(
    fails_in_window: usize,
    is_supervised_server: bool,
    active_sessions: usize,
    uptime_secs: u64,
) -> WedgeAction {
    if fails_in_window < RELAY_FAIL_THRESHOLD {
        return WedgeAction::Wait;
    }
    if !is_supervised_server {
        return WedgeAction::NotSupervised;
    }
    if active_sessions > 0 {
        return WedgeAction::DeferActiveSessions(active_sessions);
    }
    if uptime_secs < MIN_UPTIME_BEFORE_RESTART_SECS {
        return WedgeAction::DeferYoungProcess(uptime_secs);
    }
    WedgeAction::Restart
}

pub fn note_relay_handshake_failure() {
    let now = Instant::now();
    let window = StdDuration::from_secs(RELAY_FAIL_WINDOW_SECS);
    let count = {
        let mut fails = match RELAY_HANDSHAKE_FAILS.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        fails.retain(|t| now.duration_since(*t) <= window);
        fails.push(now);
        fails.len()
    };
    let uptime = PROCESS_START
        .lock()
        .ok()
        .and_then(|s| *s)
        .map(|t| now.duration_since(t).as_secs())
        .unwrap_or(0);
    let active = crate::server::CONN_COUNT.lock().map(|c| *c).unwrap_or(0);
    match decide_wedge_action(count, crate::common::is_server(), active, uptime) {
        WedgeAction::Wait => {}
        WedgeAction::NotSupervised => log::warn!(
            "Relay handshake send stalled {} times in {}s; not the supervised server process, so not restarting",
            count,
            RELAY_FAIL_WINDOW_SECS
        ),
        WedgeAction::DeferActiveSessions(n) => log::warn!(
            "Relay handshake send stalled {} times in {}s but {} session(s) active; deferring restart",
            count,
            RELAY_FAIL_WINDOW_SECS,
            n
        ),
        WedgeAction::DeferYoungProcess(up) => log::warn!(
            "Relay handshake send stalled {} times in {}s but process has only been up {}s; deferring restart",
            count,
            RELAY_FAIL_WINDOW_SECS,
            up
        ),
        WedgeAction::Restart => {
            log::error!(
                "Relay handshake send stalled {} times in {}s with no active sessions; outbound socket state is wedged",
                count,
                RELAY_FAIL_WINDOW_SECS
            );
            #[cfg(windows)]
            {
                log::error!("Exiting so the service supervisor respawns with fresh socket state");
                std::process::exit(1);
            }
        }
    }
}

#[cfg(test)]
mod wedge_tests {
    use super::*;

    #[test]
    fn below_threshold_does_nothing() {
        assert_eq!(decide_wedge_action(RELAY_FAIL_THRESHOLD - 1, true, 0, 10_000), WedgeAction::Wait);
    }

    #[test]
    fn an_unsupervised_process_is_never_killed() {
        assert_eq!(
            decide_wedge_action(RELAY_FAIL_THRESHOLD, false, 0, 10_000),
            WedgeAction::NotSupervised
        );
    }

    #[test]
    fn a_live_session_defers_the_restart() {
        assert_eq!(
            decide_wedge_action(RELAY_FAIL_THRESHOLD, true, 1, 10_000),
            WedgeAction::DeferActiveSessions(1)
        );
    }

    #[test]
    fn a_freshly_started_process_defers_so_restarts_cannot_loop() {
        assert_eq!(
            decide_wedge_action(RELAY_FAIL_THRESHOLD, true, 0, 1),
            WedgeAction::DeferYoungProcess(1)
        );
    }

    #[test]
    fn only_a_supervised_idle_long_running_process_restarts() {
        assert_eq!(
            decide_wedge_action(RELAY_FAIL_THRESHOLD, true, 0, MIN_UPTIME_BEFORE_RESTART_SECS),
            WedgeAction::Restart
        );
    }
}

#[derive(Clone)]
pub struct RendezvousMediator {
}

impl RendezvousMediator {
    pub fn restart() {
        SHOULD_EXIT.store(true, Ordering::SeqCst);
        MANUAL_RESTARTED.store(true, Ordering::SeqCst);
        log::info!("server restart");
    }

    pub async fn start_all() {
        mark_process_start();
		let mut nat_tested = false;
        check_zombie();
        let server = new_server();
        crate::server::set_incoming_server(&server);
        // Beacon LAN-only: skip NAT-type detection, it talks to the rendezvous server.
        if !crate::lan_only::LAN_ONLY && Config::get_nat_type() == NatType::UNKNOWN_NAT as i32 {
            crate::test_nat_type();
            nat_tested = true;
        }
        if !crate::lan_only::LAN_ONLY && !Config::get_option("stop-service").is_empty() {
            crate::test_rendezvous_server();
        }
        let server_cloned = server.clone();
        tokio::spawn(async move {
            direct_server(server_cloned).await;
        });
        #[cfg(any(target_os = "android", target_os = "ios"))]
        let start_lan_listening = true;
        #[cfg(not(any(target_os = "android", target_os = "ios")))]
        let start_lan_listening = crate::platform::is_installed();
        #[cfg(not(target_os = "ios"))]
        if start_lan_listening {
            std::thread::spawn(move || {
                allow_err!(super::lan::start_listening());
            });
        }
        // It is ok to run xdesktop manager when the headless function is not allowed.
        #[cfg(target_os = "linux")]
        if crate::is_server() {
            crate::platform::linux_desktop_manager::start_xdesktop();
        }
        loop {
            let conn_start_time = Instant::now();
            Config::reset_online();
            // Beacon LAN-only: mark the client as "online" so the UI shows
            // Ready instead of waiting for a rendezvous server forever.
            if crate::lan_only::LAN_ONLY {
                Config::update_latency("lan-direct", 1);
            }
            // Beacon LAN-only: no rendezvous registration, direct IP access only.
            if !crate::lan_only::LAN_ONLY && Config::get_option("stop-service").is_empty() {
                if !nat_tested {
                    crate::test_nat_type();
                    nat_tested = true;
                }
                let mut futs = Vec::new();
				if let Some(servers) = Config::get_rendezvous_servers().await {
                    SHOULD_EXIT.store(false, Ordering::SeqCst);
               		MANUAL_RESTARTED.store(false, Ordering::SeqCst);
                    for host in servers.clone() {
                        let server = server.clone();
                        futs.push(tokio::spawn(async move {
	                        if let Err(err) = Self::start(server, host).await {
	                            log::error!("Signal error: {err}");
	                        }
							//allow_err!(Self::start(server, host).await);
                            // SHOULD_EXIT here is to ensure once one exits, the others also exit.
                            SHOULD_EXIT.store(true, Ordering::SeqCst);
                        }));
                    }
                    join_all(futs).await;
		            if !MANUAL_RESTARTED.load(Ordering::SeqCst) {
		                let elapsed = conn_start_time.elapsed().as_millis() as u64;
		                if elapsed < CONNECT_TIMEOUT {
		                    sleep(((CONNECT_TIMEOUT - elapsed) / 1000) as _).await;
		                }
		            } else {
		                sleep(0.033).await;
		            }
                }
            }

            sleep(1.).await;
        }
    }

    pub async fn start(server: ServerPtr, host_list: String) -> ResultType<()> {
        let public_addr = match turn_client::get_public_ip().await {
            Some(addr) => addr,
            None => bail!("Failed to retreive public IP address"),
        };

        let (local_ip, host, websocket_client) = create_websocket(&host_list).await?;

        let (mut sender, receiver) = websocket_client.split();
        Config::update_latency(&host, 200);
        Config::set_key_confirmed(true);
        Config::set_host_key_confirmed(&host, true);

        const HEALTHCHECK: &str = r#"{"protocol":"one-to-self","data":"healthcheck"}"#;
        const HEALTHCHECK_ERROR: &str = r#"You are removed"#;

        const TIMER_OUT: Duration = Duration::from_secs(1);
        let mut last_timer = Instant::now();
        let mut timer = interval(TIMER_OUT);

        hbb_common::timeout(10_000, sender.send(WsMessage::Text(HEALTHCHECK.to_owned()))).await??;
        let mut last_healthcheck_sent = Some(chrono::Utc::now());
        let mut server_answered = false;
        let mut last_data_received = chrono::Utc::now();
        let connected_lan_ip = socket_client::get_lan_ipv4().ok();

        let socket_packets = futures::stream::unfold(receiver, move |mut receiver| async {
            match receiver.next().await {
                Some(Ok(msg)) => Some((Ok(msg), receiver)),
                Some(Err(err)) => Some((Err(err), receiver)),
                None => None,
            }
        }).fuse();

        tokio::pin!(socket_packets);
        loop {
            select! {
                _ = timer.tick() => {
                    if SHOULD_EXIT.load(Ordering::SeqCst) {
                        break;
                    }
                    let now = Instant::now();
                    let now_utc = chrono::Utc::now();
                    let elapsed = now.duration_since(last_timer);
                    if elapsed < TIMER_OUT {
                        // a workaround of tokio timer bug
                                    continue;
                    }
                    last_timer = now;
                    if elapsed > Duration::from_secs(30) {
                        log::info!("Detected sleep/wake (timer gap={}s), reconnecting signal server.", elapsed.as_secs());
                        break;
                    }
                    if let (Some(prev), Ok(curr)) = (connected_lan_ip, socket_client::get_lan_ipv4()) {
                        if curr != prev && !curr.is_unspecified() {
                            log::info!("Local IP changed {} -> {}, reconnecting signal server", prev, curr);
                            break;
                        }
                    }
                    if let Some(last_healthcheck_sent) = last_healthcheck_sent {
                        if now_utc - last_healthcheck_sent > chrono::Duration::seconds(30) {
                            log::info!("Server is unresponding, disconnect.");
                            if !server_answered && host.starts_with("ws:") {
                                log::info!("{} never answered after connecting, using TLS for the signal connection from now on", host);
                                Config::set_option(SIGNAL_PREFER_TLS.to_owned(), "Y".to_owned());
                            }
                            break;
                        }
                    }
                    if last_healthcheck_sent.is_none()
                        && now_utc - last_data_received > chrono::Duration::seconds(90) {
                        match hbb_common::timeout(10_000, sender.send(WsMessage::Text(HEALTHCHECK.to_owned()))).await {
                            Ok(Ok(())) => {}
                            Ok(Err(error)) => {
                                log::info!("Send error: {error}, disconnect.");
                                break;
                            }
                            Err(_) => {
                                log::info!("Send timeout, disconnect.");
                                break;
                            }
                        }

                        last_healthcheck_sent = Some(chrono::Utc::now());
                    }

                }
                Some(data) = socket_packets.next() => {
                    match data {
                    Ok(tokio_tungstenite::tungstenite::Message::Text(msg)) => {

                        if let Ok(close_sessions) = serde_json::from_str::<rendezvous_messages::CloseSessions>(&msg) {
                            let data: Vec<&str> = close_sessions.data.split(':').collect();
                            let mut invalid = true;
                            if data.len() == 2 {
                                if data[0] == "closeoutgoing" {
                                    invalid = false;
                                    #[cfg(not(target_os = "ios"))]
                                    ui_interface::close_remote(data[1]);
                                } else if data[0] == "closeincoming" {
                                    invalid = false;
                                    #[cfg(not(target_os = "ios"))]
                                    match crate::ipc::connect(1000, "_cm").await {
                                        Ok(mut conn) => if let Err(e) = conn.send(&crate::ipc::Data::CloseIncoming { id: data[1].to_owned() }).await {
                                            log::error!("Failed to send to ipc: {}", e);
                                        }
                                        Err(e) => log::error!("Can't connect to IPC: {}", e)
                                    }
                                }
                            }
                            if invalid {
                                log::error!("Invalid data received: {}", close_sessions.data);
                            }
                        } else if let Ok(connect_request) =
							serde_json::from_str::<rendezvous_messages::ConnectRequest>(&msg){
                            last_data_received = chrono::Utc::now();
                            if connect_request.endpoint != Config::get_id() {
                                log::debug!(
                                    "Ignoring connect request addressed to {}",
                                    connect_request.endpoint
                                );
                                continue;
                            }
                            let listener =
                                hbb_common::tcp::new_listener(SocketAddr::new(local_ip, 0), true)
                                .await?;

                                let nat_type = Config::get_nat_type();
                                let primary_bound = listener.local_addr().unwrap();

								let listening = rendezvous_messages::Listening::new(
                                    connect_request.sender_id,
                                    primary_bound,
                                    public_addr,
                                    Config::get_key_pair().1,
                                    nat_type,
                                    turn_client::relay_443_only(),
                                );
                                let result = hbb_common::timeout(
                                    10_000,
                                    sender.send(
                                        tokio_tungstenite::tungstenite::Message::Text(listening.to_json()),
                                    ),
                                )
                                .await;
                                match result {
                                Ok(Ok(_)) => {
                                    let server_clone = server.clone();
                                    tokio::spawn(async move {
                                        if let Err(_error) = crate::server::accept(listener, server_clone, true, false).await {
                                        } else {
                                        }
                                        });
                                        if listening.require_listen_ipv4() {
                                            let ipv4_listener = hbb_common::tcp::new_listener(listening.lan_ipv4.unwrap(), true).await?;
                                            let server_clone = server.clone();
                                            tokio::spawn(async move {
                                                log::info!("Starting accept ipv4...");
                                                if let Err(error) = crate::accept(ipv4_listener, server_clone, true, false).await {
                                                    log::error!("accept() failed: {:?}", error);
                                                }
                                            });
                                        }
                                    }
                                    Ok(Err(error)) => {
                                        log::error!("WS send failed: {:?}, disconnect.", error);
                                        break;
                                    }
                                    Err(_) => {
                                        log::error!("WS send timeout, disconnect.");
                                        break;
                                    }
                                }
                            } else if let Ok(relay_connection) =
                            serde_json::from_str::<rendezvous_messages::RelayConnection>(&msg)
                        {
                            last_data_received = chrono::Utc::now();
                            let mut pending_addr = relay_connection.addr;
                            let mut pending_relay = relay_connection.relay.clone();
                            loop {
                                let host_relay = if turn_client::relay_443_only() {
                                    match turn_client::connect_via_relay(pending_addr, pending_relay.clone(), false).await {
                                        Ok((relay, stream)) => Ok((Some(relay), stream)),
                                        Err(e) => {
                                            log::warn!("Firewall mode relay connect to {} failed, trying a plain connection: {}", pending_addr, e);
                                            socket_client::connect_tcp(pending_addr, turn_client::host_plain_ms())
                                                .await
                                                .map(|stream| (None, stream))
                                        }
                                    }
                                } else {
                                    match socket_client::connect_tcp(pending_addr, turn_client::host_plain_ms()).await {
                                        Ok(stream) => Ok((None, stream)),
                                        Err(e) => {
                                            log::info!("Plain dial to the viewer's relay slot {} failed ({}), trying port 443 through our relay", pending_addr, e);
                                            turn_client::connect_via_relay(pending_addr, pending_relay.clone(), true)
                                                .await
                                                .map(|(relay, stream)| (Some(relay), stream))
                                        }
                                    }
                                };
                                let (relay, stream) = match host_relay {
                                    Ok(pair) => pair,
                                    Err(e) => {
                                        log::warn!("Relay connection to {} failed: {}", pending_addr, e);
                                        break;
                                    }
                                };
                                match tokio::time::timeout(Duration::from_secs(10), socket_packets.next()).await {
                                    Ok(Some(Ok(tokio_tungstenite::tungstenite::Message::Text(msg)))) => {
                                        if let Ok(next) = serde_json::from_str::<rendezvous_messages::RelayConnection>(&msg) {
                                            log::info!("Viewer moved to a new relay slot before ready, retrying");
                                            if let Some(relay) = relay {
                                                turn_client::release_relay(relay).await;
                                            }
                                            pending_addr = next.addr;
                                            pending_relay = next.relay.clone();
                                            continue;
                                        }
                                        if serde_json::from_str::<rendezvous_messages::RelayReady>(&msg).is_ok() {
                                            let server_clone = server.clone();
                                            let addr = pending_addr;
                                            tokio::spawn(async move {
                                                if let Err(e) = crate::create_tcp_connection(
                                                    server_clone,
                                                    stream,
                                                    addr,
                                                    true,
                                                    false,
                                                    true,
                                                )
                                                    .await
                                                {
                                                    log::error!("Relay connection to {} failed: {}", addr, e);
                                                }
                                                if let Some(relay) = relay {
                                                    turn_client::release_relay(relay).await;
                                                }
                                            });
                                        } else if let Some(relay) = relay {
                                            turn_client::release_relay(relay).await;
                                        }
                                    }
                                    Ok(_) => {
                                        if let Some(relay) = relay {
                                            turn_client::release_relay(relay).await;
                                        }
                                    }
                                    Err(err) => {
                                        log::error!("receive relay from channel timeout: {}", err);
                                        if let Some(relay) = relay {
                                            turn_client::release_relay(relay).await;
                                        }
                                    }
                                }
                                break;
                            }
                        } else if msg == HEALTHCHECK {
                            last_healthcheck_sent = None;
                            last_data_received = chrono::Utc::now();
                            server_answered = true;
                        } else if msg == HEALTHCHECK_ERROR {
                            log::info!("Connection removed on server, disconnect.");
                            break;
                        }
                    }
                    Err(e) => bail!("Failed to receive next {}", e),
                    _ => bail!("Received binary message from signal server"),
                    }
                }
            }
        }
        Ok(())
    }
}

async fn create_websocket(
    host_list: &str,
) -> ResultType<(
    std::net::IpAddr,
    String,
    WebSocketStream<MaybeTlsStream<TcpStream>>,
)> {
	
    let mut hosts: Vec<&str> = host_list.split(';').collect();
    let tls_first = Config::get_option(SIGNAL_PREFER_TLS) == "Y";
    hosts.sort_by_key(|host| host.starts_with("ws:") == tls_first);

	
    for host in hosts {
		let ret = crate::rendezvous_ws::create_websocket_(host, None, 0).await;
        allow_err!(&ret);

        if ret.is_ok() {
            return ret;
        }
    }

    bail!("Failed to connect any of the hosts in list");
}

fn get_direct_port() -> i32 {
    let mut port = Config::get_option("direct-access-port")
        .parse::<i32>()
        .unwrap_or(0);
    if port <= 0 {
        port = RENDEZVOUS_PORT + 2;
    }
    port
}

async fn direct_server(server: ServerPtr) {
    let mut listener = None;
    let mut port = 0;
    loop {
        // Beacon LAN-only: direct access is the only mode, so it is on by
        // default and must be explicitly disabled with direct-server=N.
        let disabled = Config::get_option("direct-server") == "N"
            || !Config::get_option("stop-service").is_empty();
        if !disabled && listener.is_none() {
            port = get_direct_port();
            let addr = format!("0.0.0.0:{}", port);
            match hbb_common::tcp::new_listener(&addr, false).await {
                Ok(l) => {
                    listener = Some(l);
                    log::info!(
                        "Direct server listening on: {:?}",
                        listener.as_ref().unwrap().local_addr()
                    );
                }
                Err(err) => {
                    // to-do: pass to ui
                    log::error!(
                        "Failed to start direct server on : {}, error: {}",
                        addr,
                        err
                    );
                    loop {
                        if port != get_direct_port() {
                            break;
                        }
                        sleep(1.).await;
                    }
                }
            }
        }
        if let Some(l) = listener.as_mut() {
            if disabled || port != get_direct_port() {
                log::info!("Exit direct access listen");
                listener = None;
                continue;
            }
            if let Ok(Ok((stream, addr))) = hbb_common::timeout(1000, l.accept()).await {
                stream.set_nodelay(true).ok();
                log::info!("direct access from {}", addr);
                let local_addr = stream
                    .local_addr()
                    .unwrap_or(Config::get_any_listen_addr(true));
                let server = server.clone();
                tokio::spawn(async move {
                    allow_err!(
                        crate::server::create_tcp_connection(
                            server,
                            hbb_common::Stream::from(stream, local_addr),
                            addr,
                            true,
                            true,
                            false,
                        )
                        .await
                    );
                });
            } else {
                sleep(0.1).await;
            }
        } else {
            sleep(1.).await;
        }
    }
}
