use crate::{bail, bytes_codec::BytesCodec, ResultType};
use anyhow::Context as AnyhowCtx;
#[cfg(not(feature = "fips"))]
use bytes::BufMut;
use bytes::{Bytes, BytesMut};
use futures::{SinkExt, StreamExt};
use protobuf::Message;
#[cfg(not(feature = "fips"))]
use std::io::ErrorKind;
#[cfg(not(feature = "fips"))]
use sodiumoxide::crypto::{
    hash::sha256,
    secretbox::{self, Key, Nonce},
};
use std::{
    io::{self, Error},
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    ops::{Deref, DerefMut},
    pin::Pin,
    task::{Context, Poll},
};
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::{lookup_host, TcpListener, TcpSocket, ToSocketAddrs},
};
use tokio_socks::{tcp::Socks5Stream, IntoTargetAddr, ToProxyAddrs};
use tokio_util::codec::Framed;

pub trait TcpStreamTrait: AsyncRead + AsyncWrite + Unpin {}
pub struct DynTcpStream(Box<dyn TcpStreamTrait + Send + Sync>);

#[cfg(not(feature = "fips"))]
#[derive(Clone)]
pub struct Encrypt(Key, Key, u64, u64);

#[cfg(feature = "fips")]
pub trait SessionCipher: Send + Sync {
    fn enc(&mut self, data: &[u8]) -> ResultType<Vec<u8>>;
    fn dec(&mut self, bytes: &mut BytesMut) -> Result<(), Error>;
}

#[cfg(feature = "fips")]
pub type Encrypt = Box<dyn SessionCipher>;

#[cfg(not(feature = "fips"))]
fn sha256_key(parts: &[&[u8]]) -> Key {
    let mut state = sha256::State::new();
    for part in parts {
        state.update(part);
    }
    Key(state.finalize().0)
}

#[cfg(not(feature = "fips"))]
pub fn derive_pq_base(classical: &Key, shared_secret: &[u8], ek: &[u8], ct: &[u8]) -> Key {
    sha256_key(&[b"hoptodesk-pq-v1", &classical.0, shared_secret, ek, ct])
}

#[cfg(not(feature = "fips"))]
pub fn derive_dual_keys(base: &Key) -> (Key, Key) {
    (
        sha256_key(&[b"hoptodesk-dk-v1-h2v", &base.0]),
        sha256_key(&[b"hoptodesk-dk-v1-v2h", &base.0]),
    )
}

pub struct FramedStream(
    Framed<DynTcpStream, BytesCodec>,
    SocketAddr,
    Option<Encrypt>,
    u64,
    Option<BytesMut> // peek buffer 
);

