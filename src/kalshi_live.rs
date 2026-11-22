//! Genuine Kalshi Trade API v2 REST client & RSA-PSS SHA-256 signer.
//!
//! Supports both Kalshi Production (`https://api.elections.kalshi.com/trade-api/v2`)
//! and Kalshi Demo (`https://demo-api.kalshi.co/trade-api/v2`), plus loopback HTTP
//! overrides for local mock integration testing.
//!
//! Signing follows Kalshi's exact v2 RSA-PSS SHA-256 scheme:
//!   message = "{timestamp_ms}{METHOD_UPPER}/trade-api/v2{path_without_query}"
//!   headers = KALSHI-ACCESS-KEY, KALSHI-ACCESS-TIMESTAMP, KALSHI-ACCESS-SIGNATURE
//!
//! Credentials are stored with `0600` permissions under `OPENBELL_DATA` and private
//! key bytes are never logged, echoed, or serialized into status payloads.

#![allow(dead_code)]

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

pub const KALSHI_PROD_BASE: &str = "https://api.elections.kalshi.com/trade-api/v2";
pub const KALSHI_DEMO_BASE: &str = "https://demo-api.kalshi.co/trade-api/v2";
const KALSHI_SIGN_PREFIX: &str = "/trade-api/v2";
const HTTP_TIMEOUT_SECS: u64 = 25;

// ---------------------------------------------------------------------------
// Environment, Config & Masked Credentials
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum KalshiEnv {
    #[default]
    Demo,
    Prod,
}

impl KalshiEnv {
    pub fn as_str(&self) -> &'static str {
        match self {
            KalshiEnv::Demo => "demo",
            KalshiEnv::Prod => "prod",
        }
    }

    pub fn default_base_url(&self) -> &'static str {
        match self {
            KalshiEnv::Demo => KALSHI_DEMO_BASE,
            KalshiEnv::Prod => KALSHI_PROD_BASE,
        }
    }

    pub fn parse_loose(s: &str) -> Option<Self> {
        match s.trim().to_ascii_lowercase().as_str() {
            "demo" | "sandbox" => Some(KalshiEnv::Demo),
            "prod" | "production" | "live" => Some(KalshiEnv::Prod),
            _ => None,
        }
    }
}

/// Mask an API key ID for UI/status display without leaking the full key.
pub fn mask_api_key_id(id: &str) -> Option<String> {
    let trimmed = id.trim();
    if trimmed.is_empty() {
        return None;
    }
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= 8 {
        return Some("****".to_string());
    }
    let prefix: String = chars[..4].iter().collect();
    let suffix: String = chars[chars.len() - 4..].iter().collect();
    Some(format!("{prefix}...{suffix}"))
}

/// Validate a Kalshi base URL override: must be `https://` for remote hosts;
/// plain `http://` is permitted ONLY for loopback (`127.0.0.1`, `localhost`, `[::1]`).
pub fn validate_api_base_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err("api_base_url cannot be empty".to_string());
    }
    if trimmed.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err("api_base_url must not contain whitespace or control characters".to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("https://") {
        let host = rest.split('/').next().unwrap_or("");
        if host.is_empty() {
            return Err("api_base_url is missing host".to_string());
        }
        return Ok(trimmed.to_string());
    }
    if let Some(rest) = trimmed.strip_prefix("http://") {
        let authority = rest.split('/').next().unwrap_or("").to_ascii_lowercase();
        let host = if authority.starts_with('[') {
            authority.split_inclusive(']').next().unwrap_or("")
        } else {
            authority.split(':').next().unwrap_or("")
        };
        if matches!(host, "127.0.0.1" | "localhost" | "[::1]") {
            return Ok(trimmed.to_string());
        }
        return Err(
            "plain http:// api_base_url is permitted only for loopback addresses; use https://"
                .to_string(),
        );
    }
    Err("api_base_url must start with https:// (or http:// on loopback)".to_string())
}

/// Validate a Kalshi order ID before interpolating into `DELETE /portfolio/orders/{id}`.
pub fn validate_order_id(id: &str) -> Result<&str, String> {
    let trimmed = id.trim();
    if trimmed.is_empty() || trimmed.len() > 128 {
        return Err("order id must be 1..=128 characters".to_string());
    }
    if !trimmed
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return Err("order id contains invalid characters".to_string());
    }
    Ok(trimmed)
}

/// Persisted live configuration stored at `OPENBELL_DATA/live_config.json` (`0600`).
/// Never embeds raw private key bytes — stores only `private_key_path`.
#[derive(Clone, Serialize, Deserialize)]
pub struct PersistedLiveConfig {
    #[serde(default)]
    pub api_key_id: String,
    #[serde(default)]
    pub private_key_path: String,
    #[serde(default)]
    pub environment: KalshiEnv,
    #[serde(default)]
    pub api_base_url: Option<String>,
}

impl Default for PersistedLiveConfig {
    fn default() -> Self {
        Self {
            api_key_id: String::new(),
            private_key_path: String::new(),
            environment: KalshiEnv::Demo,
            api_base_url: None,
        }
    }
}

impl fmt::Debug for PersistedLiveConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PersistedLiveConfig")
            .field("api_key_id", &mask_api_key_id(&self.api_key_id))
            .field(
                "private_key_path",
                &if self.private_key_path.is_empty() {
                    "<unset>"
                } else {
                    "<configured>"
                },
            )
            .field("environment", &self.environment)
            .field("api_base_url", &self.api_base_url)
            .finish()
    }
}

impl PersistedLiveConfig {
    pub fn effective_base_url(&self) -> String {
        if let Some(ref custom) = self.api_base_url {
            if let Ok(valid) = validate_api_base_url(custom) {
                return valid;
            }
        }
        self.environment.default_base_url().to_string()
    }

    pub fn is_configured(&self) -> bool {
        !self.api_key_id.trim().is_empty()
            && !self.private_key_path.trim().is_empty()
            && Path::new(self.private_key_path.trim()).is_file()
    }
}

/// Write a file atomically with `0600` permissions on Unix.
pub fn write_secret_file_0600(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("failed to create directory: {e}"))?;
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write as _;
        let mut opts = std::fs::OpenOptions::new();
        opts.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            opts.mode(0o600);
        }
        let mut f = opts
            .open(&tmp)
            .map_err(|e| format!("failed to create temp file: {e}"))?;
        f.write_all(bytes)
            .map_err(|e| format!("failed to write secret file: {e}"))?;
        let _ = f.sync_all();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("failed to move secret file into place: {e}")
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

pub fn live_config_path(data_dir: &Path) -> PathBuf {
    data_dir.join("live_config.json")
}

pub fn managed_private_key_path(data_dir: &Path) -> PathBuf {
    data_dir.join("kalshi_private_key.pem")
}

