use hbb_common::{anyhow::anyhow, bail, bytes::{Buf, BytesMut}, tcp::SessionCipher, ResultType};
use aws_lc_rs::{
    aead::{self, Aad, Nonce, RandomizedNonceKey, AES_256_GCM},
    agreement::{self, EphemeralPrivateKey, UnparsedPublicKey, ECDH_P256},
    digest::{self, SHA256},
    encoding::AsBigEndian,
    hkdf::{Prk, Salt, HKDF_SHA256},
    kem::{Ciphertext, DecapsulationKey, EncapsulationKey, ML_KEM_768},
    rand::SystemRandom,
    signature::{self, Ed25519KeyPair, KeyPair},
};
use std::io::{Error, ErrorKind};

pub const P256_LEN: usize = 65;
pub const MLKEM_EK_LEN: usize = 1184;
pub const MLKEM_CT_LEN: usize = 1088;
const SIGNATURE_LEN: usize = 64;
const SEED_LEN: usize = 32;
const TAG_LEN: usize = 16;
const FRAMES_PER_KEY_BITS: u32 = 28;
const H2V: &[u8] = b"hoptodesk-fips-v1 h2v";
const V2H: &[u8] = b"hoptodesk-fips-v1 v2h";
const CODE: &[u8] = b"hoptodesk-fips-v1 code";

pub const PEER_NOT_FIPS: &str = "FIPS handshake failed: this device does not run a FIPS build of HopToDesk. A FIPS build connects only to other FIPS builds of HopToDesk.";
pub const HOST_KEY_UNKNOWN: &str = "FIPS handshake failed: your server has no identity key for this device, so it cannot be verified. Connect to it by ID while it is online and signed in to your server.";
pub const VIEWER_NOT_FIPS: &str = "FIPS mode: refused a connection from an app that is not a FIPS build of HopToDesk (a standard or older app, the web client or the XP app)";

pub fn new_identity() -> ResultType<(Vec<u8>, Vec<u8>)> {
    let pair = Ed25519KeyPair::generate()?;
    let seed = pair.seed()?.as_be_bytes()?;
    let pk = pair.public_key().as_ref().to_vec();
    Ok(([seed.as_ref(), &pk].concat(), pk))
}

pub fn sign(sk: &[u8], msg: &[u8]) -> ResultType<Vec<u8>> {
    if sk.len() != SEED_LEN * 2 {
        bail!("FIPS mode: the identity key has the wrong length");
    }
    let pair = Ed25519KeyPair::from_seed_and_public_key(&sk[..SEED_LEN], &sk[SEED_LEN..])?;
    let sig = pair.try_sign(msg)?;
    Ok([sig.as_ref(), msg].concat())
}

pub fn open_signed(signed: &[u8], pk: &[u8]) -> ResultType<Vec<u8>> {
    if signed.len() < SIGNATURE_LEN {
        bail!("Signature mismatch");
    }
    let (sig, msg) = signed.split_at(SIGNATURE_LEN);
    signature::UnparsedPublicKey::new(&signature::ED25519, pk)
        .verify(msg, sig)
        .map_err(|_| anyhow!("Signature mismatch"))?;
    Ok(msg.to_vec())
}

fn ecdh_key() -> ResultType<(EphemeralPrivateKey, Vec<u8>)> {
    let key = EphemeralPrivateKey::generate(&ECDH_P256, &SystemRandom::new())?;
    let public = key.compute_public_key()?.as_ref().to_vec();
    Ok((key, public))
}

fn ecdh_agree(key: EphemeralPrivateKey, peer: &[u8]) -> ResultType<Vec<u8>> {
    Ok(agreement::agree_ephemeral(
        key,
        &UnparsedPublicKey::new(&ECDH_P256, peer),
        aws_lc_rs::error::Unspecified,
        |z| Ok(z.to_vec()),
    )?)
}

fn kem_key() -> ResultType<(DecapsulationKey, Vec<u8>)> {
    let key = DecapsulationKey::generate(&ML_KEM_768)?;
    let ek = key.encapsulation_key()?.key_bytes()?.as_ref().to_vec();
    Ok((key, ek))
}

fn kem_encapsulate(ek: &[u8]) -> ResultType<(Vec<u8>, Vec<u8>)> {
    let (ct, t) = EncapsulationKey::new(&ML_KEM_768, ek)?.encapsulate()?;
    Ok((ct.as_ref().to_vec(), t.as_ref().to_vec()))
}