impl Deref for FramedStream {
    type Target = Framed<DynTcpStream, BytesCodec>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for FramedStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl Deref for DynTcpStream {
    type Target = Box<dyn TcpStreamTrait + Send + Sync>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for DynTcpStream {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn new_socket(addr: std::net::SocketAddr, reuse: bool) -> Result<TcpSocket, std::io::Error> {
    let socket = match addr {
        std::net::SocketAddr::V4(..) => TcpSocket::new_v4()?,
        std::net::SocketAddr::V6(..) => TcpSocket::new_v6()?,
    };
    if reuse {
        // windows has no reuse_port, but its reuse_address
        // almost equals to unix's reuse_port + reuse_address,
        // though may introduce nondeterministic behavior
        // illumos has no support for SO_REUSEPORT
        #[cfg(all(unix, not(target_os = "illumos")))]
        socket.set_reuseport(true).ok();
        socket.set_reuseaddr(true).ok();
    }
    socket.bind(addr)?;
    Ok(socket)
}

impl FramedStream {
    pub async fn new<T: ToSocketAddrs + std::fmt::Display>(
        remote_addr: T,
        local_addr: Option<SocketAddr>,
        ms_timeout: u64,
    ) -> ResultType<Self> {
        for remote_addr in lookup_host(&remote_addr).await? {
            let local = if let Some(addr) = local_addr {
                addr
            } else {
                crate::config::Config::get_any_listen_addr(remote_addr.is_ipv4())
            };
            if let Ok(socket) = new_socket(local, true) {
                if let Ok(Ok(stream)) =
                    super::timeout(ms_timeout, socket.connect(remote_addr)).await
                {
                    stream.set_nodelay(true).ok();
                    let addr = stream.local_addr()?;
                    return Ok(Self(
                        Framed::new(DynTcpStream(Box::new(stream)), BytesCodec::new()),
                        addr,
                        None,
                        0,
                        None,
                    ));
                }
            }
        }
        bail!(format!(
            "Failed to connect to {}",
            crate::truncate_addr_display(&remote_addr)
        ));
    }

    pub async fn connect<'a, 't, P, T>(
        proxy: P,
        target: T,
        local_addr: Option<SocketAddr>,
        username: &'a str,
        password: &'a str,
        ms_timeout: u64,
    ) -> ResultType<Self>
    where
        P: ToProxyAddrs,
        T: IntoTargetAddr<'t>,
    {
        if let Some(Ok(proxy)) = proxy.to_proxy_addrs().next().await {
            let local = if let Some(addr) = local_addr {
                addr
            } else {
                crate::config::Config::get_any_listen_addr(proxy.is_ipv4())
            };
            let stream =
                super::timeout(ms_timeout, new_socket(local, true)?.connect(proxy)).await??;
            stream.set_nodelay(true).ok();
            let stream = if username.trim().is_empty() {
                super::timeout(
                    ms_timeout,
                    Socks5Stream::connect_with_socket(stream, target),
                )
                .await??
            } else {
                super::timeout(
                    ms_timeout,
                    Socks5Stream::connect_with_password_and_socket(
                        stream, target, username, password,
                    ),
                )
                .await??
            };
            let addr = stream.local_addr()?;
            return Ok(Self(
                Framed::new(DynTcpStream(Box::new(stream)), BytesCodec::new()),
                addr,
                None,
                0,
                None,
            ));
        }
        bail!("could not resolve to any address");
    }

    pub async fn connect_via_proxy(
        conf: &crate::config::Socks5Server,
        target: &str,
        _local_addr: Option<SocketAddr>,
        ms_timeout: u64,
    ) -> ResultType<Self> {
        let stream = crate::proxy::connect_via_proxy(conf, target, ms_timeout).await?;
        stream.set_nodelay(true).ok();
        let addr = stream.local_addr()?;
        Ok(Self(
            Framed::new(DynTcpStream(Box::new(stream)), BytesCodec::new()),
            addr,
            None,
            0,
            None,
        ))
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.1
    }

    pub fn set_send_timeout(&mut self, ms: u64) {
        self.3 = ms;
    }

    pub fn from(stream: impl TcpStreamTrait + Send + Sync + 'static, addr: SocketAddr) -> Self {
        Self(
            Framed::new(DynTcpStream(Box::new(stream)), BytesCodec::new()),
            addr,
            None,
            0,
            None,
        )
    }

    #[cfg(not(feature = "fips"))]
    pub fn set_raw(&mut self) {
        self.0.codec_mut().set_raw();
        self.2 = None;
    }

    #[cfg(feature = "fips")]
    pub fn set_raw(&mut self) {}

    pub fn is_secured(&self) -> bool {
        self.2.is_some()
    }

    pub fn set_max_packet_length(&mut self, n: usize) {
        self.0.codec_mut().set_max_packet_length(n);
    }

    #[inline]
    pub async fn send(&mut self, msg: &impl Message) -> ResultType<()> {
        self.send_raw(msg.write_to_bytes()?).await
    }

    #[inline]
    pub async fn send_raw(&mut self, msg: Vec<u8>) -> ResultType<()> {
        let mut msg = msg;
        if let Some(key) = self.2.as_mut() {
            #[cfg(not(feature = "fips"))]
            {
                msg = key.enc(&msg);
            }
            #[cfg(feature = "fips")]
            {
                msg = key.enc(&msg)?;
            }
        }
        #[cfg(not(feature = "fips"))]
        self.send_bytes(bytes::Bytes::from(msg)).await?;
        #[cfg(feature = "fips")]
        self.send_frame(bytes::Bytes::from(msg)).await?;
        Ok(())
    }

    #[cfg(feature = "fips")]
    #[inline]
    pub async fn send_bytes(&mut self, bytes: Bytes) -> ResultType<()> {
        self.send_raw(bytes.to_vec()).await
    }

    #[cfg(feature = "fips")]
    #[inline]
    async fn send_frame(&mut self, bytes: Bytes) -> ResultType<()> {
        if self.3 > 0 {
            super::timeout(self.3, self.0.send(bytes)).await??;
        } else {
            self.0.send(bytes).await?;
        }
        Ok(())
    }

    #[cfg(not(feature = "fips"))]
    #[inline]
    pub async fn send_bytes(&mut self, bytes: Bytes) -> ResultType<()> {
        if self.3 > 0 {
            super::timeout(self.3, self.0.send(bytes)).await??;
        } else {
            self.0.send(bytes).await?;
        }
        Ok(())
    }

    #[inline]
    pub async fn next(&mut self) -> Option<Result<BytesMut, Error>> {
        // if our peek buffer is populated, move the result out of there
        if self.4.is_some() {
            return Ok(self.4.take()).transpose();
        } 
        else {
            return self.next_().await;
        }
    }

    #[inline]
    pub async fn peek(&mut self) -> Option<Result<&BytesMut, Error>> {
        if self.4.is_some() {
            return Ok(self.4.as_ref()).transpose();
        } 
        else {
            let next_res = self.next_().await;
            match next_res {
                Some(Ok(bytes)) => {
                    self.4 = Some(bytes);
                    return Ok(self.4.as_ref()).transpose();
                },
                Some(Err(err)) => Some(Err(err)),
                None => None
            }
        }
    }

    #[inline]
    async fn next_(&mut self) -> Option<Result<BytesMut, Error>> {
        let mut res: Option<Result<BytesMut, Error>> = self.0.next().await;

        if let Some(Ok(bytes)) = res.as_mut() {
            if let Some(key) = self.2.as_mut() {
                if let Err(err) = key.dec(bytes) {
                    return Some(Err(err));
                }
            }
        }
        res
    } 

    #[inline]
    pub async fn next_timeout(&mut self, ms: u64) -> Option<Result<BytesMut, Error>> {
        if let Ok(res) = super::timeout(ms, self.next()).await {
            res
        } else {
            None
        }
    }

    #[cfg(not(feature = "fips"))]
    pub fn set_key(&mut self, tx_key: Key, rx_key: Key) {
        self.2 = Some(Encrypt::new(tx_key, rx_key));
    }

    #[cfg(feature = "fips")]
    pub fn set_fips_key(&mut self, key: Encrypt) {
        self.2 = Some(key);
    }

    #[cfg(not(feature = "fips"))]
    fn get_nonce(seqnum: u64) -> Nonce {
        let mut nonce = Nonce([0u8; secretbox::NONCEBYTES]);
        nonce.0[..std::mem::size_of_val(&seqnum)].copy_from_slice(&seqnum.to_le_bytes());
        nonce
    }
}

const DEFAULT_BACKLOG: u32 = 128;

pub async fn new_listener<T: ToSocketAddrs>(addr: T, reuse: bool) -> ResultType<TcpListener> {
    if !reuse {
        Ok(TcpListener::bind(addr).await?)
    } else {
        let addr = lookup_host(&addr)
            .await?
            .next()
            .context("could not resolve to any address")?;
        new_socket(addr, true)?
            .listen(DEFAULT_BACKLOG)
            .map_err(anyhow::Error::msg)
    }
}

pub async fn listen_any(port: u16) -> ResultType<TcpListener> {
    if let Ok(mut socket) = TcpSocket::new_v6() {
        #[cfg(unix)]
        {
            // illumos has no support for SO_REUSEPORT
            #[cfg(not(target_os = "illumos"))]
            socket.set_reuseport(true).ok();
            socket.set_reuseaddr(true).ok();
            use std::os::unix::io::{FromRawFd, IntoRawFd};
            let raw_fd = socket.into_raw_fd();
            let sock2 = unsafe { socket2::Socket::from_raw_fd(raw_fd) };
            sock2.set_only_v6(false).ok();
            socket = unsafe { TcpSocket::from_raw_fd(sock2.into_raw_fd()) };
        }
        #[cfg(windows)]
        {
            use std::os::windows::prelude::{FromRawSocket, IntoRawSocket};
            let raw_socket = socket.into_raw_socket();
            let sock2 = unsafe { socket2::Socket::from_raw_socket(raw_socket) };
            sock2.set_only_v6(false).ok();
            socket = unsafe { TcpSocket::from_raw_socket(sock2.into_raw_socket()) };
        }
        if socket
            .bind(SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), port))
            .is_ok()
        {
            if let Ok(l) = socket.listen(DEFAULT_BACKLOG) {
                return Ok(l);
            }
        }
    }
    Ok(new_socket(
        SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), port),
        true,
    )?
    .listen(DEFAULT_BACKLOG)?)
}