/// Load persisted live config from `OPENBELL_DATA/live_config.json`, falling back
/// cleanly to environment variables (`KALSHI_API_KEY_ID`, `KALSHI_PRIVATE_KEY_PATH`,
/// `KALSHI_ENV`, `KALSHI_API_BASE`) when no config file exists yet.
pub fn load_live_config(data_dir: &Path) -> PersistedLiveConfig {
    let path = live_config_path(data_dir);
    if let Ok(raw) = std::fs::read_to_string(&path) {
        if let Ok(cfg) = serde_json::from_str::<PersistedLiveConfig>(&raw) {
            return cfg;
        }
    }
    let api_key_id = std::env::var("KALSHI_API_KEY_ID").unwrap_or_default();
    let private_key_path = std::env::var("KALSHI_PRIVATE_KEY_PATH").unwrap_or_default();
    let environment = std::env::var("KALSHI_ENV")
        .ok()
        .and_then(|s| KalshiEnv::parse_loose(&s))
        .unwrap_or(KalshiEnv::Demo);
    let api_base_url = std::env::var("KALSHI_API_BASE")
        .ok()
        .and_then(|s| validate_api_base_url(&s).ok());
    PersistedLiveConfig {
        api_key_id,
        private_key_path,
        environment,
        api_base_url,
    }
}

/// Save live config atomically with `0600` permissions. If `private_key_pem` is
/// supplied, validates it parses as a real RSA private key first, writes it to
/// `OPENBELL_DATA/kalshi_private_key.pem` (`0600`), and points `private_key_path` there.
pub fn save_live_config(
    data_dir: &Path,
    mut cfg: PersistedLiveConfig,
    private_key_pem: Option<&str>,
) -> Result<PersistedLiveConfig, String> {
    if let Some(ref base) = cfg.api_base_url {
        let valid = validate_api_base_url(base)?;
        cfg.api_base_url = Some(valid);
    }
    if let Some(pem) = private_key_pem {
        let trimmed = pem.trim();
        if !trimmed.is_empty() {
            // Validate PEM cryptographically before touching disk.
            let _ = RsaSigningKey::from_pem(trimmed)?;
            let key_path = managed_private_key_path(data_dir);
            write_secret_file_0600(&key_path, trimmed.as_bytes())?;
            cfg.private_key_path = key_path.to_string_lossy().to_string();
        }
    } else if !cfg.private_key_path.trim().is_empty() {
        let p = Path::new(cfg.private_key_path.trim());
        let pem = std::fs::read_to_string(p)
            .map_err(|e| format!("unable to read private key file: {e}"))?;
        let _ = RsaSigningKey::from_pem(&pem)?;
    }
    let body = serde_json::to_vec_pretty(&cfg).map_err(|e| format!("serialize config: {e}"))?;
    write_secret_file_0600(&live_config_path(data_dir), &body)?;
    Ok(cfg)
}

// ---------------------------------------------------------------------------
// PEM/DER Parsing & RSA-PSS SHA-256 Signer
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq)]
enum PemKind {
    Pkcs8,
    Pkcs1,
}