fn kem_decapsulate(key: &DecapsulationKey, ct: &[u8]) -> ResultType<Vec<u8>> {
    Ok(key.decapsulate(Ciphertext::from(ct))?.as_ref().to_vec())
}

fn transcript(offer: &[u8], viewer_p256: &[u8], ct: &[u8]) -> digest::Digest {
    let mut ctx = digest::Context::new(&SHA256);
    ctx.update(offer);
    ctx.update(viewer_p256);
    ctx.update(ct);
    ctx.finish()
}

fn extract(transcript: &digest::Digest, z: &[u8], t: &[u8]) -> Prk {
    Salt::new(HKDF_SHA256, transcript.as_ref()).extract(&[z, t].concat())
}

fn expand(prk: &Prk, label: &[u8], epoch: u32) -> ResultType<[u8; 32]> {
    let epoch = epoch.to_be_bytes();
    let info = [label, &epoch[..]];
    let mut out = [0u8; 32];
    prk.expand(&info, HKDF_SHA256)?.fill(&mut out)?;
    Ok(out)
}

fn check_len(name: &str, value: &[u8], len: usize) -> ResultType<()> {
    if value.len() != len {
        bail!("FIPS handshake: {} has {} bytes, expected {}", name, value.len(), len);
    }
    Ok(())
}

pub struct HostOffer {
    ecdh: EphemeralPrivateKey,
    kem: DecapsulationKey,
    pub p256: Vec<u8>,
    pub mlkem_ek: Vec<u8>,
}

impl HostOffer {
    pub fn new() -> ResultType<Self> {
        let (ecdh, p256) = ecdh_key()?;
        let (kem, mlkem_ek) = kem_key()?;
        Ok(Self { ecdh, kem, p256, mlkem_ek })
    }

    pub fn accept(self, signed_offer: &[u8], viewer_p256: &[u8], ct: &[u8]) -> ResultType<Session> {
        check_len("viewer P-256 key", viewer_p256, P256_LEN)?;
        check_len("ML-KEM ciphertext", ct, MLKEM_CT_LEN)?;
        let t = kem_decapsulate(&self.kem, ct)?;
        let z = ecdh_agree(self.ecdh, viewer_p256)?;
        Session::new(extract(&transcript(signed_offer, viewer_p256, ct), &z, &t))
    }
}

pub fn answer(signed_offer: &[u8], host_p256: &[u8], mlkem_ek: &[u8]) -> ResultType<(Vec<u8>, Vec<u8>, Session)> {
    check_len("host P-256 key", host_p256, P256_LEN)?;
    check_len("ML-KEM key", mlkem_ek, MLKEM_EK_LEN)?;
    let (ecdh, p256) = ecdh_key()?;
    let (ct, t) = kem_encapsulate(mlkem_ek)?;
    let z = ecdh_agree(ecdh, host_p256)?;
    let session = Session::new(extract(&transcript(signed_offer, &p256, &ct), &z, &t))?;
    Ok((p256, ct, session))
}

pub struct Session {
    prk: Prk,
    pub security_code: String,
}

impl Session {
    fn new(prk: Prk) -> ResultType<Self> {
        let code = expand(&prk, CODE, 0)?;
        let security_code = code
            .chunks(2)
            .map(|pair| format!("{:0>5} ", u16::from_be_bytes([pair[0], pair[1]])))
            .collect();
        Ok(Self { prk, security_code })
    }

    pub fn host(&self) -> Cipher {
        Cipher::new(&self.prk, H2V, V2H)
    }

    pub fn viewer(&self) -> Cipher {
        Cipher::new(&self.prk, V2H, H2V)
    }
}

struct Direction {
    prk: Prk,
    label: &'static [u8],
    seq: u64,
    key: Option<(u32, RandomizedNonceKey)>,
}

impl Direction {
    fn next(&mut self) -> ResultType<([u8; 8], &RandomizedNonceKey)> {
        self.seq += 1;
        let epoch = (self.seq >> FRAMES_PER_KEY_BITS) as u32;
        if self.key.as_ref().map(|k| k.0) != Some(epoch) {
            let bytes = expand(&self.prk, self.label, epoch)?;
            self.key = Some((epoch, RandomizedNonceKey::new(&AES_256_GCM, &bytes)?));
        }
        let key = &self.key.as_ref().ok_or_else(|| anyhow!("no session key"))?.1;
        Ok((self.seq.to_be_bytes(), key))
    }
}

