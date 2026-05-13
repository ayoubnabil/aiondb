#![allow(clippy::missing_errors_doc, clippy::no_effect_underscore_binding)]

use std::{
    borrow::Cow,
    collections::{HashMap, VecDeque},
    fmt::Write as _,
    fs,
    fs::OpenOptions,
    io::{ErrorKind, Read as _, Write as _},
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use aiondb_core::{DbError, DbResult, SqlState};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use sha2::{Digest, Sha256};

use crate::{TransportInfo, TransportKind};

#[allow(clippy::missing_errors_doc)]
pub trait AuthRateLimiter: Send + Sync {
    fn check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()>;
    fn record_success(&self, principal: &str, transport: &TransportInfo) -> DbResult<()>;
    fn record_failure(&self, principal: &str, transport: &TransportInfo) -> DbResult<()>;
}

#[derive(Debug, Default)]
pub struct NoopAuthRateLimiter;

impl AuthRateLimiter for NoopAuthRateLimiter {
    fn check(&self, _principal: &str, _transport: &TransportInfo) -> DbResult<()> {
        Ok(())
    }

    fn record_success(&self, _principal: &str, _transport: &TransportInfo) -> DbResult<()> {
        Ok(())
    }

    fn record_failure(&self, _principal: &str, _transport: &TransportInfo) -> DbResult<()> {
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
enum RateLimitScope {
    InProcess,
    Network,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd, Hash)]
struct RateLimitKey {
    principal: String,
    scope: RateLimitScope,
    source: String,
}

#[derive(Debug)]
pub struct InMemoryAuthRateLimiter {
    max_failures: u32,
    lockout_window: Duration,
    failures: Mutex<HashMap<RateLimitKey, VecDeque<Instant>>>,
}

#[derive(Debug)]
pub struct FileBackedAuthRateLimiter {
    max_failures: u32,
    lockout_window: Duration,
    state_path: PathBuf,
    state: Mutex<PersistedRateLimitState>,
}

#[derive(Debug, Default)]
struct PersistedRateLimitState {
    failures: HashMap<RateLimitKey, VecDeque<u64>>,
}

const DEFAULT_MAX_AUTH_LOCKOUT_STATE_BYTES: u64 = 8 * 1024 * 1024;
const MIN_MAX_AUTH_LOCKOUT_STATE_BYTES: u64 = 64 * 1024;
const MAX_MAX_AUTH_LOCKOUT_STATE_BYTES: u64 = 256 * 1024 * 1024;

fn parse_max_auth_lockout_state_bytes(value: Option<&str>) -> u64 {
    value.and_then(|raw| raw.parse::<u64>().ok()).map_or(
        DEFAULT_MAX_AUTH_LOCKOUT_STATE_BYTES,
        |bytes| {
            bytes.clamp(
                MIN_MAX_AUTH_LOCKOUT_STATE_BYTES,
                MAX_MAX_AUTH_LOCKOUT_STATE_BYTES,
            )
        },
    )
}

fn max_auth_lockout_state_bytes() -> u64 {
    static MAX_AUTH_LOCKOUT_STATE_BYTES: OnceLock<u64> = OnceLock::new();
    *MAX_AUTH_LOCKOUT_STATE_BYTES.get_or_init(|| {
        parse_max_auth_lockout_state_bytes(
            std::env::var("AIONDB_SECURITY_AUTH_LOCKOUT_MAX_STATE_BYTES")
                .ok()
                .as_deref(),
        )
    })
}

impl InMemoryAuthRateLimiter {
    #[must_use]
    pub fn new(max_failures: u32, lockout_window: Duration) -> Self {
        Self {
            max_failures,
            lockout_window,
            failures: Mutex::new(HashMap::new()),
        }
    }

    fn disabled(&self) -> bool {
        self.max_failures == 0
    }

    fn max_failures_usize(&self) -> usize {
        usize::try_from(self.max_failures).unwrap_or(usize::MAX)
    }

    fn failures_mut(
        &self,
    ) -> DbResult<std::sync::MutexGuard<'_, HashMap<RateLimitKey, VecDeque<Instant>>>> {
        self.failures
            .lock()
            .map_err(|e| DbError::internal(format!("auth rate limiter state poisoned: {e}")))
    }

    fn prune_expired(
        failures: &mut HashMap<RateLimitKey, VecDeque<Instant>>,
        now: Instant,
        window: Duration,
    ) {
        failures.retain(|_, entries| {
            while let Some(timestamp) = entries.front() {
                if now.saturating_duration_since(*timestamp) < window {
                    break;
                }
                entries.pop_front();
            }
            !entries.is_empty()
        });
    }
}

impl AuthRateLimiter for InMemoryAuthRateLimiter {
    fn check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        let now = Instant::now();
        let key = rate_limit_key(principal, transport);
        let mut failures = self.failures_mut()?;
        Self::prune_expired(&mut failures, now, self.lockout_window);
        let recorded = matching_failure_count(&failures, &key);
        if recorded >= self.max_failures_usize() {
            return Err(auth_failure_limit_error());
        }
        Ok(())
    }

    fn record_success(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        let mut failures = self.failures_mut()?;
        let key = rate_limit_key(principal, transport);
        clear_matching_failures(&mut failures, &key);
        Ok(())
    }

    fn record_failure(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        let now = Instant::now();
        let key = rate_limit_key(principal, transport);
        let mut failures = self.failures_mut()?;
        Self::prune_expired(&mut failures, now, self.lockout_window);
        let entries = failures.entry(key).or_default();
        entries.push_back(now);
        while entries.len() > self.max_failures_usize() {
            entries.pop_front();
        }
        Ok(())
    }
}