impl Unpin for DynTcpStream {}

impl AsyncRead for DynTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        AsyncRead::poll_read(Pin::new(&mut self.0), cx, buf)
    }
}

impl AsyncWrite for DynTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        AsyncWrite::poll_write(Pin::new(&mut self.0), cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_flush(Pin::new(&mut self.0), cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        AsyncWrite::poll_shutdown(Pin::new(&mut self.0), cx)
    }
}

impl<R: AsyncRead + AsyncWrite + Unpin> TcpStreamTrait for R {}

#[cfg(not(feature = "fips"))]
impl Encrypt {
    pub fn new(tx_key: Key, rx_key: Key) -> Self {
        Self(tx_key, rx_key, 0, 0)
    }

    pub fn dec(&mut self, bytes: &mut BytesMut) -> Result<(), Error> {
        if bytes.len() <= 1 {
            return Ok(());
        }
        self.3 += 1;
        let nonce = FramedStream::get_nonce(self.3);
        match secretbox::open(bytes, &nonce, &self.1) {
            Ok(res) => {
                bytes.clear();
                bytes.put_slice(&res);
                Ok(())
            }
            Err(()) => Err(Error::new(ErrorKind::Other, "decryption error")),
        }
    }

    pub fn enc(&mut self, data: &[u8]) -> Vec<u8> {
        self.2 += 1;
        let nonce = FramedStream::get_nonce(self.2);
        secretbox::seal(&data, &nonce, &self.0)
    }
}