pub struct Cipher {
    tx: Direction,
    rx: Direction,
}

impl Cipher {
    fn new(prk: &Prk, tx: &'static [u8], rx: &'static [u8]) -> Self {
        let direction = |label| Direction { prk: prk.clone(), label, seq: 0, key: None };
        Self { tx: direction(tx), rx: direction(rx) }
    }
}

impl SessionCipher for Cipher {
    fn enc(&mut self, data: &[u8]) -> ResultType<Vec<u8>> {
        let (aad, key) = self.tx.next()?;
        let mut sealed = data.to_vec();
        let nonce = key.seal_in_place_append_tag(Aad::from(aad), &mut sealed)?;
        Ok([nonce.as_ref(), &sealed[..]].concat())
    }

    fn dec(&mut self, bytes: &mut BytesMut) -> Result<(), Error> {
        let failed = || Error::new(ErrorKind::Other, "decryption error");
        if bytes.len() < aead::NONCE_LEN + TAG_LEN {
            return Err(failed());
        }
        let (aad, key) = self.rx.next().map_err(|_| failed())?;
        let nonce = Nonce::try_assume_unique_for_key(&bytes[..aead::NONCE_LEN]).map_err(|_| failed())?;
        let plain = key
            .open_in_place(nonce, Aad::from(aad), &mut bytes[aead::NONCE_LEN..])
            .map_err(|_| failed())?
            .len();
        bytes.advance(aead::NONCE_LEN);
        bytes.truncate(plain);
        Ok(())
    }
}

fn approved<T>(name: &'static str, lines: &mut Vec<String>, f: impl FnOnce() -> ResultType<T>) -> ResultType<T> {
    let before = unsafe { aws_lc_fips_sys::FIPS_service_indicator_before_call() };
    let out = f();
    let after = unsafe { aws_lc_fips_sys::FIPS_service_indicator_after_call() };
    let out = out.map_err(|e| anyhow!("{} failed: {}", name, e))?;
    if after <= before {
        bail!("{} is not an approved service", name);
    }
    lines.push(format!("{}: approved", name));
    Ok(out)
}

