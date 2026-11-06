use futures::SinkExt;
use hbb_common::{
    bail, config::Config, lazy_static, log, socket_client,
    tcp::FramedStream,
    tokio::{self, net::TcpStream, sync::mpsc, time::timeout},
    ResultType,
};
use std::sync::{atomic::Ordering, Mutex};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};
use tokio_rustls::rustls::{self, ClientConfig as TlsClientConfig, OwnedTrustAnchor};
use hbb_common::proxy;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use turn::client::{tcp::TcpTurn, ClientConfig, TlsConfig};
use webrtc_util::conn::Conn;

use crate::rendezvous_messages::{self, ToJson};

lazy_static::lazy_static! {
    static ref PUBLIC_IP: Arc<Mutex<Option<(IpAddr, SocketAddr, Instant)>>> = Default::default();
    static ref TURN_SERVERS_CACHE: Arc<Mutex<Option<(Vec<TurnConfig>, Instant)>>> = Default::default();
    static ref OWNED_RELAY_IPS_CACHE: Arc<Mutex<Option<(Vec<SocketAddr>, Instant)>>> = Default::default();
}

#[derive(Debug, Clone)]
pub struct TurnConfig {
    addr: String,
    username: String,
    password: String,
    tls_config: Option<TlsConfig>,
}

pub struct ServerConfigs {
    pub turn_servers: Vec<TurnConfig>,
}

async fn get_server_configs() -> Option<ServerConfigs> {
    let mut root_cert_store = rustls::RootCertStore::empty();
    root_cert_store.add_trust_anchors(webpki_roots::TLS_SERVER_ROOTS.iter().map(|ta| {
        OwnedTrustAnchor::from_subject_spki_name_constraints(
            ta.subject.as_ref().to_vec(),
            ta.subject_public_key_info.as_ref().to_vec(),
            ta.name_constraints
                .as_ref()
                .map(|nc| nc.as_ref().to_vec()),
        )
    }));
    let tls_config = Arc::new(
        TlsClientConfig::builder()
            .with_safe_defaults()
            .with_root_certificates(root_cert_store)
            .with_no_client_auth(),
    );
    let map = hbb_common::api::call_api().await.ok()?;
    let mut turn_servers = Vec::new();
    let mut seen_addrs = std::collections::HashSet::new();

    // Check if turnservers exists, if not add a dummy server
    if let Some(turnservers) = map["turnservers"].as_array() {
        for server in turnservers {
            let host = server["host"].as_str()?;
            let port = server["port"].as_str()?;
            let addr = if host.contains(':') {
                // IPv6 address: wrap in brackets for valid socket address
                format!("[{}]:{}", host, port)
            } else {
                format!("{}:{}", host, port)
            };

			if seen_addrs.contains(&addr) {
				log::warn!("[turn] Skipping duplicate server");
				continue;
			}

            seen_addrs.insert(addr.clone());

            if server["protocol"].as_str()? == "turn" && !cfg!(feature = "fips") {
                turn_servers.push(TurnConfig {
                    addr,
                    username: server["username"].as_str()?.to_string(),
                    password: server["password"].as_str()?.to_string(),
                    tls_config: None,
                });
            } else if server["protocol"].as_str()? == "turn-tls" {
                turn_servers.push(TurnConfig {
                    addr: addr.clone(),
                    username: server["username"].as_str()?.to_string(),
                    password: server["password"].as_str()?.to_string(),
                    tls_config: Some(TlsConfig {
                        client_config: tls_config.clone(),
                        domain: server["host"].as_str()?.try_into().unwrap(),
                    }),
                });
            }
        }
    } else if !cfg!(feature = "fips") {
        turn_servers.push(TurnConfig {
            addr: "127.0.0.1:3478".to_string(),
            username: "".to_string(),
            password: "".to_string(),
            tls_config: None,
        });
    }

    if !turn_servers.is_empty() {
        log::info!("[turn] Loaded {} TURN servers", turn_servers.len());
    }
    Some(ServerConfigs { turn_servers })
}

async fn get_turn_servers() -> Option<Vec<TurnConfig>> {
    {
        let cached = TURN_SERVERS_CACHE.lock().unwrap();
        if let Some((servers, cached_at)) = cached.as_ref() {
            if cached_at.elapsed() < Duration::from_secs(60) {
                return Some(servers.clone());
            }
        }
    }

    for attempt in 0..3u64 {
        if attempt > 0 {
            log::info!("[turn] Retry attempt {} to fetch TURN servers", attempt);
            tokio::time::sleep(Duration::from_millis(500 * attempt)).await;
        }
        if let Some(configs) = get_server_configs().await {
            if !configs.turn_servers.is_empty() {
                let mut cached = TURN_SERVERS_CACHE.lock().unwrap();
                *cached = Some((configs.turn_servers.clone(), Instant::now()));
                return Some(configs.turn_servers);
            }
        }
    }
    log::warn!("[turn] Failed to fetch TURN servers after 3 attempts");
    None
}