fn decode_rsa_pem(pem: &str) -> Result<(PemKind, Vec<u8>), String> {
    let mut kind: Option<PemKind> = None;
    let mut b64 = String::new();
    let mut inside = false;
    let mut closed = false;
    for raw_line in pem.lines() {
        let line = raw_line.trim();
        if line == "-----BEGIN PRIVATE KEY-----" {
            kind = Some(PemKind::Pkcs8);
            inside = true;
            continue;
        }
        if line == "-----BEGIN RSA PRIVATE KEY-----" {
            kind = Some(PemKind::Pkcs1);
            inside = true;
            continue;
        }
        if line == "-----END PRIVATE KEY-----" || line == "-----END RSA PRIVATE KEY-----" {
            if inside {
                closed = true;
            }
            break;
        }
        if inside && !line.is_empty() {
            b64.push_str(line);
        }
    }
    let k = kind.ok_or_else(|| {
        "private key must be PEM-encoded PKCS#8 (PRIVATE KEY) or PKCS#1 (RSA PRIVATE KEY)"
            .to_string()
    })?;
    if !closed || b64.is_empty() {
        return Err("private key PEM is incomplete or empty".to_string());
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(b64.as_bytes())
        .map_err(|e| format!("invalid base64 in private key PEM: {e}"))?;
    Ok((k, der))
}

/// Pure-Rust RFC 8017 RSA-PSS SHA-256 implementation (used natively on Windows
/// so `x86_64-pc-windows-msvc` builds with zero C compiler dependencies, and
/// cross-verified against `ring::signature::RSA_PSS_2048_8192_SHA256` in tests).
#[cfg(any(target_os = "windows", test))]
mod pure_rsa_pss {
    use super::PemKind;
    use num_bigint::BigUint;
    use sha2::{Digest, Sha256};

    #[derive(Clone)]
    pub(super) struct PureRsaKey {
        n: BigUint,
        d: BigUint,
        p: BigUint,
        q: BigUint,
        dp: BigUint,
        dq: BigUint,
        qinv: BigUint,
        mod_bits: usize,
        mod_bytes: usize,
    }

    fn read_tlv(buf: &[u8], expected_tag: u8) -> Result<(&[u8], &[u8]), String> {
        if buf.is_empty() {
            return Err("unexpected end of DER input".to_string());
        }
        if buf[0] != expected_tag {
            return Err(format!(
                "unexpected DER tag 0x{:02x}, expected 0x{:02x}",
                buf[0], expected_tag
            ));
        }
        if buf.len() < 2 {
            return Err("truncated DER length".to_string());
        }
        let first_len = buf[1];
        let (len, header_len) = if first_len & 0x80 == 0 {
            (first_len as usize, 2usize)
        } else {
            let nbytes = (first_len & 0x7f) as usize;
            if nbytes == 0 || nbytes > 4 || buf.len() < 2 + nbytes {
                return Err("invalid DER length encoding".to_string());
            }
            let mut v = 0usize;
            for &b in &buf[2..2 + nbytes] {
                v = (v << 8) | (b as usize);
            }
            (v, 2 + nbytes)
        };
        if buf.len() < header_len + len {
            return Err("truncated DER payload".to_string());
        }
        Ok((&buf[header_len..header_len + len], &buf[header_len + len..]))
    }

    fn read_uint(buf: &[u8]) -> Result<(BigUint, &[u8]), String> {
        let (val_bytes, rest) = read_tlv(buf, 0x02)?;
        let trimmed = val_bytes.strip_prefix(&[0x00]).unwrap_or(val_bytes);
        Ok((BigUint::from_bytes_be(trimmed), rest))
    }

    impl PureRsaKey {
        pub(super) fn from_der(kind: PemKind, der: &[u8]) -> Result<Self, String> {
            let pkcs1_der: &[u8] = match kind {
                PemKind::Pkcs1 => der,
                PemKind::Pkcs8 => {
                    // PrivateKeyInfo ::= SEQUENCE { version INTEGER, alg SEQUENCE, privateKey OCTET STRING }
                    let (seq, _) = read_tlv(der, 0x30)?;
                    let (_, after_ver) = read_tlv(seq, 0x02)?;
                    let (_, after_alg) = read_tlv(after_ver, 0x30)?;
                    let (priv_octets, _) = read_tlv(after_alg, 0x04)?;
                    priv_octets
                }
            };
            // RSAPrivateKey ::= SEQUENCE { version, n, e, d, p, q, dp, dq, qinv }
            let (seq, _) = read_tlv(pkcs1_der, 0x30)?;
            let (_ver, r) = read_uint(seq)?;
            let (n, r) = read_uint(r)?;
            let (_e, r) = read_uint(r)?;
            let (d, r) = read_uint(r)?;
            let (p, r) = read_uint(r)?;
            let (q, r) = read_uint(r)?;
            let (dp, r) = read_uint(r)?;
            let (dq, r) = read_uint(r)?;
            let (qinv, _) = read_uint(r)?;

            let mod_bits = n.bits() as usize;
            if mod_bits < 1024 {
                return Err(format!("RSA key modulus too small ({mod_bits} bits; need >= 2048)"));
            }
            let mod_bytes = mod_bits.div_ceil(8);
            Ok(Self {
                n,
                d,
                p,
                q,
                dp,
                dq,
                qinv,
                mod_bits,
                mod_bytes,
            })
        }

        fn rsasp1(&self, m: &BigUint) -> BigUint {
            let zero = BigUint::from(0u8);
            if self.p > zero && self.q > zero && self.dp > zero && self.dq > zero {
                let m1 = (m % &self.p).modpow(&self.dp, &self.p);
                let m2 = (m % &self.q).modpow(&self.dq, &self.q);
                let m2_mod_p = &m2 % &self.p;
                let diff = if m1 >= m2_mod_p {
                    m1 - m2_mod_p
                } else {
                    (&self.p + m1) - m2_mod_p
                };
                let h = (&self.qinv * diff) % &self.p;
                m2 + h * &self.q
            } else {
                m.modpow(&self.d, &self.n)
            }
        }

        /// RFC 8017 Section 8.1.1 RSASSA-PSS-SIGN with SHA-256, MGF1-SHA-256, salt length 32.
        pub(super) fn sign_pss_sha256(&self, msg: &[u8]) -> Result<Vec<u8>, String> {
            let mut salt = [0u8; 32];
            getrandom::getrandom(&mut salt)
                .map_err(|e| format!("CSPRNG failure while generating PSS salt: {e}"))?;
            self.sign_pss_sha256_with_salt(msg, &salt)
        }

        fn sign_pss_sha256_with_salt(&self, msg: &[u8], salt: &[u8; 32]) -> Result<Vec<u8>, String> {
            let em_bits = self.mod_bits - 1;
            let em_len = em_bits.div_ceil(8);
            let h_len = 32usize;
            let s_len = 32usize;
            if em_len < h_len + s_len + 2 {
                return Err("RSA modulus too short for SHA-256 PSS".to_string());
            }

            // 1. mHash = SHA-256(msg)
            let m_hash = Sha256::digest(msg);

            // 2. M' = 0x00*8 || mHash || salt
            let mut m_prime = Vec::with_capacity(8 + h_len + s_len);
            m_prime.extend_from_slice(&[0u8; 8]);
            m_prime.extend_from_slice(&m_hash);
            m_prime.extend_from_slice(salt);

            // 3. H = SHA-256(M')
            let h = Sha256::digest(&m_prime);

            // 4. DB = PS || 0x01 || salt (length = em_len - h_len - 1)
            let db_len = em_len - h_len - 1;
            let ps_len = db_len - s_len - 1;
            let mut db = Vec::with_capacity(db_len);
            db.resize(ps_len, 0u8);
            db.push(0x01);
            db.extend_from_slice(salt);

            // 5. dbMask = MGF1-SHA-256(H, db_len)
            let mut db_mask = Vec::with_capacity(db_len + 32);
            let mut counter: u32 = 0;
            while db_mask.len() < db_len {
                let mut hasher = Sha256::new();
                hasher.update(h);
                hasher.update(counter.to_be_bytes());
                db_mask.extend_from_slice(&hasher.finalize());
                counter += 1;
            }
            db_mask.truncate(db_len);

            // 6. maskedDB = DB XOR dbMask, clear leftmost unused bits
            let mut masked_db = vec![0u8; db_len];
            for i in 0..db_len {
                masked_db[i] = db[i] ^ db_mask[i];
            }
            let unused_bits = 8 * em_len - em_bits;
            if unused_bits > 0 {
                masked_db[0] &= 0xFFu8 >> unused_bits;
            }

            // 7. EM = maskedDB || H || 0xBC
            let mut em = Vec::with_capacity(em_len);
            em.extend_from_slice(&masked_db);
            em.extend_from_slice(&h);
            em.push(0xBC);

            // 8. s = RSASP1(K, OS2IP(EM)), S = I2OSP(s, mod_bytes)
            let m_int = BigUint::from_bytes_be(&em);
            let s_int = self.rsasp1(&m_int);
            let raw_sig = s_int.to_bytes_be();
            if raw_sig.len() > self.mod_bytes {
                return Err("internal RSA signature length overflow".to_string());
            }
            let mut out = vec![0u8; self.mod_bytes - raw_sig.len()];
            out.extend_from_slice(&raw_sig);
            Ok(out)
        }
    }
}

/// Redacted RSA signing key wrapper (uses `ring` on Unix/macOS and pure-Rust
/// RFC 8017 RSA-PSS on Windows; private key bytes are never printed in Debug).
pub struct RsaSigningKey {
    #[cfg(not(target_os = "windows"))]
    inner: ring::signature::RsaKeyPair,
    #[cfg(target_os = "windows")]
    inner: pure_rsa_pss::PureRsaKey,
}

impl fmt::Debug for RsaSigningKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("RsaSigningKey([REDACTED])")
    }
}

impl RsaSigningKey {
    pub fn from_pem(pem: &str) -> Result<Self, String> {
        let (kind, der) = decode_rsa_pem(pem)?;
        #[cfg(not(target_os = "windows"))]
        {
            let pair = match kind {
                PemKind::Pkcs8 => ring::signature::RsaKeyPair::from_pkcs8(&der)
                    .map_err(|_| "invalid PKCS#8 RSA private key".to_string())?,
                PemKind::Pkcs1 => ring::signature::RsaKeyPair::from_der(&der)
                    .map_err(|_| "invalid PKCS#1 RSA private key".to_string())?,
            };
            Ok(Self { inner: pair })
        }
        #[cfg(target_os = "windows")]
        {
            let key = pure_rsa_pss::PureRsaKey::from_der(kind, &der)?;
            Ok(Self { inner: key })
        }
    }