impl FileBackedAuthRateLimiter {
    #[must_use]
    pub fn new(
        max_failures: u32,
        lockout_window: Duration,
        state_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            max_failures,
            lockout_window,
            state_path: state_path.into(),
            state: Mutex::new(PersistedRateLimitState::default()),
        }
    }

    fn disabled(&self) -> bool {
        self.max_failures == 0
    }

    fn max_failures_usize(&self) -> usize {
        usize::try_from(self.max_failures).unwrap_or(usize::MAX)
    }

    fn state_mut(&self) -> DbResult<std::sync::MutexGuard<'_, PersistedRateLimitState>> {
        self.state
            .lock()
            .map_err(|e| DbError::internal(format!("auth rate limiter state poisoned: {e}")))
    }

    fn refresh_from_disk(&self, state: &mut PersistedRateLimitState) -> DbResult<()> {
        state.failures = load_state(&self.state_path)?;
        Ok(())
    }

    fn acquire_state_file_lock(&self) -> DbResult<fs::File> {
        let lock_path = lock_state_path(&self.state_path);
        if let Some(parent) = lock_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(|error| {
                DbError::internal(format!(
                    "failed to create auth lockout state lock directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                DbError::internal(format!(
                    "failed to open auth lockout state lock {}: {error}",
                    lock_path.display()
                ))
            })?;
        lock_file.lock().map_err(|error| {
            DbError::internal(format!(
                "failed to lock auth lockout state {}: {error}",
                lock_path.display()
            ))
        })?;
        Ok(lock_file)
    }
}

struct StateFileLock {
    file: fs::File,
}

impl StateFileLock {
    fn acquire(limiter: &FileBackedAuthRateLimiter) -> DbResult<Self> {
        let file = limiter.acquire_state_file_lock()?;
        Ok(Self { file })
    }
}

impl Drop for StateFileLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl FileBackedAuthRateLimiter {
    fn with_locked_state<T>(
        &self,
        mut apply: impl FnMut(&mut PersistedRateLimitState) -> DbResult<T>,
    ) -> DbResult<T> {
        let _lock = StateFileLock::acquire(self)?;
        let mut state = self.state_mut()?;
        self.refresh_from_disk(&mut state)?;
        let result = apply(&mut state)?;
        Ok(result)
    }
}

impl FileBackedAuthRateLimiter {
    fn check_locked(
        &self,
        principal: &str,
        transport: &TransportInfo,
        state: &mut PersistedRateLimitState,
    ) -> DbResult<()> {
        let key = rate_limit_key(principal, transport);
        let now = now_epoch_millis();
        let pruned = prune_expired_millis(&mut state.failures, now, self.lockout_window);
        if pruned {
            persist_state(&self.state_path, &state.failures)?;
        }
        let recorded = matching_failure_count(&state.failures, &key);
        if recorded >= self.max_failures_usize() {
            return Err(auth_failure_limit_error());
        }
        Ok(())
    }

    fn record_success_locked(
        &self,
        principal: &str,
        transport: &TransportInfo,
        state: &mut PersistedRateLimitState,
    ) -> DbResult<()> {
        let key = rate_limit_key(principal, transport);
        if clear_matching_failures(&mut state.failures, &key) {
            persist_state(&self.state_path, &state.failures)?;
        }
        Ok(())
    }

    fn record_failure_locked(
        &self,
        principal: &str,
        transport: &TransportInfo,
        state: &mut PersistedRateLimitState,
    ) -> DbResult<()> {
        let key = rate_limit_key(principal, transport);
        let now = now_epoch_millis();
        prune_expired_millis(&mut state.failures, now, self.lockout_window);
        let entries = state.failures.entry(key).or_default();
        entries.push_back(now);
        while entries.len() > self.max_failures_usize() {
            entries.pop_front();
        }
        persist_state(&self.state_path, &state.failures)?;
        Ok(())
    }
}