pub fn relay_443_only() -> bool {
    Config::get_option("relay-443-only") == "Y"
}

fn is_port_443(config: &TurnConfig) -> bool {
    config.addr.rsplit(':').next() == Some("443")
}

fn is_owned_443(config: &TurnConfig) -> bool {
    is_port_443(config)
        && config
            .addr
            .rsplit_once(':')
            .map_or(false, |(host, _)| host.ends_with(".hoptodesk.com"))
}

async fn resolve_relay_ips(servers: &[TurnConfig]) -> Vec<SocketAddr> {
    let lookups = servers.iter().map(|config| async move {
        match tokio::net::lookup_host(&config.addr).await {
            Ok(addrs) => addrs
                .filter(|a| a.is_ipv4())
                .map(|a| SocketAddr::new(a.ip(), 1))
                .collect(),
            Err(e) => {
                log::warn!("[turn] could not resolve relay {}: {}", config.addr, e);
                Vec::new()
            }
        }
    });
    let mut ips = Vec::new();
    for found in futures::future::join_all(lookups).await {
        for ip in found {
            if !ips.contains(&ip) {
                ips.push(ip);
            }
        }
    }
    ips
}

pub async fn owned_relay_ips() -> Vec<SocketAddr> {
    {
        let cached = OWNED_RELAY_IPS_CACHE.lock().unwrap();
        if let Some((ips, cached_at)) = cached.as_ref() {
            if cached_at.elapsed() < Duration::from_secs(60) {
                return ips.clone();
            }
        }
    }
    let owned: Vec<TurnConfig> = get_turn_servers()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(is_owned_443)
        .collect();
    let ips = timeout(Duration::from_millis(1000), resolve_relay_ips(&owned))
        .await
        .unwrap_or_default();
    if !ips.is_empty() {
        *OWNED_RELAY_IPS_CACHE.lock().unwrap() = Some((ips.clone(), Instant::now()));
    }
    ips
}

const HOST_RELAY_BUDGET: Duration = Duration::from_millis(4500);
const HOST_CANDIDATE_MS: u64 = 2_000;
const HOST_PLAIN_MS: u64 = 3_500;