#[cfg(all(test, not(feature = "fips")))]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{:02x}", b)).collect()
    }

    #[test]
    fn dual_keys_match_the_cross_language_vector() {
        let base = Key([0x11u8; 32]);
        let (h2v, v2h) = derive_dual_keys(&base);
        assert_eq!(
            hex(&h2v.0),
            "3046eb179fb802c1bf7dad7b546a6c41b9f54cead405cb452bb6022bdf88e287"
        );
        assert_eq!(
            hex(&v2h.0),
            "1f18bbb5cae1486a71bedc45ef92b5914692d9a22ff5f33aa147851a6d8cd4e6"
        );
    }

    #[test]
    fn pq_base_matches_the_cross_language_vector() {
        let classical = Key([0x11u8; 32]);
        let shared_secret = [0x22u8; 32];
        let ek: Vec<u8> = (0..1184).map(|i| (i % 251) as u8).collect();
        let ct: Vec<u8> = (0..1088).map(|i| (i % 241) as u8).collect();
        let base = derive_pq_base(&classical, &shared_secret, &ek, &ct);
        assert_eq!(
            hex(&base.0),
            "299375f0570178a16699b38e427121e54bf14fa5445a8c0112db8387c563f713"
        );
        let (h2v, v2h) = derive_dual_keys(&base);
        assert_eq!(
            hex(&h2v.0),
            "e35e7a58a3ff0bb94e5d9409c093e7bae359d4f4c3da129c3007763a0914b05f"
        );
        assert_eq!(
            hex(&v2h.0),
            "8689d7f197f15ceb5e74a199922e53df9e8922615cbfb9edfe65fdaa4af330c0"
        );
    }

    #[test]
    fn pq_base_binds_every_input() {
        let c = Key([0x11u8; 32]);
        let ss = [0x22u8; 32];
        let ek = vec![1u8; 1184];
        let ct = vec![2u8; 1088];
        let base = derive_pq_base(&c, &ss, &ek, &ct);
        assert_ne!(base.0, derive_pq_base(&Key([0x12u8; 32]), &ss, &ek, &ct).0);
        assert_ne!(base.0, derive_pq_base(&c, &[0x23u8; 32], &ek, &ct).0);
        assert_ne!(base.0, derive_pq_base(&c, &ss, &vec![3u8; 1184], &ct).0);
        assert_ne!(base.0, derive_pq_base(&c, &ss, &ek, &vec![4u8; 1088]).0);
        assert_ne!(base.0, c.0);
    }

    #[test]
    fn dual_keys_differ_from_each_other_and_from_the_base() {
        let base = Key([0x11u8; 32]);
        let (h2v, v2h) = derive_dual_keys(&base);
        assert_ne!(h2v.0, v2h.0);
        assert_ne!(h2v.0, base.0);
        assert_ne!(v2h.0, base.0);
    }

    #[test]
    fn handshake_messages_encode_identically_to_the_web_client() {
        use crate::message_proto::{IdPk, PublicKey};
        use protobuf::Message as _;

        fn digest(bytes: &[u8]) -> String {
            hex(&sha256::hash(bytes).0)
        }

        let id_pk = IdPk {
            id: "100000001".to_owned(),
            pk: bytes::Bytes::from(vec![0xCCu8; 32]),
            mlkem_ek: bytes::Bytes::from(vec![0xAAu8; 1184]),
            dual_key: true,
            ..Default::default()
        };
        let encoded = id_pk.write_to_bytes().unwrap();
        assert_eq!(encoded.len(), 1234);
        assert_eq!(
            digest(&encoded),
            "b682fbebb65d637970829d7634ee6e8dbef3bc9f0f5496589ddd868eca0b8fcf"
        );

        let public_key = PublicKey {
            asymmetric_value: bytes::Bytes::from(vec![0xDDu8; 32]),
            symmetric_value: bytes::Bytes::from(vec![0xEEu8; 48]),
            mlkem_ct: bytes::Bytes::from(vec![0xBBu8; 1088]),
            dual_key: true,
            ..Default::default()
        };
        let encoded = public_key.write_to_bytes().unwrap();
        assert_eq!(encoded.len(), 1177);
        assert_eq!(
            digest(&encoded),
            "597364bdc41f542e4ef56a48ba54278ad6f50de1db1937fef5ba89a416293b04"
        );

        let classical = PublicKey {
            asymmetric_value: bytes::Bytes::from(vec![0xDDu8; 32]),
            symmetric_value: bytes::Bytes::from(vec![0xEEu8; 48]),
            ..Default::default()
        };
        let encoded = classical.write_to_bytes().unwrap();
        assert_eq!(encoded.len(), 84);
        assert_eq!(
            digest(&encoded),
            "d20920ac47f952b2271d1046ab669475c9dd13d002ab5dd42f0e033c1e84482e"
        );
    }

    #[test]
    fn nonce_layout_is_unchanged() {
        assert_eq!(
            hex(&FramedStream::get_nonce(1).0),
            "010000000000000000000000000000000000000000000000"
        );
        assert_eq!(
            hex(&FramedStream::get_nonce(258).0),
            "020100000000000000000000000000000000000000000000"
        );
    }

    #[test]
    fn mirrored_peers_round_trip_in_both_directions() {
        let (h2v, v2h) = derive_dual_keys(&Key([0x11u8; 32]));
        let mut host = Encrypt::new(h2v.clone(), v2h.clone());
        let mut viewer = Encrypt::new(v2h, h2v);
        for n in 0..4u8 {
            let payload = vec![n; 40];
            let mut wire = BytesMut::from(&host.enc(&payload)[..]);
            viewer.dec(&mut wire).unwrap();
            assert_eq!(&wire[..], &payload[..]);

            let mut wire = BytesMut::from(&viewer.enc(&payload)[..]);
            host.dec(&mut wire).unwrap();
            assert_eq!(&wire[..], &payload[..]);
        }
    }

    #[test]
    fn opposing_directions_no_longer_share_a_keystream() {
        let (h2v, v2h) = derive_dual_keys(&Key([0x11u8; 32]));
        let mut host = Encrypt::new(h2v.clone(), v2h.clone());
        let mut viewer = Encrypt::new(v2h, h2v);
        let payload = vec![0xABu8; 64];
        assert_ne!(host.enc(&payload), viewer.enc(&payload));
    }

    #[test]
    fn a_shared_key_still_round_trips_for_peers_without_dual_key() {
        let key = Key([0x11u8; 32]);
        let mut host = Encrypt::new(key.clone(), key.clone());
        let mut viewer = Encrypt::new(key.clone(), key);
        let payload = vec![7u8; 24];
        let mut wire = BytesMut::from(&host.enc(&payload)[..]);
        viewer.dec(&mut wire).unwrap();
        assert_eq!(&wire[..], &payload[..]);
    }

    #[test]
    fn a_shared_key_reproduces_the_legacy_keystream_collision() {
        let key = Key([0x11u8; 32]);
        let mut host = Encrypt::new(key.clone(), key.clone());
        let mut viewer = Encrypt::new(key.clone(), key);
        let payload = vec![0xABu8; 64];
        assert_eq!(host.enc(&payload), viewer.enc(&payload));
    }
}