impl AuthRateLimiter for FileBackedAuthRateLimiter {
    fn check(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        self.with_locked_state(|state| self.check_locked(principal, transport, state))
    }

    fn record_success(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        self.with_locked_state(|state| self.record_success_locked(principal, transport, state))
    }

    fn record_failure(&self, principal: &str, transport: &TransportInfo) -> DbResult<()> {
        if self.disabled() {
            return Ok(());
        }

        self.with_locked_state(|state| self.record_failure_locked(principal, transport, state))
    }
}

/// Maximum principal name length tracked by the rate limiter.
/// Longer names are truncated to prevent memory exhaustion attacks.
const MAX_PRINCIPAL_LENGTH: usize = 128;
const MAX_SOURCE_LENGTH: usize = 128;
const HASHED_COMPONENT_SUFFIX_HEX_LEN: usize = 24;

fn rate_limit_key(principal: &str, transport: &TransportInfo) -> RateLimitKey {
    let scope = match transport.kind {
        TransportKind::InProcess => RateLimitScope::InProcess,
        TransportKind::Network { .. } => RateLimitScope::Network,
    };
    let canonical_principal = canonicalize_principal_for_rate_limit(principal);
    let source = transport_source_key(transport);
    RateLimitKey {
        principal: normalize_rate_limit_component(&canonical_principal, MAX_PRINCIPAL_LENGTH),
        scope,
        source: normalize_rate_limit_component(&source, MAX_SOURCE_LENGTH),
    }
}

fn canonicalize_principal_for_rate_limit(principal: &str) -> Cow<'_, str> {
    // ASCII fast path: standard case-fold so "User", "USER", "user" share one
    // bucket. For non-ASCII names, run NFKC compatibility decomposition then
    // ASCII-lowercase the result. NFKC collapses Unicode confusables (e.g.
    // fullwidth `Ｕｓｅｒ` → `User`, ligatures, common compatibility forms)
    // into a canonical form so visually-equivalent variants share a single
    // rate-limit bucket. Cyrillic homoglyphs that share no Unicode
    // canonical-equivalence with their Latin look-alikes still bucket
    // separately, but those are treated as a distinct principal at the auth
    // layer too, so the rate-limit/auth identity boundaries stay consistent.
    if principal.is_ascii() {
        if principal.bytes().any(|byte| byte.is_ascii_uppercase()) {
            Cow::Owned(principal.to_ascii_lowercase())
        } else {
            Cow::Borrowed(principal)
        }
    } else {
        use unicode_normalization::UnicodeNormalization;
        Cow::Owned(principal.nfkc().collect::<String>().to_lowercase())
    }
}

fn normalize_rate_limit_component(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        return value.to_owned();
    }

    let suffix_separator_len = 1usize;
    let suffix_budget = HASHED_COMPONENT_SUFFIX_HEX_LEN + suffix_separator_len;
    if max_len <= suffix_budget {
        return truncate_hex_hash(value, max_len);
    }

    let prefix = truncate_utf8_boundary(value, max_len - suffix_budget);
    let mut normalized = String::with_capacity(prefix.len() + suffix_budget);
    normalized.push_str(prefix);
    normalized.push('~');
    append_component_hash_suffix(&mut normalized, value);
    normalized
}

fn truncate_hex_hash(value: &str, max_len: usize) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut output = String::with_capacity(max_len);
    for byte in digest {
        if output.len() + 2 > max_len {
            break;
        }
        let _ = write!(&mut output, "{byte:02x}");
    }
    output
}

fn append_component_hash_suffix(output: &mut String, value: &str) {
    let digest = Sha256::digest(value.as_bytes());
    for byte in digest.iter().take(HASHED_COMPONENT_SUFFIX_HEX_LEN / 2) {
        let _ = write!(output, "{byte:02x}");
    }
}