    pub fn from_file(path: &Path) -> Result<Self, String> {
        let pem = std::fs::read_to_string(path)
            .map_err(|e| format!("unable to read private key file: {e}"))?;
        Self::from_pem(&pem)
    }

    /// Produce a base64-encoded RSA-PSS SHA-256 signature over `message`.
    pub fn sign_base64(&self, message: &str) -> Result<String, String> {
        #[cfg(not(target_os = "windows"))]
        {
            let rng = ring::rand::SystemRandom::new();
            let mut sig = vec![0u8; self.inner.public().modulus_len()];
            self.inner
                .sign(
                    &ring::signature::RSA_PSS_SHA256,
                    &rng,
                    message.as_bytes(),
                    &mut sig,
                )
                .map_err(|_| "RSA-PSS SHA-256 signing failed".to_string())?;
            Ok(base64::engine::general_purpose::STANDARD.encode(&sig))
        }
        #[cfg(target_os = "windows")]
        {
            let sig = self.inner.sign_pss_sha256(message.as_bytes())?;
            Ok(base64::engine::general_purpose::STANDARD.encode(&sig))
        }
    }
}

/// Build the exact Kalshi v2 signing payload:
/// `timestamp_ms + METHOD + "/trade-api/v2" + path_without_query`.
pub fn build_signing_message(timestamp_ms: &str, method: &str, rel_path: &str) -> String {
    let path_no_query = rel_path.split('?').next().unwrap_or(rel_path);
    let normalized = if path_no_query.starts_with(KALSHI_SIGN_PREFIX) {
        path_no_query.to_string()
    } else if path_no_query.starts_with('/') {
        format!("{KALSHI_SIGN_PREFIX}{path_no_query}")
    } else {
        format!("{KALSHI_SIGN_PREFIX}/{path_no_query}")
    };
    format!(
        "{}{}{}",
        timestamp_ms.trim(),
        method.trim().to_ascii_uppercase(),
        normalized
    )
}

fn now_millis_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