pub async fn connect_over_turn_servers(
    peer_id: &str,
    peer_addr: SocketAddr,
    sender: Arc<tokio::sync::Mutex<crate::client::WsSender>>,
    relay_443_only: bool,
) -> ResultType<(Arc<impl Conn>, FramedStream)> {
    let mut forced_refresh_done = false;
    loop {
        let all_servers = match get_turn_servers().await {
            Some(servers) => servers,
            None => bail!("empty turn servers!"),
        };
        let all_servers: Vec<TurnConfig> = if relay_443_only {
            let port_443: Vec<TurnConfig> = all_servers.iter().filter(|c| is_port_443(c)).cloned().collect();
            if port_443.is_empty() {
                log::warn!("[turn] peer wants port 443 only but no relay in the list listens on 443, using the full list");
                all_servers
            } else {
                port_443
            }
        } else {
            all_servers
        };
        let extra_peers = if relay_443_only {
            resolve_relay_ips(&all_servers).await
        } else {
            Vec::new()
        };
        let total_servers = all_servers.len();
        if total_servers == 0 {
            bail!("empty turn servers!");
        }

        let max_attempts = total_servers.min(5);
        let alloc_timeout = tokio::time::Duration::from_secs(5);
        let phase2_timeout = tokio::time::Duration::from_secs(10);
        let mut excluded: Vec<String> = Vec::new();
        let mut last_err: Option<String> = None;

        for attempt in 0..max_attempts {
            let candidates: Vec<TurnConfig> = all_servers
                .iter()
                .filter(|c| !excluded.contains(&c.addr))
                .cloned()
                .collect();
            if candidates.is_empty() {
                break;
            }
            if attempt > 0 {
                log::info!(
                    "[turn] retry attempt {} ({} candidate(s) remaining, {} excluded)",
                    attempt + 1,
                    candidates.len(),
                    excluded.len()
                );
            }

            let candidate_count = candidates.len();
            let (tx, mut rx) = mpsc::channel(candidate_count);
            let mut handles = Vec::new();

            for config in candidates {
                let tx = tx.clone();
                let extra_peers = extra_peers.clone();
                let handle = tokio::spawn(async move {
                    let server_addr = config.addr.clone();
                    let truncated_ip = server_addr
                        .split('.')
                        .take(3)
                        .collect::<Vec<&str>>()
                        .join(".");
                    log::info!("[turn] start establishing over TURN server: {}", truncated_ip);
                    let result = timeout(alloc_timeout, async {
                        let tc = match TurnClient::new(config).await {
                            Ok(tc) => tc,
                            Err(e) => {
                                log::warn!("[turn] init failed for {}: {}", truncated_ip, e);
                                return None;
                            }
                        };
                        match tc.create_relay_connection(peer_addr, &extra_peers).await {
                            Ok((conn, relay_addr)) => {
                                Some((tc, conn, relay_addr, truncated_ip.clone(), server_addr.clone()))
                            }
                            Err(e) => {
                                log::warn!("[turn] allocate failed for {}: {}", truncated_ip, e);
                                None
                            }
                        }
                    })
                    .await
                    .ok()
                    .flatten();
                    let _ = tx.send(result).await;
                });
                handles.push(handle);
            }
            drop(tx);

            let mut chosen = None;
            while let Some(maybe_alloc) = rx.recv().await {
                if let Some(alloc) = maybe_alloc {
                    chosen = Some(alloc);
                    break;
                }
            }
            for handle in handles {
                handle.abort();
            }

            let (turn_client, conn, relay_addr, truncated_ip, server_addr) = match chosen {
                Some(c) => c,
                None => {
                    last_err = Some(format!(
                        "all {} candidate(s) failed to allocate",
                        candidate_count
                    ));
                    break;
                }
            };

            match timeout(
                phase2_timeout,
                establish_over_relay(
                    peer_id,
                    turn_client,
                    conn.clone(),
                    relay_addr,
                    sender.clone(),
                    server_addr.clone(),
                    !relay_443_only,
                ),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    log::info!("[turn] established over TURN server: {}", truncated_ip);
                    log::info!("[turn] successfully established connection over a TURN server");
                    return Ok((conn, stream));
                }
                Ok(Err(e)) => {
                    log::warn!("[turn] relay handshake failed for {}: {}", truncated_ip, e);
                    last_err = Some(format!("{}: {}", truncated_ip, e));
                    excluded.push(server_addr);
                }
                Err(_) => {
                    log::warn!(
                        "[turn] timeout waiting for peer over TURN server: {} (will retry without it)",
                        truncated_ip
                    );
                    last_err = Some(format!(
                        "{}: peer did not connect within {}s",
                        truncated_ip,
                        phase2_timeout.as_secs()
                    ));
                    excluded.push(server_addr);
                }
            }
        }

        if !forced_refresh_done && hbb_common::api::force_refresh_on_connect_failure().await {
            forced_refresh_done = true;
            {
                *TURN_SERVERS_CACHE.lock().unwrap() = None;
            }
            log::info!("[turn] all candidates failed; refreshed TURN list, retrying once");
            continue;
        }

        bail!(
            "Failed to connect via relay server: all {} candidates exhausted (last error: {})",
            excluded.len().max(1),
            last_err.unwrap_or_else(|| "no allocation succeeded".to_string())
        );
    }
}


async fn establish_over_relay(
    peer_id: &str,
    turn_client: TurnClient,
    conn: Arc<impl Conn>,
    relay_addr: SocketAddr,
    sender: Arc<tokio::sync::Mutex<crate::client::WsSender>>,
    server_addr: String,
    permit_owned_relays: bool,
) -> ResultType<FramedStream> {
    let mut sender = sender.lock().await;
    sender
        .send(WsMessage::Text(
            rendezvous_messages::RelayConnection::new(peer_id, relay_addr, Some(server_addr)).to_json(),
        ))
        .await?;
    if permit_owned_relays {
        for ip in owned_relay_ips().await {
            if let Err(e) = conn.send_to(b"init", ip).await {
                log::warn!("[turn] permission for relay {} failed: {}", ip.ip(), e);
            }
        }
    }
    match turn_client.wait_new_connection().await {
        Ok(stream) => {
            sender
                .send(WsMessage::Text(
                    rendezvous_messages::RelayReady::new(peer_id).to_json(),
                ))
                .await?;
            let _ = sender.close().await; // close after established
            return Ok(stream);
        }
        Err(e) => bail!("Failed to connect via relay server: {}", e),
    }
}