fn truncate_utf8_boundary(value: &str, max_len: usize) -> &str {
    if value.len() <= max_len {
        return value;
    }
    let mut end = max_len;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn transport_source_key(transport: &TransportInfo) -> String {
    match &transport.kind {
        TransportKind::InProcess => String::new(),
        TransportKind::Network { peer_addr, .. } => peer_addr
            .as_deref()
            .and_then(normalize_peer_addr)
            .unwrap_or_else(|| "unknown".to_owned()),
    }
}

fn normalize_peer_addr(peer_addr: &str) -> Option<String> {
    let trimmed = peer_addr.trim();
    if trimmed.is_empty() {
        return None;
    }

    if let Ok(socket_addr) = trimmed.parse::<std::net::SocketAddr>() {
        return Some(socket_addr.ip().to_string());
    }

    if let Ok(ip_addr) = trimmed.parse::<std::net::IpAddr>() {
        return Some(ip_addr.to_string());
    }

    if let Some(stripped) = trimmed
        .strip_prefix('[')
        .and_then(|value| value.strip_suffix(']'))
    {
        if let Ok(ip_addr) = stripped.parse::<std::net::IpAddr>() {
            return Some(ip_addr.to_string());
        }
    }

    let host = strip_optional_host_port(trimmed);
    Some(host.to_ascii_lowercase())
}

fn strip_optional_host_port(value: &str) -> &str {
    let Some((host, port)) = value.rsplit_once(':') else {
        return value;
    };
    if host.contains(':') || port.is_empty() || !port.bytes().all(|byte| byte.is_ascii_digit()) {
        return value;
    }
    host
}

fn auth_failure_limit_error() -> DbError {
    DbError::authorization_error(
        SqlState::TooManyAuthenticationFailures,
        "too many authentication failures; retry later",
    )
}

fn prune_expired_millis(
    failures: &mut HashMap<RateLimitKey, VecDeque<u64>>,
    now: u64,
    window: Duration,
) -> bool {
    let mut changed = false;
    let window_ms = duration_millis(window);
    failures.retain(|_, entries| {
        while let Some(timestamp) = entries.front() {
            if now.saturating_sub(*timestamp) < window_ms {
                break;
            }
            entries.pop_front();
            changed = true;
        }
        if entries.is_empty() {
            changed = true;
            return false;
        }
        true
    });
    changed
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis().min(u128::from(u64::MAX))).unwrap_or(u64::MAX)
}

fn matching_failure_count<T>(
    failures: &HashMap<RateLimitKey, VecDeque<T>>,
    key: &RateLimitKey,
) -> usize {
    let mut recorded = failures.get(key).map_or(0, VecDeque::len);
    if let Some(legacy_key) = legacy_network_fallback_key(key) {
        recorded += failures.get(&legacy_key).map_or(0, VecDeque::len);
    }
    recorded
}

fn clear_matching_failures<T>(
    failures: &mut HashMap<RateLimitKey, VecDeque<T>>,
    key: &RateLimitKey,
) -> bool {
    let mut removed = failures.remove(key).is_some();
    if let Some(legacy_key) = legacy_network_fallback_key(key) {
        removed |= failures.remove(&legacy_key).is_some();
    }
    removed
}

fn legacy_network_fallback_key(key: &RateLimitKey) -> Option<RateLimitKey> {
    if key.scope != RateLimitScope::Network || key.source.is_empty() {
        return None;
    }

    Some(RateLimitKey {
        principal: key.principal.clone(),
        scope: key.scope,
        source: String::new(),
    })
}

fn now_epoch_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u128::from(u64::MAX))
        .try_into()
        .unwrap_or(u64::MAX)
}

fn load_state(path: &Path) -> DbResult<HashMap<RateLimitKey, VecDeque<u64>>> {
    let max_state_bytes = max_auth_lockout_state_bytes();
    let Some(content) = read_state_file_capped(path, max_state_bytes)? else {
        return Ok(HashMap::new());
    };

    let mut failures = HashMap::new();
    for (line_no, raw_line) in content.lines().enumerate() {
        let line = raw_line.trim();
        if line.is_empty() || matches!(line, "v1" | "v2") {
            continue;
        }

        let mut parts = line.split('\t');
        let scope = parse_scope(
            parts
                .next()
                .ok_or_else(|| malformed_state_error(path, line_no + 1))?,
            path,
            line_no + 1,
        )?;
        let principal = decode_component(
            parts
                .next()
                .ok_or_else(|| malformed_state_error(path, line_no + 1))?,
            "principal",
            path,
            line_no + 1,
        )?;
        let source_or_timestamps = parts
            .next()
            .ok_or_else(|| malformed_state_error(path, line_no + 1))?;
        let next = parts.next();
        let (source, timestamps) = match next {
            Some(timestamps) => (
                decode_component(source_or_timestamps, "source", path, line_no + 1)?,
                timestamps,
            ),
            None => (String::new(), source_or_timestamps),
        };
        if parts.next().is_some() {
            return Err(malformed_state_error(path, line_no + 1));
        }

        let mut entries = VecDeque::new();
        if !timestamps.is_empty() {
            for raw_timestamp in timestamps.split(',') {
                let timestamp = raw_timestamp.parse::<u64>().map_err(|error| {
                    DbError::internal(format!(
                        "invalid auth lockout timestamp in {} at line {}: {error}",
                        path.display(),
                        line_no + 1
                    ))
                })?;
                entries.push_back(timestamp);
            }
        }

        if !entries.is_empty() {
            failures.insert(
                RateLimitKey {
                    principal,
                    scope,
                    source,
                },
                entries,
            );
        }
    }

    Ok(failures)
}