pub fn self_check() -> ResultType<Vec<String>> {
    let mut l = Vec::new();
    let (sk, pk) = approved("Ed25519 key generation", &mut l, new_identity)?;
    let offer = approved("Ed25519 signing", &mut l, || sign(&sk, b"hoptodesk-fips-v1 self-check"))?;
    approved("Ed25519 verification", &mut l, || open_signed(&offer, &pk))?;
    let (host_ecdh, host_p256) = approved("ECDH P-256 key generation", &mut l, ecdh_key)?;
    let (viewer_ecdh, viewer_p256) = ecdh_key()?;
    let (kem, ek) = approved("ML-KEM-768 key generation", &mut l, kem_key)?;
    let (ct, t) = approved("ML-KEM-768 encapsulation", &mut l, || kem_encapsulate(&ek))?;
    let t2 = approved("ML-KEM-768 decapsulation", &mut l, || kem_decapsulate(&kem, &ct))?;
    let z = approved("ECDH P-256 shared secret", &mut l, || ecdh_agree(host_ecdh, &viewer_p256))?;
    let z2 = ecdh_agree(viewer_ecdh, &host_p256)?;
    if z != z2 || t != t2 {
        bail!("key agreement gave different secrets");
    }
    let th = approved("SHA-256", &mut l, || Ok(transcript(&offer, &viewer_p256, &ct)))?;
    let prk = extract(&th, &z, &t);
    approved("HKDF-SHA-256", &mut l, || expand(&prk, H2V, 0))?;
    let session = Session::new(prk)?;
    let (mut host, mut viewer) = (session.host(), session.viewer());
    let mut wire = BytesMut::from(&host.enc(b"warm-up")?[..]);
    viewer.dec(&mut wire)?;
    let sealed = approved("AES-256-GCM seal (module IV)", &mut l, || host.enc(b"frame"))?;
    let mut wire = BytesMut::from(&sealed[..]);
    approved("AES-256-GCM open", &mut l, || Ok(viewer.dec(&mut wire)?))?;
    if &wire[..] != b"frame" {
        bail!("AES-256-GCM round trip gave the wrong bytes");
    }
    Ok(l)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pair() -> (Cipher, Cipher, String, String) {
        let (sk, pk) = new_identity().unwrap();
        let offer = HostOffer::new().unwrap();
        let signed = sign(&sk, &[&offer.p256[..], &offer.mlkem_ek[..]].concat()).unwrap();
        open_signed(&signed, &pk).unwrap();
        let (vp, ct, viewer) = answer(&signed, &offer.p256, &offer.mlkem_ek).unwrap();
        let host = offer.accept(&signed, &vp, &ct).unwrap();
        (host.host(), viewer.viewer(), host.security_code, viewer.security_code)
    }

    #[test]
    fn module_services_are_approved() {
        aws_lc_rs::try_fips_mode().unwrap();
        let lines = self_check().unwrap();
        println!("{}", lines.join("\n"));
        assert_eq!(lines.len(), 12, "{:?}", lines);
    }

    #[test]
    fn both_sides_agree_and_round_trip() {
        let (mut host, mut viewer, a, b) = pair();
        assert_eq!(a, b);
        assert_eq!(a.split_whitespace().count(), 16);
        for n in 0..5u8 {
            let payload = vec![n; 100];
            let mut wire = BytesMut::from(&host.enc(&payload).unwrap()[..]);
            assert_eq!(wire.len(), 100 + aead::NONCE_LEN + TAG_LEN);
            viewer.dec(&mut wire).unwrap();
            assert_eq!(&wire[..], &payload[..]);
            let mut wire = BytesMut::from(&viewer.enc(&payload).unwrap()[..]);
            host.dec(&mut wire).unwrap();
            assert_eq!(&wire[..], &payload[..]);
        }
    }

    #[test]
    fn replayed_reordered_and_tampered_frames_fail() {
        let (mut host, mut viewer, _, _) = pair();
        let first = host.enc(b"one").unwrap();
        let second = host.enc(b"two").unwrap();
        assert!(viewer.dec(&mut BytesMut::from(&second[..])).is_err());
        let (mut host, mut viewer, _, _) = pair();
        let first2 = host.enc(b"one").unwrap();
        viewer.dec(&mut BytesMut::from(&first2[..])).unwrap();
        assert!(viewer.dec(&mut BytesMut::from(&first2[..])).is_err());
        let (mut host, mut viewer, _, _) = pair();
        let mut bad = host.enc(b"one").unwrap();
        *bad.last_mut().unwrap() ^= 1;
        assert!(viewer.dec(&mut BytesMut::from(&bad[..])).is_err());
        assert!(viewer.dec(&mut BytesMut::from(&first[..1])).is_err());
    }

    #[test]
    fn directions_use_different_keys() {
        let (mut host, mut viewer, _, _) = pair();
        let wire = host.enc(b"same").unwrap();
        assert!(host.dec(&mut BytesMut::from(&wire[..])).is_err());
        viewer.dec(&mut BytesMut::from(&wire[..])).unwrap();
    }

    #[test]
    fn keys_change_at_the_epoch_boundary() {
        let (mut host, mut viewer, _, _) = pair();
        let edge = (1u64 << FRAMES_PER_KEY_BITS) - 2;
        host.tx.seq = edge;
        viewer.rx.seq = edge;
        for n in 0..4u8 {
            let mut wire = BytesMut::from(&host.enc(&[n; 8]).unwrap()[..]);
            viewer.dec(&mut wire).unwrap();
            assert_eq!(&wire[..], &[n; 8]);
        }
        assert_eq!(host.tx.key.as_ref().unwrap().0, 1);
        assert_ne!(expand(&host.tx.prk, H2V, 0).unwrap(), expand(&host.tx.prk, H2V, 1).unwrap());
    }

    #[test]
    fn a_tampered_offer_gives_different_keys() {
        let offer = HostOffer::new().unwrap();
        let (vp, ct, viewer) = answer(b"offer-a", &offer.p256, &offer.mlkem_ek).unwrap();
        let host = offer.accept(b"offer-b", &vp, &ct).unwrap();
        assert_ne!(host.security_code, viewer.security_code);
    }

    #[test]
    fn wrong_signature_and_lengths_are_refused() {
        let (sk, _) = new_identity().unwrap();
        let (_, other) = new_identity().unwrap();
        assert!(open_signed(&sign(&sk, b"x").unwrap(), &other).is_err());
        assert!(answer(b"o", &[4u8; 33], &[0u8; MLKEM_EK_LEN]).is_err());
        assert!(answer(b"o", &[4u8; P256_LEN], &[0u8; 800]).is_err());
    }
}