pub async fn get_public_ip() -> Option<SocketAddr> {
    {
        let mut cached = PUBLIC_IP.lock().unwrap();
        if let Some((cached_local_ip, public_ip, cached_at)) = *cached {
            //  Time since cached is in 10 minutes.
            if cached_at.elapsed() < Duration::from_secs(600) {
                let local_ip = socket_client::get_lan_ipv4().ok()?;
                if cached_local_ip == local_ip {
                    log::info!("Got public ip from cache");
                    return Some(public_ip);
                }
            }
        }
        *cached = None;
    }

    if let Some(turn_servers) = get_turn_servers().await {
        let len = turn_servers.len();
        let (tx, mut rx) = tokio::sync::mpsc::channel(len);
        for config in turn_servers {
            let tx = tx.clone();
            tokio::spawn(async move {
                if let Ok(turn_client) = TurnClient::new(config).await {
                    if let Ok(addr) = turn_client.get_public_ip().await {
                        let _ = tx.send(Some(addr)).await;
                        return;
                    }
                }
                let _ = tx.send(None).await;
            });
        }
        for _ in 0..len {
            if let Some(Some(addr)) = rx.recv().await {
                if let Ok(local_ip) = socket_client::get_lan_ipv4() {
                    let mut cached = PUBLIC_IP.lock().unwrap();
                    *cached = Some((local_ip, addr, Instant::now()));
                }
                return Some(addr);
            }
        }
        log::warn!("[turn] All TURN servers failed for public IP discovery");
    }

    None
}

pub struct TurnClient {
    client: turn::client::Client,
    local_addr: SocketAddr,
}

impl TurnClient {
    pub async fn new(config: TurnConfig) -> ResultType<Self> {
        // Connect to TURN server - either directly or through proxy
        let stream = if let Some(conf) = Config::get_socks() {
            log::info!("[turn] Connecting to TURN server via proxy: {}", config.addr);
            proxy::connect_via_proxy(&conf, &config.addr, 10_000).await?
        } else {
            TcpStream::connect(&config.addr).await?
        };
        stream.set_nodelay(true).ok();
        let local_addr = stream.local_addr()?;
        // Resolve the TURN server address to fix peer_addr when proxied
        // (into_inner() makes peer_addr point to the SOCKS proxy, not the target)
        let turn_server_addr: SocketAddr = config.addr.parse().or_else(|_| {
            use std::net::ToSocketAddrs;
            config.addr.to_socket_addrs()
                .map(|mut addrs| addrs.next().unwrap())
        })?;
        let mut tcp_turn = if let Some(tls) = config.tls_config.as_ref() {
            TcpTurn::new_tls(tls.client_config.clone(), stream, tls.domain.clone()).await?
        } else {
            TcpTurn::from(stream)
        };
        if Config::get_socks().is_some() {
            tcp_turn.set_peer_addr(turn_server_addr);
        }
        let socks5_proxy = Config::get_socks().map(|conf| {
            let resolved_type = match conf.proxy_type {
                hbb_common::config::ProxyType::Http => turn::client::ProxyType::Http,
                hbb_common::config::ProxyType::Socks5 => turn::client::ProxyType::Socks5,
                hbb_common::config::ProxyType::Auto => {
                    match proxy::get_resolved_proxy_type() {
                        Some(hbb_common::config::ProxyType::Http) => turn::client::ProxyType::Http,
                        _ => turn::client::ProxyType::Socks5,
                    }
                }
            };
            turn::client::ProxyConfig {
                proxy: conf.proxy,
                username: conf.username,
                password: conf.password,
                proxy_type: resolved_type,
            }
        });
        let mut client = turn::client::Client::new(ClientConfig {
            stun_serv_addr: config.addr.clone(),
            turn_serv_addr: config.addr,
            username: config.username,
            password: config.password,
            realm: String::new(),
            tls_config: config.tls_config,
            software: String::new(),
            rto_in_ms: 0,
            conn: Arc::new(tcp_turn),
            vnet: None,
            socks5_proxy,
        })
        .await?;
        client.listen().await?;
        Ok(Self { client, local_addr })
    }

	pub async fn get_public_ip(&self) -> ResultType<SocketAddr> {
        Ok(self.client.send_binding_request().await?)
    }

    pub async fn create_relay_connection(
        &self,
        peer_addr: SocketAddr,
        extra_peers: &[SocketAddr],
    ) -> ResultType<(Arc<impl Conn>, SocketAddr)> {
        let relay_connection = self.client.allocate().await?;
        relay_connection.send_to(b"init", peer_addr).await?;
        for extra in extra_peers {
            if let Err(e) = relay_connection.send_to(b"init", *extra).await {
                log::warn!("[turn] permission for relay {} failed: {}", extra.ip(), e);
            }
        }
        let local_addr = relay_connection.local_addr()?;

        Ok((
            Arc::new(relay_connection),
            local_addr,
        ))
    }