fn read_state_file_capped(path: &Path, max_state_bytes: u64) -> DbResult<Option<String>> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(DbError::internal(format!(
                "failed to read auth lockout state {}: {error}",
                path.display()
            )));
        }
    };
    let file_len = file
        .metadata()
        .map_err(|error| {
            DbError::internal(format!(
                "failed to read auth lockout state metadata {}: {error}",
                path.display()
            ))
        })?
        .len();
    if file_len > max_state_bytes {
        return Err(DbError::internal(format!(
            "auth lockout state {} is {} bytes, exceeding maximum {} bytes",
            path.display(),
            file_len,
            max_state_bytes
        )));
    }
    let capacity = usize::try_from(file_len).map_err(|_| {
        DbError::internal(format!(
            "auth lockout state {} size {} does not fit in usize",
            path.display(),
            file_len
        ))
    })?;
    let mut content = String::with_capacity(capacity);
    let mut reader = file.take(max_state_bytes.saturating_add(1));
    reader.read_to_string(&mut content).map_err(|error| {
        DbError::internal(format!(
            "failed to read auth lockout state {}: {error}",
            path.display()
        ))
    })?;
    if u64::try_from(content.len()).unwrap_or(u64::MAX) > max_state_bytes {
        return Err(DbError::internal(format!(
            "auth lockout state {} grew while reading, exceeding maximum {} bytes",
            path.display(),
            max_state_bytes
        )));
    }
    Ok(Some(content))
}

fn parse_scope(scope: &str, path: &Path, line_no: usize) -> DbResult<RateLimitScope> {
    match scope {
        "in" => Ok(RateLimitScope::InProcess),
        "net" => Ok(RateLimitScope::Network),
        _ => Err(DbError::internal(format!(
            "invalid auth lockout scope in {} at line {}",
            path.display(),
            line_no
        ))),
    }
}

fn decode_component(encoded: &str, field: &str, path: &Path, line_no: usize) -> DbResult<String> {
    let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|error| {
        DbError::internal(format!(
            "invalid auth lockout {field} encoding in {} at line {}: {error}",
            path.display(),
            line_no
        ))
    })?;
    String::from_utf8(bytes).map_err(|error| {
        DbError::internal(format!(
            "invalid auth lockout {field} text in {} at line {}: {error}",
            path.display(),
            line_no
        ))
    })
}

fn persist_state(path: &Path, failures: &HashMap<RateLimitKey, VecDeque<u64>>) -> DbResult<()> {
    if failures.is_empty() {
        return remove_if_exists(path);
    }

    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent).map_err(|error| {
            DbError::internal(format!(
                "failed to create auth lockout state directory {}: {error}",
                parent.display()
            ))
        })?;
    }

    let mut items = failures
        .iter()
        .filter(|(_, entries)| !entries.is_empty())
        .collect::<Vec<_>>();
    items.sort_by(|(left_key, left_entries), (right_key, right_entries)| {
        let left_ts = left_entries.back().copied().unwrap_or(0);
        let right_ts = right_entries.back().copied().unwrap_or(0);
        left_ts.cmp(&right_ts).then_with(|| left_key.cmp(right_key))
    });

    let mut content = String::from("v2\n");
    let max_state_bytes = usize::try_from(max_auth_lockout_state_bytes()).unwrap_or(usize::MAX);
    let mut truncated = false;
    for (key, entries) in items.into_iter().rev() {
        let scope = match key.scope {
            RateLimitScope::InProcess => "in",
            RateLimitScope::Network => "net",
        };
        let timestamps = entries
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<_>>()
            .join(",");
        let mut line = String::new();
        line.push_str(scope);
        line.push('\t');
        line.push_str(&URL_SAFE_NO_PAD.encode(key.principal.as_bytes()));
        line.push('\t');
        line.push_str(&URL_SAFE_NO_PAD.encode(key.source.as_bytes()));
        line.push('\t');
        line.push_str(&timestamps);
        line.push('\n');

        if content
            .len()
            .checked_add(line.len())
            .map_or(true, |next_len| next_len > max_state_bytes)
        {
            truncated = true;
            continue;
        }
        content.push_str(&line);
    }

    if truncated {
        // lockout entries. No tracing dep in this crate, so use stderr
        // which the server pipes through its observability layer at
        // boot. An operator running over the configured cap deserves to
        // know their lockout state was partially serialized.
        eprintln!(
            "warning: aiondb_security::rate_limit state file truncated \
             to fit byte budget {max_state_bytes}; oldest entries dropped"
        );
    }
    if content == "v2\n" {
        return remove_if_exists(path);
    }

    let tmp_path = tmp_state_path(path);
    let mut file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&tmp_path)
        .map_err(|error| {
            DbError::internal(format!(
                "failed to create auth lockout state {}: {error}",
                tmp_path.display()
            ))
        })?;

    // Restrict file permissions to owner-only (0600) so that other users on
    // the system cannot read authentication failure metadata (usernames,
    // source IPs, timestamps). This is defense-in-depth; the file should
    // already live in a directory owned by the database process user.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let permissions = std::fs::Permissions::from_mode(0o600);
        // Best-effort: if this fails, the file still remains protected by the
        // process umask. The subsequent fsync persists whichever mode exists.
        let _ = fs::set_permissions(&tmp_path, permissions);
    }

    file.write_all(content.as_bytes()).map_err(|error| {
        DbError::internal(format!(
            "failed to write auth lockout state {}: {error}",
            tmp_path.display()
        ))
    })?;
    file.sync_all().map_err(|error| {
        DbError::internal(format!(
            "failed to sync auth lockout state {}: {error}",
            tmp_path.display()
        ))
    })?;
    drop(file);

    fs::rename(&tmp_path, path).map_err(|error| {
        DbError::internal(format!(
            "failed to install auth lockout state {}: {error}",
            path.display()
        ))
    })?;
    sync_parent_dir(path)?;
    Ok(())
}

