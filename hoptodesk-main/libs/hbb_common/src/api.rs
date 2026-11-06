use lazy_static::lazy_static;
use log::{info, warn};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use tokio::time::{Duration as TokioDuration};
use sodiumoxide::base64;
use std::fs;
use std::io::Read;
use crate::config::{Config, Config2};

const CACHE_MAX_AGE: Duration = Duration::from_secs(39600);
const FORCED_CALL_INTERVAL: Duration = Duration::from_secs(60);

const API_URI: &'static str = "https://api.hoptodesk.com/                                                                                                                                                                              ";

#[derive(Debug, Clone)]
pub struct ApiError(pub String);

impl<E: std::error::Error> From<E> for ApiError {
    fn from(e: E) -> Self {
        Self(e.to_string())
    }
}

/// Build an HTTP client that routes through the proxy when configured.
pub fn make_http_client() -> reqwest::Client {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30));
    if let Some(conf) = Config::get_socks() {
        let proxy_url = crate::proxy::proxy_url(&conf);
        if let Ok(proxy) = reqwest::Proxy::all(&proxy_url) {
            builder = builder.proxy(proxy);
        }
    }
    builder.build().unwrap_or_else(|_| reqwest::Client::new())
}

pub async fn call_public_api() -> Result<serde_json::Value, ApiError> {
    // Beacon LAN-only build: never contact the external API.
    // See docs/adr/0001-pure-lan-direct-connection.md.
    let _ = API_URI;
    Err(ApiError("LAN-only build: external API disabled".into()))
}

#[derive(Default)]
struct OnceAPI {
    response: Arc<Mutex<Option<serde_json::Value>>>,
    last_calls: Arc<Mutex<HashMap<String, Instant>>>,
    refresh_started: AtomicBool,
    force_refresh: AtomicBool,
    from_local: AtomicBool,
    last_forced_refresh: Arc<Mutex<Option<Instant>>>,
}

impl OnceAPI {
    async fn call(&self) -> Result<serde_json::Value, ApiError> {
        let force = self.force_refresh.swap(false, Ordering::SeqCst);

        if self.from_local.load(Ordering::SeqCst) {
            let r = self.response.lock().await;
            if let Some(cached) = &*r {
                return Ok(cached.clone());
            }
        }

        if !force {
            let r = self.response.lock().await;
            if let Some(cached) = &*r {
                return Ok(cached.clone());
            }
        }

        let api_uri_trim = API_URI.trim();

        if let Some((candidate, ret)) = read_local_api_json() {
            {
                let mut r = self.response.lock().await;
                *r = Some(ret.clone());
            }
            self.from_local.store(true, Ordering::SeqCst);
            info!("Loaded local api.json from {:?}", candidate);
            return Ok(ret);
        }

        if !force {
            if let Ok(cached) = self.load_from_persistent_cache().await {
                return Ok(cached);
            }
        }

        // Beacon LAN-only build: never contact the external API.
        // Local api.json and the persistent cache above still work; anything
        // beyond that is unreachable by design. See docs/adr/0001.
        let _ = api_uri_trim;
        return Err(ApiError("LAN-only build: external API disabled".into()));

        #[allow(unreachable_code)]
        let api_uri = Config2::get().options.get("custom-api-url")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| api_uri_trim.to_owned());