    pub async fn connect_to_peer(&self, peer: SocketAddr) -> ResultType<FramedStream> {
        let stream = self.client.connect(peer).await?;
        Ok(FramedStream::from(stream, self.local_addr))
    }

    pub async fn wait_new_connection(&self) -> ResultType<FramedStream> {
        match self.client.wait_new_connection().await {
            Some(tcp_stream) => Ok(FramedStream::from(tcp_stream, self.local_addr)),
            None => bail!("TURN connection closed while waiting for peer"),
        }
    }
}

pub async fn connect_via_relay(
    peer_relay_addr: SocketAddr,
    viewer_relay: Option<String>,
    owned_only: bool,
) -> ResultType<(Arc<impl Conn>, FramedStream)> {
    let attempts = async {
        let mut last_err = String::new();
        for pass in 0..2 {
            if pass > 0 {
                if !hbb_common::api::force_refresh_on_connect_failure().await {
                    break;
                }
                *TURN_SERVERS_CACHE.lock().unwrap() = None;
                log::info!("[turn] host refreshed the relay list after a failed pass, retrying once");
            }
            let servers = match get_turn_servers().await {
                Some(servers) => servers,
                None => bail!("empty turn servers!"),
            };
            let candidates: Vec<TurnConfig> = servers
                .into_iter()
                .filter(|c| if owned_only { is_owned_443(c) } else { is_port_443(c) })
                .collect();
            if candidates.is_empty() {
                bail!(
                    "no {}port-443 relay in the server list",
                    if owned_only { "owned " } else { "" }
                );
            }
            let (tx, mut rx) = mpsc::channel(candidates.len());
            let winner = Arc::new(std::sync::atomic::AtomicBool::new(false));
            for config in candidates {
                let tx = tx.clone();
                let winner = winner.clone();
                let relay_name = config.addr.clone();
                let same_relay = viewer_relay.as_deref() == Some(relay_name.as_str());
                tokio::spawn(async move {
                    let result = async {
                        let (tc, conn) = match timeout(Duration::from_millis(HOST_CANDIDATE_MS), async {
                            let tc = TurnClient::new(config).await?;
                            let (conn, _) = tc.create_relay_connection(peer_relay_addr, &[]).await?;
                            Ok::<_, hbb_common::anyhow::Error>((tc, conn))
                        })
                        .await
                        {
                            Ok(Ok(pair)) => pair,
                            Ok(Err(e)) => return Err(e),
                            Err(_) => bail!("allocation did not finish within {} ms", HOST_CANDIDATE_MS),
                        };
                        match timeout(Duration::from_millis(HOST_CANDIDATE_MS), tc.connect_to_peer(peer_relay_addr)).await {
                            Ok(Ok(stream)) => Ok((conn, stream)),
                            Ok(Err(e)) => {
                                let _ = conn.close().await;
                                Err(e)
                            }
                            Err(_) => {
                                let _ = conn.close().await;
                                bail!("connect did not finish within {} ms", HOST_CANDIDATE_MS)
                            }
                        }
                    }
                    .await;
                    let result = match result {
                        Ok((conn, stream)) => {
                            if winner
                                .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                                .is_ok()
                            {
                                Ok((conn, stream))
                            } else {
                                let _ = conn.close().await;
                                Err(hbb_common::anyhow::anyhow!("another relay answered first"))
                            }
                        }
                        Err(e) => Err(e),
                    };
                    let _ = tx.send((relay_name, same_relay, result)).await;
                });
            }
            drop(tx);
            while let Some((relay_name, same_relay, result)) = rx.recv().await {
                match result {
                    Ok(result) => {
                        log::info!(
                            "[turn] host reached the viewer's relay slot through port 443 ({} relay: {})",
                            if same_relay { "same" } else { "other" },
                            relay_name
                        );
                        return Ok(result);
                    }
                    Err(e) => {
                        last_err = format!("{}: {}", relay_name, e);
                        log::warn!("[turn] port-443 relay connect failed: {}", last_err);
                    }
                }
            }
        }
        bail!("port-443 relay connect failed ({})", last_err)
    };
    match timeout(HOST_RELAY_BUDGET, attempts).await {
        Ok(result) => result,
        Err(_) => bail!("port-443 relay connect did not finish within {} ms", HOST_RELAY_BUDGET.as_millis()),
    }
}

pub async fn release_relay(relay: Arc<impl Conn>) {
    let _ = relay.close().await;
}

pub fn host_plain_ms() -> u64 {
    HOST_PLAIN_MS
}