fn sync_parent_dir(path: &Path) -> DbResult<()> {
    aiondb_core::bounded_io::sync_parent_dir(path).map_err(|error| {
        let parent = path.parent().unwrap_or(path);
        DbError::internal(format!(
            "failed to sync auth lockout state directory {}: {error}",
            parent.display()
        ))
    })
}

fn remove_if_exists(path: &Path) -> DbResult<()> {
    match fs::remove_file(path) {
        Ok(()) => sync_parent_dir(path),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(DbError::internal(format!(
            "failed to remove auth lockout state {}: {error}",
            path.display()
        ))),
    }
}

fn tmp_state_path(path: &Path) -> PathBuf {
    let mut tmp = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth_lockout_state");
    tmp.set_file_name(format!("{name}.tmp"));
    tmp
}

fn lock_state_path(path: &Path) -> PathBuf {
    let mut lock = path.to_path_buf();
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("auth_lockout_state");
    lock.set_file_name(format!("{name}.lock"));
    lock
}

fn malformed_state_error(path: &Path, line_no: usize) -> DbError {
    DbError::internal(format!(
        "malformed auth lockout state in {} at line {}",
        path.display(),
        line_no
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process;

    fn transport() -> TransportInfo {
        TransportInfo::in_process()
    }

    fn network_transport() -> TransportInfo {
        network_transport_with_peer("127.0.0.1:5432")
    }

    fn network_transport_with_peer(peer_addr: &str) -> TransportInfo {
        TransportInfo {
            kind: crate::TransportKind::Network {
                tls: true,
                peer_addr: Some(peer_addr.to_owned()),
            },
        }
    }

    fn temp_state_path(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aiondb-rate-limit-{name}-{}-{:?}-{}.state",
            process::id(),
            std::thread::current().id(),
            now_epoch_millis()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    // --- check() always returns Ok ---
    #[test]
    fn noop_check_always_ok() {
        let rl = NoopAuthRateLimiter;
        assert!(rl.check("user1", &transport()).is_ok());
        assert!(rl.check("user2", &transport()).is_ok());
        assert!(rl.check("", &transport()).is_ok());
        for _ in 0..100 {
            assert!(rl.check("attacker", &transport()).is_ok());
        }
    }

    // --- record_success() doesn't panic ---
    #[test]
    fn noop_record_success_no_panic() {
        let rl = NoopAuthRateLimiter;
        rl.record_success("user1", &transport()).unwrap();
        rl.record_success("", &transport()).unwrap();
        for _ in 0..50 {
            rl.record_success("user", &transport()).unwrap();
        }
    }

    // --- record_failure() doesn't panic ---
    #[test]
    fn noop_record_failure_no_panic() {
        let rl = NoopAuthRateLimiter;
        rl.record_failure("user1", &transport()).unwrap();
        rl.record_failure("", &transport()).unwrap();
        for _ in 0..50 {
            rl.record_failure("user", &transport()).unwrap();
        }
    }

    // ===================================================================
    // NEW EDGE CASE TESTS
    // ===================================================================

    #[test]
    fn noop_check_unicode_principal() {
        let rl = NoopAuthRateLimiter;
        assert!(rl.check("日本語ユーザー", &transport()).is_ok());
    }

    #[test]
    fn noop_check_very_long_principal() {
        let rl = NoopAuthRateLimiter;
        let long_principal = "u".repeat(100_000);
        assert!(rl.check(&long_principal, &transport()).is_ok());
    }

    #[test]
    fn noop_check_special_chars_principal() {
        let rl = NoopAuthRateLimiter;
        assert!(rl.check("user@domain.com", &transport()).is_ok());
        assert!(rl.check("user\0null", &transport()).is_ok());
        assert!(rl.check("user\nnewline", &transport()).is_ok());
    }

    #[test]
    fn noop_interleaved_success_failure() {
        let rl = NoopAuthRateLimiter;
        for _ in 0..50 {
            rl.record_failure("user", &transport()).unwrap();
            rl.record_success("user", &transport()).unwrap();
            assert!(rl.check("user", &transport()).is_ok());
        }
    }

    #[test]
    fn noop_check_with_network_transport() {
        let rl = NoopAuthRateLimiter;
        let ti = TransportInfo {
            kind: crate::TransportKind::Network {
                tls: true,
                peer_addr: Some("10.0.0.1:4321".to_string()),
            },
        };
        assert!(rl.check("user", &ti).is_ok());
        rl.record_success("user", &ti).unwrap();
        rl.record_failure("user", &ti).unwrap();
    }

    #[test]
    fn noop_check_with_network_no_tls() {
        let rl = NoopAuthRateLimiter;
        let ti = TransportInfo {
            kind: crate::TransportKind::Network {
                tls: false,
                peer_addr: None,
            },
        };
        assert!(rl.check("user", &ti).is_ok());
    }

    #[test]
    fn noop_rate_limiter_debug() {
        let rl = NoopAuthRateLimiter;
        let dbg = format!("{rl:?}");
        assert!(dbg.contains("NoopAuthRateLimiter"));
    }

    #[test]
    fn noop_rate_limiter_default() {
        let _rl = NoopAuthRateLimiter;
    }

    #[test]
    fn noop_many_failures_still_ok() {
        let rl = NoopAuthRateLimiter;
        for _ in 0..10_000 {
            rl.record_failure("brute_force_user", &transport()).unwrap();
        }
        assert!(rl.check("brute_force_user", &transport()).is_ok());
    }

    #[test]
    fn noop_different_principals_interleaved() {
        let rl = NoopAuthRateLimiter;
        for i in 0..100 {
            let user = format!("user_{i}");
            rl.record_failure(&user, &transport()).unwrap();
            assert!(rl.check(&user, &transport()).is_ok());
            rl.record_success(&user, &transport()).unwrap();
        }
    }

    #[test]
    fn in_memory_limiter_blocks_after_max_failures() {
        let rl = InMemoryAuthRateLimiter::new(2, Duration::from_secs(60));
        assert!(rl.check("user", &transport()).is_ok());
        rl.record_failure("user", &transport()).unwrap();
        assert!(rl.check("user", &transport()).is_ok());
        rl.record_failure("user", &transport()).unwrap();

        let err = rl
            .check("user", &transport())
            .expect_err("user should be locked out");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
        assert!(format!("{err}").contains("too many authentication failures"));
    }

    #[test]
    fn in_memory_limiter_success_resets_failures() {
        let rl = InMemoryAuthRateLimiter::new(2, Duration::from_secs(60));
        rl.record_failure("user", &transport()).unwrap();
        assert!(rl.check("user", &transport()).is_ok());
        rl.record_success("user", &transport()).unwrap();
        rl.record_failure("user", &transport()).unwrap();
        assert!(rl.check("user", &transport()).is_ok());
    }

    #[test]
    fn in_memory_limiter_tracks_network_and_local_separately() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::from_secs(60));
        rl.record_failure("user", &network_transport()).unwrap();

        let err = rl
            .check("user", &network_transport())
            .expect_err("network scope should be locked");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
        assert!(format!("{err}").contains("too many authentication failures"));
        assert!(rl.check("user", &transport()).is_ok());
    }

    #[test]
    fn in_memory_limiter_tracks_network_sources_separately() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::from_secs(60));
        rl.record_failure("user", &network_transport_with_peer("127.0.0.1:5432"))
            .unwrap();

        let err = rl
            .check("user", &network_transport_with_peer("127.0.0.1:5432"))
            .expect_err("same source should be locked");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
        assert!(
            rl.check("user", &network_transport_with_peer("127.0.0.2:5432"))
                .is_ok(),
            "different source should not inherit the lockout"
        );
    }

    #[test]
    fn in_memory_limiter_zero_max_failures_disables_lockout() {
        let rl = InMemoryAuthRateLimiter::new(0, Duration::from_secs(60));
        for _ in 0..10 {
            rl.record_failure("user", &transport()).unwrap();
        }
        assert!(rl.check("user", &transport()).is_ok());
    }

    #[test]
    fn in_memory_limiter_zero_window_expires_immediately() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::ZERO);
        rl.record_failure("user", &transport()).unwrap();
        assert!(rl.check("user", &transport()).is_ok());
    }

    #[test]
    fn file_backed_limiter_missing_file_starts_empty() {
        let path = temp_state_path("missing");
        let rl = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        assert!(rl.check("user", &transport()).is_ok());
        assert!(!path.exists());
    }

    #[test]
    fn file_backed_limiter_persists_lockout_across_instances() {
        let path = temp_state_path("persist");
        let rl = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        rl.record_failure("user", &transport()).unwrap();
        rl.record_failure("user", &transport()).unwrap();

        let err = rl
            .check("user", &transport())
            .expect_err("user should be locked out");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);

        drop(rl);

        let reloaded = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        let err = reloaded
            .check("user", &transport())
            .expect_err("lockout should survive reload");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn in_memory_limiter_long_principals_with_same_prefix_do_not_collide() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::from_secs(60));
        let shared_prefix = "p".repeat(MAX_PRINCIPAL_LENGTH + 32);
        let first = format!("{shared_prefix}-alpha");
        let second = format!("{shared_prefix}-beta");

        rl.record_failure(&first, &transport()).unwrap();

        let err = rl
            .check(&first, &transport())
            .expect_err("first principal should be locked out");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);

        assert!(
            rl.check(&second, &transport()).is_ok(),
            "different long principal must not share the same lockout bucket"
        );
    }

    #[test]
    fn in_memory_limiter_canonicalizes_network_host_source_keys() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::from_secs(60));
        let first = network_transport_with_peer("Example.COM:5432");
        let second = network_transport_with_peer("example.com:9999");

        rl.record_failure("user", &first).unwrap();

        let err = rl
            .check("user", &second)
            .expect_err("same network host should map to the same lockout bucket");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
    }

    #[test]
    fn in_memory_limiter_canonicalizes_principal_case() {
        let rl = InMemoryAuthRateLimiter::new(1, Duration::from_secs(60));
        rl.record_failure("reader", &transport()).unwrap();

        let err = rl
            .check("Reader", &transport())
            .expect_err("principal case variants should map to the same lockout bucket");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);
    }

    #[test]
    fn file_backed_limiter_success_clears_persisted_failures() {
        let path = temp_state_path("reset");
        let rl = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        rl.record_failure("user", &transport()).unwrap();
        rl.record_success("user", &transport()).unwrap();
        drop(rl);

        let reloaded = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        assert!(reloaded.check("user", &transport()).is_ok());
        assert!(!path.exists());
    }

    #[test]
    fn file_backed_limiter_rejects_malformed_state() {
        let path = temp_state_path("malformed");
        std::fs::write(&path, "not-a-valid-state\n").unwrap();

        let rl = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        let err = rl
            .check("user", &transport())
            .expect_err("malformed file should fail");
        assert_eq!(err.sqlstate(), SqlState::InternalError);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn read_state_file_capped_rejects_oversized_state() {
        let path = temp_state_path("oversized-helper");
        std::fs::write(&path, "v2\n0123456789\n").unwrap();

        let err = read_state_file_capped(&path, 4).expect_err("state must exceed helper cap");
        assert!(
            err.to_string().contains("exceeding maximum"),
            "unexpected error: {err}"
        );

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn file_backed_limiter_reads_legacy_state_without_source_column() {
        let path = temp_state_path("legacy");
        let encoded_user = URL_SAFE_NO_PAD.encode("user");
        let now = now_epoch_millis();
        std::fs::write(&path, format!("v1\nnet\t{encoded_user}\t{now},{now}\n")).unwrap();

        let rl = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        let err = rl
            .check("user", &network_transport_with_peer("127.0.0.1:5432"))
            .expect_err("legacy lockout should still be enforced");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn file_backed_limiter_multi_instance_updates_are_atomic() {
        let path = temp_state_path("multi-instance-atomic");
        let lock_path = lock_state_path(&path);
        let rl1 = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        let rl2 = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());

        // Warm both instances so stale in-memory caches would exist if each
        // operation didn't reload the state under a shared file lock.
        assert!(rl1.check("user", &transport()).is_ok());
        assert!(rl2.check("user", &transport()).is_ok());

        rl1.record_failure("user", &transport()).unwrap();
        rl2.record_failure("user", &transport()).unwrap();

        let reloaded = FileBackedAuthRateLimiter::new(2, Duration::from_secs(60), path.clone());
        let err = reloaded
            .check("user", &transport())
            .expect_err("both failures must be persisted atomically");
        assert_eq!(err.sqlstate(), SqlState::TooManyAuthenticationFailures);

        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_file(lock_path);
    }
}