// ---------------------------------------------------------------------------
// Kalshi v2 Typed Domain Models
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiBalance {
    pub balance_cents: i64,
    pub balance_dollars: f64,
    pub portfolio_value_cents: i64,
    pub portfolio_value_dollars: f64,
    pub payout_cents: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiPosition {
    pub ticker: String,
    pub position: i64,
    pub market_exposure_cents: i64,
    pub market_exposure_dollars: f64,
    pub realized_pnl_cents: i64,
    pub realized_pnl_dollars: f64,
    pub resting_orders_count: i64,
    pub fees_paid_dollars: f64,
    pub total_traded_dollars: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiOrder {
    pub order_id: String,
    pub client_order_id: Option<String>,
    pub ticker: String,
    pub status: String,
    pub action: String,
    pub side: String,
    #[serde(rename = "type")]
    pub order_type: String,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub initial_count: i64,
    pub remaining_count: i64,
    pub fill_count: i64,
    pub created_time: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiFill {
    pub trade_id: String,
    pub order_id: String,
    pub ticker: String,
    pub action: String,
    pub side: String,
    pub count: i64,
    pub yes_price: Option<i64>,
    pub no_price: Option<i64>,
    pub is_taker: Option<bool>,
    pub created_time: Option<String>,
}

/// Request payload for `POST /portfolio/orders` on Kalshi Trade API v2.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KalshiOrderRequest {
    pub client_order_id: String,
    pub ticker: String,
    pub action: String,
    pub side: String,
    pub count: u32,
    #[serde(rename = "type")]
    pub order_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub yes_price: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub no_price: Option<u8>,
}

impl KalshiOrderRequest {
    /// Construct a validated whole-cent limit order request (`price_cents` in `1..=99`).
    pub fn limit(
        ticker: &str,
        action: &str,
        side: &str,
        count: u32,
        price_cents: u8,
        client_order_id: Option<String>,
    ) -> Result<Self, String> {
        let ticker = ticker.trim();
        if ticker.is_empty()
            || !ticker
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err("invalid ticker format".to_string());
        }
        let action = action.trim().to_ascii_lowercase();
        if action != "buy" && action != "sell" {
            return Err(format!("action must be \"buy\" or \"sell\", got \"{action}\""));
        }
        let side = side.trim().to_ascii_lowercase();
        if side != "yes" && side != "no" {
            return Err(format!("side must be \"yes\" or \"no\", got \"{side}\""));
        }
        if count == 0 {
            return Err("count must be >= 1".to_string());
        }
        if !(1..=99).contains(&price_cents) {
            return Err(format!("price_cents must be 1..=99, got {price_cents}"));
        }
        let cid = match client_order_id {
            Some(c) if !c.trim().is_empty() => c.trim().to_string(),
            _ => {
                let mut rand_bytes = [0u8; 6];
                let _ = getrandom::getrandom(&mut rand_bytes);
                let hex: String = rand_bytes.iter().map(|b| format!("{b:02x}")).collect();
                format!("ob-{}-{hex}", now_millis_string())
            }
        };
        let (yes_price, no_price) = if side == "yes" {
            (Some(price_cents), None)
        } else {
            (None, Some(price_cents))
        };
        Ok(Self {
            client_order_id: cid,
            ticker: ticker.to_string(),
            action,
            side,
            count,
            order_type: "limit".to_string(),
            yes_price,
            no_price,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CancelAllReport {
    pub attempted: usize,
    pub cancelled_count: usize,
    pub cancelled_ids: Vec<String>,
    pub errors: Vec<String>,
}

// ---------------------------------------------------------------------------
// Kalshi v2 REST Client
// ---------------------------------------------------------------------------

pub struct KalshiLiveClient {
    api_key_id: String,
    base_url: String,
    environment: KalshiEnv,
    signer: RsaSigningKey,
    agent: ureq::Agent,
}

impl fmt::Debug for KalshiLiveClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KalshiLiveClient")
            .field("api_key_id", &mask_api_key_id(&self.api_key_id))
            .field("base_url", &self.base_url)
            .field("environment", &self.environment)
            .field("signer", &self.signer)
            .finish()
    }
}

impl KalshiLiveClient {
    pub fn from_config(cfg: &PersistedLiveConfig) -> Result<Self, String> {
        let key_id = cfg.api_key_id.trim();
        if key_id.is_empty() {
            return Err("KALSHI API key ID is not configured".to_string());
        }
        let key_path = cfg.private_key_path.trim();
        if key_path.is_empty() {
            return Err("KALSHI private key path is not configured".to_string());
        }
        let signer = RsaSigningKey::from_file(Path::new(key_path))?;
        let base_url = validate_api_base_url(&cfg.effective_base_url())?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build();
        Ok(Self {
            api_key_id: key_id.to_string(),
            base_url,
            environment: cfg.environment,
            signer,
            agent,
        })
    }

    pub fn from_parts(
        api_key_id: &str,
        private_key_pem: &str,
        environment: KalshiEnv,
        base_url: &str,
    ) -> Result<Self, String> {
        let key_id = api_key_id.trim();
        if key_id.is_empty() {
            return Err("api_key_id cannot be empty".to_string());
        }
        let signer = RsaSigningKey::from_pem(private_key_pem)?;
        let valid_base = validate_api_base_url(base_url)?;
        let agent = ureq::AgentBuilder::new()
            .timeout(Duration::from_secs(HTTP_TIMEOUT_SECS))
            .build();
        Ok(Self {
            api_key_id: key_id.to_string(),
            base_url: valid_base,
            environment,
            signer,
            agent,
        })
    }

    pub fn environment(&self) -> KalshiEnv {
        self.environment
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Execute one signed Kalshi v2 REST request and return parsed JSON.
    fn request_json(
        &self,
        method: &str,
        rel_path: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        let ts = now_millis_string();
        let signing_msg = build_signing_message(&ts, method, rel_path);
        let sig = self.signer.sign_base64(&signing_msg)?;
        let path_part = if rel_path.starts_with('/') {
            rel_path.to_string()
        } else {
            format!("/{rel_path}")
        };
        let full_url = format!("{}{}", self.base_url.trim_end_matches('/'), path_part);

        #[cfg(target_os = "windows")]
        if full_url.starts_with("https://") {
            return self.request_https_via_curl_windows(method, &full_url, &ts, &sig, body);
        }

        let mut req = self
            .agent
            .request(method, &full_url)
            .set("KALSHI-ACCESS-KEY", &self.api_key_id)
            .set("KALSHI-ACCESS-TIMESTAMP", &ts)
            .set("KALSHI-ACCESS-SIGNATURE", &sig)
            .set("Accept", "application/json");

        let resp = match body {
            Some(payload) => {
                req = req.set("Content-Type", "application/json");
                req.send_json(payload.clone())
            }
            None => req.call(),
        };

        match resp {
            Ok(ok_resp) => {
                let text = ok_resp
                    .into_string()
                    .map_err(|e| format!("failed reading Kalshi response body: {e}"))?;
                if text.trim().is_empty() {
                    Ok(serde_json::json!({}))
                } else {
                    serde_json::from_str(&text)
                        .map_err(|e| format!("Kalshi returned non-JSON body: {e}"))
                }
            }
            Err(ureq::Error::Status(code, err_resp)) => {
                let msg = err_resp.into_string().unwrap_or_default();
                let snippet: String = msg.chars().take(240).collect();
                Err(format!("Kalshi HTTP {code}: {snippet}"))
            }
            Err(ureq::Error::Transport(t)) => Err(format!("Kalshi transport error: {t}")),
        }
    }

    /// Windows HTTPS fallback using built-in `curl.exe` with `--config -` stdin so
    /// auth headers and signatures never appear in process command-line arguments.
    #[cfg(target_os = "windows")]
    fn request_https_via_curl_windows(
        &self,
        method: &str,
        full_url: &str,
        ts: &str,
        sig: &str,
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, String> {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let esc = |s: &str| s.replace('\\', "\\\\").replace('"', "\\\"");
        let mut cfg = String::new();
        cfg.push_str(&format!("url = \"{}\"\n", esc(full_url)));
        cfg.push_str(&format!("request = \"{}\"\n", esc(method)));
        cfg.push_str(&format!("header = \"KALSHI-ACCESS-KEY: {}\"\n", esc(&self.api_key_id)));
        cfg.push_str(&format!("header = \"KALSHI-ACCESS-TIMESTAMP: {}\"\n", esc(ts)));
        cfg.push_str(&format!("header = \"KALSHI-ACCESS-SIGNATURE: {}\"\n", esc(sig)));
        cfg.push_str("header = \"Accept: application/json\"\n");
        if let Some(payload) = body {
            let body_str = serde_json::to_string(payload).map_err(|e| e.to_string())?;
            cfg.push_str("header = \"Content-Type: application/json\"\n");
            cfg.push_str(&format!("data-raw = \"{}\"\n", esc(&body_str)));
        }

        let mut child = Command::new("curl.exe")
            .args(["-q", "-sS", "-m", "25", "-w", "\n%{http_code}", "--config", "-"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("failed spawning curl.exe for HTTPS: {e}"))?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(cfg.as_bytes())
                .map_err(|e| format!("failed writing curl config: {e}"))?;
        }
        let out = child
            .wait_with_output()
            .map_err(|e| format!("curl.exe failed: {e}"))?;
        if !out.status.success() {
            let err = String::from_utf8_lossy(&out.stderr);
            return Err(format!("Kalshi HTTPS curl error: {}", err.trim()));
        }
        let stdout = String::from_utf8_lossy(&out.stdout);
        let (body_part, status_part) = stdout
            .rsplit_once('\n')
            .unwrap_or((stdout.as_ref(), "200"));
        let status: u16 = status_part.trim().parse().unwrap_or(502);
        if !(200..300).contains(&status) {
            let snippet: String = body_part.chars().take(240).collect();
            return Err(format!("Kalshi HTTP {status}: {snippet}"));
        }
        if body_part.trim().is_empty() {
            Ok(serde_json::json!({}))
        } else {
            serde_json::from_str(body_part)
                .map_err(|e| format!("Kalshi returned non-JSON body: {e}"))
        }
    }

    pub fn get_balance(&self) -> Result<KalshiBalance, String> {
        let val = self.request_json("GET", "/portfolio/balance", None)?;
        let bal_c = val.get("balance").and_then(|v| v.as_i64()).unwrap_or(0);
        let port_c = val
            .get("portfolio_value")
            .and_then(|v| v.as_i64())
            .unwrap_or(bal_c);
        let payout_c = val.get("payout").and_then(|v| v.as_i64());
        Ok(KalshiBalance {
            balance_cents: bal_c,
            balance_dollars: bal_c as f64 / 100.0,
            portfolio_value_cents: port_c,
            portfolio_value_dollars: port_c as f64 / 100.0,
            payout_cents: payout_c,
        })
    }

    pub fn get_positions(&self) -> Result<Vec<KalshiPosition>, String> {
        let val = self.request_json("GET", "/portfolio/positions", None)?;
        let rows = val
            .get("market_positions")
            .or_else(|| val.get("positions"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let ticker = r
                .get("ticker")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if ticker.is_empty() {
                continue;
            }
            let position = r.get("position").and_then(|v| v.as_i64()).unwrap_or(0);
            let exp_c = r
                .get("market_exposure")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let exp_d = parse_dollars_or_cents(&r, "market_exposure_dollars", exp_c);
            let rpnl_c = r.get("realized_pnl").and_then(|v| v.as_i64()).unwrap_or(0);
            let rpnl_d = parse_dollars_or_cents(&r, "realized_pnl_dollars", rpnl_c);
            let resting = r
                .get("resting_orders_count")
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let fees_c = r.get("fees_paid").and_then(|v| v.as_i64()).unwrap_or(0);
            let fees_d = parse_dollars_or_cents(&r, "fees_paid_dollars", fees_c);
            let traded_c = r.get("total_traded").and_then(|v| v.as_i64()).unwrap_or(0);
            let traded_d = parse_dollars_or_cents(&r, "total_traded_dollars", traded_c);
            out.push(KalshiPosition {
                ticker,
                position,
                market_exposure_cents: exp_c,
                market_exposure_dollars: exp_d,
                realized_pnl_cents: rpnl_c,
                realized_pnl_dollars: rpnl_d,
                resting_orders_count: resting,
                fees_paid_dollars: fees_d,
                total_traded_dollars: traded_d,
            });
        }
        Ok(out)
    }

    pub fn get_orders(&self, status_filter: Option<&str>) -> Result<Vec<KalshiOrder>, String> {
        let path = match status_filter {
            Some(s) if !s.trim().is_empty() => {
                format!("/portfolio/orders?status={}", s.trim())
            }
            _ => "/portfolio/orders".to_string(),
        };
        let val = self.request_json("GET", &path, None)?;
        let rows = val
            .get("orders")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(rows.iter().filter_map(parse_kalshi_order).collect())
    }

    pub fn get_fills(&self, limit: Option<usize>) -> Result<Vec<KalshiFill>, String> {
        let path = match limit {
            Some(n) if n > 0 => format!("/portfolio/fills?limit={}", n.min(200)),
            _ => "/portfolio/fills".to_string(),
        };
        let val = self.request_json("GET", &path, None)?;
        let rows = val
            .get("fills")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::with_capacity(rows.len());
        for r in rows {
            let trade_id = r
                .get("trade_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let order_id = r
                .get("order_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let ticker = r
                .get("ticker")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if ticker.is_empty() && order_id.is_empty() {
                continue;
            }
            out.push(KalshiFill {
                trade_id,
                order_id,
                ticker,
                action: r
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                side: r
                    .get("side")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
                count: r.get("count").and_then(|v| v.as_i64()).unwrap_or(0),
                yes_price: r.get("yes_price").and_then(|v| v.as_i64()),
                no_price: r.get("no_price").and_then(|v| v.as_i64()),
                is_taker: r.get("is_taker").and_then(|v| v.as_bool()),
                created_time: r
                    .get("created_time")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
            });
        }
        Ok(out)
    }

    pub fn place_order(&self, req: &KalshiOrderRequest) -> Result<KalshiOrder, String> {
        let payload = serde_json::to_value(req).map_err(|e| format!("serialize order: {e}"))?;
        let val = self.request_json("POST", "/portfolio/orders", Some(&payload))?;
        let obj = val.get("order").unwrap_or(&val);
        parse_kalshi_order(obj)
            .ok_or_else(|| "Kalshi order response missing order object".to_string())
    }

    pub fn cancel_order(&self, order_id: &str) -> Result<serde_json::Value, String> {
        let safe_id = validate_order_id(order_id)?;
        let path = format!("/portfolio/orders/{safe_id}");
        self.request_json("DELETE", &path, None)
    }

    /// Fetch all resting orders (`status=resting`) and cancel every one, continuing
    /// past per-order errors so one failed cancel never stops the kill switch.
    pub fn cancel_all_orders(&self) -> CancelAllReport {
        let resting = match self.get_orders(Some("resting")) {
            Ok(list) => list,
            Err(e) => {
                return CancelAllReport {
                    attempted: 0,
                    cancelled_count: 0,
                    cancelled_ids: Vec::new(),
                    errors: vec![format!("failed listing resting orders: {e}")],
                };
            }
        };
        let mut cancelled_ids = Vec::new();
        let mut errors = Vec::new();
        for ord in &resting {
            match self.cancel_order(&ord.order_id) {
                Ok(_) => cancelled_ids.push(ord.order_id.clone()),
                Err(e) => errors.push(format!("{}: {e}", ord.order_id)),
            }
        }
        CancelAllReport {
            attempted: resting.len(),
            cancelled_count: cancelled_ids.len(),
            cancelled_ids,
            errors,
        }
    }
}

fn parse_dollars_or_cents(v: &serde_json::Value, dollars_key: &str, cents_fallback: i64) -> f64 {
    if let Some(d) = v
        .get(dollars_key)
        .and_then(|x| x.as_f64().or_else(|| x.as_str().and_then(|s| s.parse().ok())))
    {
        if d.is_finite() {
            return d;
        }
    }
    cents_fallback as f64 / 100.0
}

fn parse_kalshi_order(v: &serde_json::Value) -> Option<KalshiOrder> {
    let order_id = v.get("order_id").and_then(|x| x.as_str())?.to_string();
    let ticker = v
        .get("ticker")
        .and_then(|x| x.as_str())
        .unwrap_or("")
        .to_string();
    Some(KalshiOrder {
        order_id,
        client_order_id: v
            .get("client_order_id")
            .and_then(|x| x.as_str())
            .map(ToString::to_string),
        ticker,
        status: v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("resting")
            .to_string(),
        action: v
            .get("action")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        side: v
            .get("side")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string(),
        order_type: v
            .get("type")
            .and_then(|x| x.as_str())
            .unwrap_or("limit")
            .to_string(),
        yes_price: v.get("yes_price").and_then(|x| x.as_i64()),
        no_price: v.get("no_price").and_then(|x| x.as_i64()),
        initial_count: v
            .get("initial_count")
            .or_else(|| v.get("count"))
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        remaining_count: v
            .get("remaining_count")
            .and_then(|x| x.as_i64())
            .unwrap_or(0),
        fill_count: v.get("fill_count").and_then(|x| x.as_i64()).unwrap_or(0),
        created_time: v
            .get("created_time")
            .and_then(|x| x.as_str())
            .map(ToString::to_string),
    })
}

// ---------------------------------------------------------------------------
// Comprehensive Cryptographic & Local Mock Kalshi v2 Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    /// Generate an ephemeral 2048-bit RSA test keypair via `openssl` (returns PKCS#8 PEM,
    /// PKCS#1 PEM, and SPKI public key DER bytes). Zero hardcoded keys in repo!
    pub(crate) fn generate_ephemeral_test_rsa() -> (String, String, Vec<u8>) {
        use std::process::Command;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQ: AtomicU64 = AtomicU64::new(1);
        let id = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ob-rsa-{}-{}-{}",
            std::process::id(),
            now_millis_string(),
            id
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let pkcs8_path = dir.join("key_pkcs8.pem");
        let pkcs1_path = dir.join("key_pkcs1.pem");
        let pub_der_path = dir.join("pub.der");

        let st1 = Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "RSA",
                "-pkeyopt",
                "rsa_keygen_bits:2048",
                "-out",
                pkcs8_path.to_str().unwrap(),
            ])
            .output()
            .expect("openssl genpkey");
        assert!(st1.status.success(), "openssl genpkey failed");

        let st2 = Command::new("openssl")
            .args([
                "rsa",
                "-in",
                pkcs8_path.to_str().unwrap(),
                "-traditional",
                "-out",
                pkcs1_path.to_str().unwrap(),
            ])
            .output()
            .expect("openssl rsa -traditional");
        assert!(st2.status.success(), "openssl pkcs1 convert failed");

        // Extract raw PKCS#1 RSAPublicKey DER (ring's UnparsedPublicKey expects RSAPublicKey DER).
        let st3 = Command::new("openssl")
            .args([
                "rsa",
                "-in",
                pkcs8_path.to_str().unwrap(),
                "-RSAPublicKey_out",
                "-outform",
                "DER",
                "-out",
                pub_der_path.to_str().unwrap(),
            ])
            .output()
            .expect("openssl RSAPublicKey_out");
        assert!(st3.status.success(), "openssl pubkey der failed");

        let pkcs8 = std::fs::read_to_string(&pkcs8_path).unwrap();
        let pkcs1 = std::fs::read_to_string(&pkcs1_path).unwrap();
        let pub_der = std::fs::read(&pub_der_path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        (pkcs8, pkcs1, pub_der)
    }

    #[test]
    fn signing_message_format_and_cross_verified_rsa_pss_sha256() {
        assert_eq!(
            build_signing_message("1700000000123", "get", "/portfolio/orders?status=resting"),
            "1700000000123GET/trade-api/v2/portfolio/orders"
        );
        assert_eq!(
            build_signing_message("1700000000123", "POST", "/trade-api/v2/portfolio/orders"),
            "1700000000123POST/trade-api/v2/portfolio/orders"
        );

        let (pkcs8, pkcs1, pub_der) = generate_ephemeral_test_rsa();
        let msg = build_signing_message("1765000000999", "POST", "/portfolio/orders");

        // Verify ring signer on BOTH PKCS#8 and PKCS#1 PEM formats.
        let k8 = RsaSigningKey::from_pem(&pkcs8).unwrap();
        let k1 = RsaSigningKey::from_pem(&pkcs1).unwrap();
        assert_eq!(format!("{k8:?}"), "RsaSigningKey([REDACTED])");

        let sig8_b64 = k8.sign_base64(&msg).unwrap();
        let sig1_b64 = k1.sign_base64(&msg).unwrap();

        // Also cross-verify our pure-Rust Windows RFC 8017 signer on BOTH PKCS#8 and PKCS#1!
        let (kind8, der8) = decode_rsa_pem(&pkcs8).unwrap();
        let (kind1, der1) = decode_rsa_pem(&pkcs1).unwrap();
        let pure8 = pure_rsa_pss::PureRsaKey::from_der(kind8, &der8).unwrap();
        let pure1 = pure_rsa_pss::PureRsaKey::from_der(kind1, &der1).unwrap();
        let pure_sig8 = pure8.sign_pss_sha256(msg.as_bytes()).unwrap();
        let pure_sig1 = pure1.sign_pss_sha256(msg.as_bytes()).unwrap();

        #[cfg(not(target_os = "windows"))]
        {
            let pubkey = ring::signature::UnparsedPublicKey::new(
                &ring::signature::RSA_PSS_2048_8192_SHA256,
                &pub_der,
            );
            let raw8 = base64::engine::general_purpose::STANDARD
                .decode(&sig8_b64)
                .unwrap();
            let raw1 = base64::engine::general_purpose::STANDARD
                .decode(&sig1_b64)
                .unwrap();
            pubkey.verify(msg.as_bytes(), &raw8).expect("ring PKCS#8 sig valid");
            pubkey.verify(msg.as_bytes(), &raw1).expect("ring PKCS#1 sig valid");
            pubkey
                .verify(msg.as_bytes(), &pure_sig8)
                .expect("pure-Rust Windows PKCS#8 sig verified by ring!");
            pubkey
                .verify(msg.as_bytes(), &pure_sig1)
                .expect("pure-Rust Windows PKCS#1 sig verified by ring!");
            // Negative control: tampered message MUST fail verification.
            assert!(pubkey.verify(b"tampered", &pure_sig8).is_err());
        }
    }

    #[test]
    fn credential_persistence_0600_and_url_validation() {
        assert!(validate_api_base_url("https://demo-api.kalshi.co/trade-api/v2").is_ok());
        assert!(validate_api_base_url("http://127.0.0.1:19999/trade-api/v2").is_ok());
        assert!(validate_api_base_url("http://evil.example.com/trade-api/v2").is_err());
        assert!(validate_order_id("ord-123_abc").is_ok());
        assert!(validate_order_id("../other").is_err());
        assert_eq!(mask_api_key_id("kls_live_1234567890abcd").as_deref(), Some("kls_...abcd"));
        assert_eq!(mask_api_key_id("short").as_deref(), Some("****"));

        let (pkcs8, _, _) = generate_ephemeral_test_rsa();
        let dir = std::env::temp_dir().join(format!("ob-live-cfg-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let cfg = PersistedLiveConfig {
            api_key_id: "test_key_12345678".to_string(),
            private_key_path: String::new(),
            environment: KalshiEnv::Demo,
            api_base_url: None,
        };
        let saved = save_live_config(&dir, cfg, Some(&pkcs8)).unwrap();
        assert!(saved.is_configured());
        let raw_json = std::fs::read_to_string(live_config_path(&dir)).unwrap();
        assert!(
            !raw_json.contains("BEGIN PRIVATE KEY"),
            "live_config.json must never embed raw private key PEM"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let m_cfg = std::fs::metadata(live_config_path(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            let m_key = std::fs::metadata(managed_private_key_path(&dir))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(m_cfg, 0o600);
            assert_eq!(m_key, 0o600);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn mock_kalshi_v2_end_to_end_signed_rest_and_cancel_all() {
        let (pkcs8, _, pub_der) = generate_ephemeral_test_rsa();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();

        let state = Arc::new(Mutex::new(vec![
            serde_json::json!({
                "order_id": "ord-existing-1",
                "client_order_id": "cid-0",
                "ticker": "KXBTC15M-TEST",
                "status": "resting",
                "action": "buy",
                "side": "yes",
                "type": "limit",
                "yes_price": 62,
                "initial_count": 5,
                "remaining_count": 5,
                "fill_count": 0
            }),
        ]));
        let pub_der_srv = pub_der.clone();
        let state_srv = state.clone();

        // Spawn local mock Kalshi v2 server that cryptographically verifies EVERY
        // incoming KALSHI-ACCESS-SIGNATURE header before serving responses.
        let handle = std::thread::spawn(move || {
            for stream in listener.incoming().take(7) {
                let Ok(mut stream) = stream else { break };
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut req_line = String::new();
                if reader.read_line(&mut req_line).unwrap_or(0) == 0 {
                    continue;
                }
                let mut parts = req_line.split_whitespace();
                let method = parts.next().unwrap_or("").to_string();
                let path = parts.next().unwrap_or("").to_string();
                let mut headers = std::collections::HashMap::new();
                let mut content_len = 0usize;
                loop {
                    let mut line = String::new();
                    if reader.read_line(&mut line).unwrap_or(0) == 0 || line == "\r\n" {
                        break;
                    }
                    if let Some((k, v)) = line.split_once(':') {
                        let kl = k.trim().to_ascii_lowercase();
                        let val = v.trim().to_string();
                        if kl == "content-length" {
                            content_len = val.parse().unwrap_or(0);
                        }
                        headers.insert(kl, val);
                    }
                }
                let mut body = vec![0u8; content_len];
                if content_len > 0 {
                    let _ = reader.read_exact(&mut body);
                }

                // Cryptographically verify Kalshi v2 RSA-PSS SHA-256 signature!
                let key = headers.get("kalshi-access-key").cloned().unwrap_or_default();
                let ts = headers
                    .get("kalshi-access-timestamp")
                    .cloned()
                    .unwrap_or_default();
                let sig_b64 = headers
                    .get("kalshi-access-signature")
                    .cloned()
                    .unwrap_or_default();
                assert_eq!(key, "mock-key-id-9999");
                let expected_msg = build_signing_message(&ts, &method, &path);
                #[cfg(not(target_os = "windows"))]
                {
                    let sig_raw = base64::engine::general_purpose::STANDARD
                        .decode(&sig_b64)
                        .expect("base64 sig");
                    let pk = ring::signature::UnparsedPublicKey::new(
                        &ring::signature::RSA_PSS_2048_8192_SHA256,
                        &pub_der_srv,
                    );
                    pk.verify(expected_msg.as_bytes(), &sig_raw)
                        .expect("mock server verified RSA-PSS signature");
                }

                let path_no_q = path.split('?').next().unwrap_or(&path);
                let (status, resp_json) = match (method.as_str(), path_no_q) {
                    ("GET", "/trade-api/v2/portfolio/balance") => (
                        200,
                        serde_json::json!({ "balance": 12540, "portfolio_value": 14890 }),
                    ),
                    ("GET", "/trade-api/v2/portfolio/positions") => (
                        200,
                        serde_json::json!({
                            "market_positions": [{
                                "ticker": "KXBTC15M-TEST",
                                "position": 10,
                                "market_exposure": 640,
                                "realized_pnl": 120,
                                "resting_orders_count": 1,
                                "fees_paid": 14,
                                "total_traded": 1200
                            }]
                        }),
                    ),
                    ("GET", "/trade-api/v2/portfolio/orders") => {
                        let ords = state_srv.lock().unwrap().clone();
                        (200, serde_json::json!({ "orders": ords }))
                    }
                    ("GET", "/trade-api/v2/portfolio/fills") => (
                        200,
                        serde_json::json!({
                            "fills": [{
                                "trade_id": "tr-1",
                                "order_id": "ord-0",
                                "ticker": "KXBTC15M-TEST",
                                "action": "buy",
                                "side": "yes",
                                "count": 5,
                                "yes_price": 60,
                                "is_taker": true
                            }]
                        }),
                    ),
                    ("POST", "/trade-api/v2/portfolio/orders") => {
                        let req_v: serde_json::Value = serde_json::from_slice(&body).unwrap();
                        let new_ord = serde_json::json!({
                            "order_id": "ord-created-2",
                            "client_order_id": req_v["client_order_id"],
                            "ticker": req_v["ticker"],
                            "status": "resting",
                            "action": req_v["action"],
                            "side": req_v["side"],
                            "type": req_v["type"],
                            "yes_price": req_v["yes_price"],
                            "no_price": req_v["no_price"],
                            "initial_count": req_v["count"],
                            "remaining_count": req_v["count"],
                            "fill_count": 0
                        });
                        state_srv.lock().unwrap().push(new_ord.clone());
                        (201, serde_json::json!({ "order": new_ord }))
                    }
                    ("DELETE", p) if p.starts_with("/trade-api/v2/portfolio/orders/") => {
                        let id = p.rsplit('/').next().unwrap_or("");
                        state_srv
                            .lock()
                            .unwrap()
                            .retain(|o| o["order_id"].as_str() != Some(id));
                        (200, serde_json::json!({ "order_id": id, "status": "canceled" }))
                    }
                    _ => (404, serde_json::json!({ "error": "not found" })),
                };
                let body_str = resp_json.to_string();
                let resp = format!(
                    "HTTP/1.1 {status} OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body_str}",
                    body_str.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });

        let base = format!("http://{addr}/trade-api/v2");
        let client =
            KalshiLiveClient::from_parts("mock-key-id-9999", &pkcs8, KalshiEnv::Demo, &base)
                .unwrap();

        let bal = client.get_balance().unwrap();
        assert_eq!(bal.balance_cents, 12_540);
        assert!((bal.balance_dollars - 125.40).abs() < 1e-9);
        assert!((bal.portfolio_value_dollars - 148.90).abs() < 1e-9);

        let pos = client.get_positions().unwrap();
        assert_eq!(pos.len(), 1);
        assert_eq!(pos[0].ticker, "KXBTC15M-TEST");
        assert!((pos[0].market_exposure_dollars - 6.40).abs() < 1e-9);

        let fills = client.get_fills(Some(25)).unwrap();
        assert_eq!(fills.len(), 1);
        assert_eq!(fills[0].trade_id, "tr-1");

        let req = KalshiOrderRequest::limit("KXBTC15M-TEST", "buy", "yes", 3, 64, None).unwrap();
        let placed = client.place_order(&req).unwrap();
        assert_eq!(placed.order_id, "ord-created-2");

        // Cancel-all fetches both resting orders (ord-existing-1 + ord-created-2) and deletes both!
        let report = client.cancel_all_orders();
        assert_eq!(report.attempted, 2);
        assert_eq!(report.cancelled_count, 2);
        assert!(report.errors.is_empty());

        handle.join().unwrap();
    }
}