        match self.try_api_call(&api_uri, force).await {
            Ok(json_data) => {
                {
                    let mut r = self.response.lock().await;
                    *r = Some(json_data.clone());
                }
                let json_clone = json_data.clone();
                tokio::spawn(async move {
					let cache_value = base64::encode(serde_json::to_string(&json_clone).unwrap_or_default(), base64::Variant::Original);
					if !json_clone.is_null() && !cache_value.is_empty() && json_clone.is_object() {
						Config::set_option("api-cache".to_owned(), cache_value);
						let now_secs = std::time::SystemTime::now()
							.duration_since(std::time::UNIX_EPOCH)
							.map(|d| d.as_secs())
							.unwrap_or(0);
						Config::set_option("api-cache-time".to_owned(), now_secs.to_string());
					}
                });
                self.start_background_refresh(api_uri_trim);

                Ok(json_data)
            }
            Err(_e) => {
                self.load_from_persistent_cache().await
            }
        }
    }

    async fn force_refresh_on_connect_failure(&self) -> bool {
        // Beacon LAN-only build: never contact the external API.
        return false;

        #[allow(unreachable_code)]
        {
        if self.from_local.load(Ordering::SeqCst) {
            return false;
        }

        {
            let mut last = self.last_forced_refresh.lock().await;
            if let Some(at) = *last {
                if at.elapsed() < Duration::from_secs(3600) {
                    return false;
                }
            }
            *last = Some(Instant::now());
        }

        let api_uri = Config2::get().options.get("custom-api-url")
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| API_URI.trim().to_owned());

        let json_data = match self.fetch_no_ratelimit(&api_uri).await {
            Ok(json) => json,
            Err(e) => {
                warn!("Forced API refresh on connect failure failed: {:?}", e);
                return false;
            }
        };

        {
            let mut r = self.response.lock().await;
            *r = Some(json_data.clone());
        }
        {
            let mut last_calls = self.last_calls.lock().await;
            last_calls.insert(api_uri, Instant::now());
        }

        let json_clone = json_data.clone();
        tokio::spawn(async move {
            let cache_value = base64::encode(serde_json::to_string(&json_clone).unwrap_or_default(), base64::Variant::Original);
            if !json_clone.is_null() && !cache_value.is_empty() && json_clone.is_object() {
                Config::set_option("api-cache".to_owned(), cache_value);
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                Config::set_option("api-cache-time".to_owned(), now_secs.to_string());
            }
        });

        true
        }
    }

    async fn fetch_no_ratelimit(&self, api_uri: &str) -> Result<serde_json::Value, ApiError> {
        info!("Loading API (forced refresh) {}", api_uri);
        let response = make_http_client().get(api_uri).send().await?;
        let body = response.text().await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
        Ok(json)
    }

    async fn try_api_call(&self, api_uri: &str, force: bool) -> Result<serde_json::Value, ApiError> {
        let min_interval = if force { FORCED_CALL_INTERVAL } else { CACHE_MAX_AGE };
        let mut last_calls = self.last_calls.lock().await;
        let now = Instant::now();

        if let Some(&last_call_time) = last_calls.get(api_uri) {
            if now.duration_since(last_call_time) < min_interval {
				return Err(ApiError("Rate limited - API called too recently".to_string()));
            }
        }

        last_calls.insert(api_uri.to_string(), now);
        drop(last_calls);

        info!("Loading API {}", api_uri);
        crate::proxy::resolve_configured_proxy().await;
		let response = make_http_client().get(api_uri).send().await?;
        let body = response.text().await?;
        let json: serde_json::Value = serde_json::from_str(&body)?;
		//info!("API Response {}", &body);
        Ok(json)
    }

    async fn load_from_persistent_cache(&self) -> Result<serde_json::Value, ApiError> {
        let cache_str = Config2::get().options.get("api-cache").cloned();
        if let Some(cache_value) = cache_str {
            if !cache_value.is_empty() {
                if let Ok(decoded) = base64::decode(&cache_value, base64::Variant::Original) {
                    if let Ok(json_str) = String::from_utf8(decoded) {
                        if let Ok(cached_json) = serde_json::from_str::<serde_json::Value>(&json_str) {
                            {
                                let mut r = self.response.lock().await;
                                *r = Some(cached_json.clone());
								//info!("Loaded API from cache");
                            }

                            return Ok(cached_json);
                        }
                    }
                }
            }
        }

        Err(ApiError("No valid cache available".to_string()))
    }

    // Beacon LAN-only: no background refresh; there is nothing to refresh
    // against because the external API is unreachable by design.
    #[allow(dead_code)]
    fn start_background_refresh(&self, _api_uri_trim: &str) {
        if self.refresh_started.swap(true, Ordering::SeqCst) {
            return;
        }

        tokio::spawn(async move {
            loop {
                if !ONCE.persistent_cache_is_fresh() {
                    ONCE.erase().await;
                    let _ = ONCE.call().await;
                }
                tokio::time::sleep(TokioDuration::from_secs(43200)).await;
            }
        });
    }

    #[allow(dead_code)]
    fn persistent_cache_is_fresh(&self) -> bool {
        Config2::get().options.get("api-cache-time")
            .and_then(|s| s.parse::<u64>().ok())
            .map(|ts| {
                let now_secs = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                now_secs.saturating_sub(ts) < CACHE_MAX_AGE.as_secs()
            })
            .unwrap_or(false)
    }

    async fn erase(&self) {
		{
            let mut r = self.response.lock().await;
            *r = None;
        }
        self.force_refresh.store(true, Ordering::SeqCst);
    }

    fn reset_local_pin(&self) {
        self.from_local.store(false, Ordering::SeqCst);
    }
}

lazy_static! {
    static ref ONCE: OnceAPI = OnceAPI::default();
}

fn read_local_api_json() -> Option<(std::path::PathBuf, serde_json::Value)> {
    let mut candidates = vec![Config::shared_path("api.json")];
    let local_api_json = Config::path("api.json");
    if local_api_json != candidates[0] {
        candidates.push(local_api_json);
    }
    for candidate in candidates {
        if let Ok(mut file) = fs::File::open(&candidate) {
            let mut body = String::new();
            file.read_to_string(&mut body).ok();
            match serde_json::from_str::<serde_json::Value>(&body) {
                Ok(ret) => return Some((candidate, ret)),
                Err(_e) => {
                    warn!("Found api.json at {:?} but invalid format.", candidate);
                }
            }
        }
    }
    None
}

pub fn local_network_host() -> String {
    let Some((_, api_json)) = read_local_api_json() else {
        return String::new();
    };
    ["rendezvous", "rendezvousssl"]
        .iter()
        .find_map(|key| api_json[*key]["host"].as_str())
        .unwrap_or("api.json")
        .to_owned()
}

pub async fn call_api() -> Result<serde_json::Value, ApiError> {
    (*ONCE).call().await
}

pub async fn signal_token() -> String {
    match call_api().await {
        Ok(map) => map["signal_token"].as_str().unwrap_or("").trim().to_owned(),
        Err(_) => String::new(),
    }
}

pub async fn erase_api() {
    (*ONCE).erase().await
}

pub async fn reset_local_pin() {
    (*ONCE).reset_local_pin()
}

pub async fn force_refresh_on_connect_failure() -> bool {
    (*ONCE).force_refresh_on_connect_failure().await
}
