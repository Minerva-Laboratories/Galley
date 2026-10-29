//! One in-memory rate limiter for the whole server, set under `[limits]` in `galley.toml`.
//!
//! Stage 1 counts per client IP before any handler runs ([`enforce`], one middleware on the whole
//! router). Stage 2 counts per account once the caller is known. [`group_for`] is the only policy
//! table: it sorts every route into a [`Group`], and anything it does not name is
//! [`Group::Default`], so a new route is limited by default.
//!
//! Every map is bounded. When one is full, a new key counts in a coarser bucket: an IP under its
//! network prefix, anything else under one of [`SHARDS`] hashed shards. Nothing fails open.

use std::collections::hash_map::RandomState;
use std::collections::HashMap;
use std::hash::{BuildHasher, Hash};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderName, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::{AppError, AppState};
use crate::config::{LimitsConfig, Rule};

/// The body of every refusal, in the user's words.
pub const TOO_MANY: &str = "Too many attempts. Try again in a few moments.";

/// Buckets for account, link and email keys that arrive while their map is full.
pub const SHARDS: u64 = 4096;

/// Expired windows are dropped at most this often while a map has room.
const PRUNE_EVERY: Duration = Duration::from_secs(60);
/// A full map is pruned at most this often before a new key goes to the overflow buckets, so a
/// flood of new keys cannot make every request scan the whole map.
const FULL_PRUNE_EVERY: Duration = Duration::from_secs(1);
/// A full map logs at most this often.
const WARN_EVERY: Duration = Duration::from_secs(60);
/// Longer windows are cut to this, so no `Instant` arithmetic can overflow.
const MAX_WINDOW_SECS: u64 = 366 * 24 * 3600;
/// The longest login lock per (email, IP).
pub const MAX_LOGIN_LOCK: Duration = Duration::from_secs(900);
/// A failed-login record with no request in flight is dropped this long after its last failure.
const LOGIN_RECORD_TTL: Duration = Duration::from_secs(24 * 3600);
/// A successful sign-in spares its network from the email-wide lock for this long.
const LOGIN_SUCCESS_TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// Emails longer than any real address are cut to this many bytes before they become keys, so a
/// huge request body cannot become a huge key.
const MAX_EMAIL_KEY: usize = 320;

/// Created once per process, so nobody outside can predict which shard a key lands in.
static SHARD_HASHER: LazyLock<RandomState> = LazyLock::new(RandomState::new);

/// What a request does, as far as limits go.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Group {
    Login,
    Signup,
    Join,
    SharePreview,
    Oidc,
    Build,
    Import,
    Mcp,
    TokenCreate,
    ProjectCreate,
    Invite,
    Lookup,
    Socket,
    Default,
}

/// The policy table. `matched_path` is the route template from [`MatchedPath`], or `None` for the
/// assets fallback. HEAD runs the GET handler, so it counts as GET.
pub fn group_for(method: &Method, matched_path: Option<&str>) -> Group {
    let Some(path) = matched_path else { return Group::Default };
    let get = method == Method::GET || method == Method::HEAD;
    let post = method == Method::POST;
    match path {
        "/api/auth/login" if post => Group::Login,
        "/api/auth/signup" if post => Group::Signup,
        "/api/auth/landing" if post => Group::Join,
        "/api/share/{token}" if get => Group::SharePreview,
        "/api/auth/oidc/start" | "/api/auth/oidc/callback" if get => Group::Oidc,
        "/api/projects/{id}/build" | "/api/projects/{id}/latexdiff" | "/api/projects/{id}/pack" if post => Group::Build,
        "/api/projects/import" if post => Group::Import,
        "/mcp/{id}" if post => Group::Mcp,
        "/api/auth/tokens" if post => Group::TokenCreate,
        "/api/projects" if post => Group::ProjectCreate,
        "/api/projects/{id}/members" if post => Group::Invite,
        "/api/projects/{id}/grammar"
        | "/api/projects/{id}/bib/{key}/lookup"
        | "/api/projects/{id}/citations/candidates"
        | "/api/projects/{id}/bib"
            if post =>
        {
            Group::Lookup
        }
        "/api/projects/{id}/bib/search" if get => Group::Lookup,
        "/ws/{id}/events" | "/ws/{id}/doc/{*path}" if get => Group::Socket,
        _ => Group::Default,
    }
}

impl Group {
    /// The stage 1 rule counted per client IP, besides `default_ip` which every request counts.
    pub fn ip_rule(self) -> Option<RuleId> {
        match self {
            Group::Login => Some(RuleId::LoginIp),
            Group::Signup => Some(RuleId::SignupIp),
            Group::Join => Some(RuleId::JoinIp),
            Group::SharePreview => Some(RuleId::SharePreviewIp),
            Group::Oidc => Some(RuleId::OidcIp),
            Group::Build => Some(RuleId::BuildIp),
            Group::Import => Some(RuleId::ImportIp),
            Group::Mcp => Some(RuleId::McpIp),
            Group::Socket => Some(RuleId::SocketIp),
            Group::TokenCreate | Group::ProjectCreate | Group::Invite | Group::Lookup | Group::Default => None,
        }
    }

    /// The stage 2 rule the request extractors count per account. Sockets count theirs in
    /// `ws::authenticate`, and MCP counts per JSON-RPC message, so neither is listed here.
    pub fn account_rule(self) -> Option<RuleId> {
        match self {
            Group::Build => Some(RuleId::BuildAccount),
            Group::Import => Some(RuleId::ImportAccount),
            Group::TokenCreate => Some(RuleId::TokenCreateAccount),
            Group::ProjectCreate => Some(RuleId::ProjectCreateAccount),
            Group::Invite => Some(RuleId::InviteAccount),
            Group::Lookup => Some(RuleId::LookupAccount),
            _ => None,
        }
    }
}

/// Every counted rule. Each has its own map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleId {
    LoginIp,
    SignupIp,
    SignupGlobal,
    JoinIp,
    JoinGlobal,
    JoinLink,
    SharePreviewIp,
    OidcIp,
    BuildIp,
    ImportIp,
    McpIp,
    SocketIp,
    DefaultIp,
    BuildAccount,
    ImportAccount,
    McpAccount,
    TokenCreateAccount,
    ProjectCreateAccount,
    InviteAccount,
    LookupAccount,
    SocketAccount,
    LoginEmailFailures,
}

impl RuleId {
    pub const ALL: [RuleId; 22] = [
        RuleId::LoginIp,
        RuleId::SignupIp,
        RuleId::SignupGlobal,
        RuleId::JoinIp,
        RuleId::JoinGlobal,
        RuleId::JoinLink,
        RuleId::SharePreviewIp,
        RuleId::OidcIp,
        RuleId::BuildIp,
        RuleId::ImportIp,
        RuleId::McpIp,
        RuleId::SocketIp,
        RuleId::DefaultIp,
        RuleId::BuildAccount,
        RuleId::ImportAccount,
        RuleId::McpAccount,
        RuleId::TokenCreateAccount,
        RuleId::ProjectCreateAccount,
        RuleId::InviteAccount,
        RuleId::LookupAccount,
        RuleId::SocketAccount,
        RuleId::LoginEmailFailures,
    ];

    /// This rule's setting in `config`.
    pub fn rule(self, config: &LimitsConfig) -> Rule {
        match self {
            RuleId::LoginIp => config.login_ip,
            RuleId::SignupIp => config.signup_ip,
            RuleId::SignupGlobal => config.signup_global,
            RuleId::JoinIp => config.join_ip,
            RuleId::JoinGlobal => config.join_global,
            RuleId::JoinLink => config.join_link,
            RuleId::SharePreviewIp => config.share_preview_ip,
            RuleId::OidcIp => config.oidc_ip,
            RuleId::BuildIp => config.build_ip,
            RuleId::ImportIp => config.import_ip,
            RuleId::McpIp => config.mcp_ip,
            RuleId::SocketIp => config.socket_ip,
            RuleId::DefaultIp => config.default_ip,
            RuleId::BuildAccount => config.build_account,
            RuleId::ImportAccount => config.import_account,
            RuleId::McpAccount => config.mcp_account,
            RuleId::TokenCreateAccount => config.token_create_account,
            RuleId::ProjectCreateAccount => config.project_create_account,
            RuleId::InviteAccount => config.invite_account,
            RuleId::LookupAccount => config.lookup_account,
            RuleId::SocketAccount => config.socket_account,
            RuleId::LoginEmailFailures => config.login_email_failures,
        }
    }

    fn index(self) -> usize {
        self as usize
    }
}

/// A client address as the limiter counts it. IPv4-mapped IPv6 is the IPv4 address, and a real
/// IPv6 address is cut to its /64, which one client usually holds whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IpKey {
    V4([u8; 4]),
    V6([u8; 16]),
    /// No peer address is known. Every such request shares this one key.
    Unknown,
}

impl IpKey {
    pub fn from_ip(ip: IpAddr) -> IpKey {
        match ip.to_canonical() {
            IpAddr::V4(a) => IpKey::V4(a.octets()),
            IpAddr::V6(a) => IpKey::V6(mask_v6(u128::from(a), 64)),
        }
    }

    /// The coarse network: IPv4 /16, IPv6 /48.
    pub fn prefix(self) -> IpKey {
        match self {
            IpKey::V4([a, b, _, _]) => IpKey::V4([a, b, 0, 0]),
            IpKey::V6(bytes) => IpKey::V6(mask_v6(u128::from_be_bytes(bytes), 48)),
            IpKey::Unknown => IpKey::Unknown,
        }
    }
}

fn mask_v6(addr: u128, bits: u32) -> [u8; 16] {
    (addr & (u128::MAX << (128 - bits))).to_be_bytes()
}

/// What a count is kept for.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Key {
    Ip(IpKey),
    /// A network prefix, where IP keys go when their map is full.
    Prefix(IpKey),
    /// A user id.
    Account(String),
    /// A share link id.
    Link(String),
    /// A normalized email.
    Email(String),
    Global,
    /// One of [`SHARDS`] buckets, where other keys go when their map is full.
    Shard(u16),
}

impl Key {
    /// The coarser bucket this key counts in when its map is full.
    fn overflow(&self) -> Key {
        match self {
            Key::Ip(ip) => Key::Prefix(ip.prefix()),
            other => Key::Shard(shard_of(other)),
        }
    }
}

fn shard_of<T: Hash>(value: &T) -> u16 {
    // SHARDS is 4096, so the remainder always fits.
    (SHARD_HASHER.hash_one(value) % SHARDS) as u16
}

/// The client address that stage 1 resolved, left in the request extensions for later stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub IpKey);

/// The client IP. With no peer, nothing is trusted and the key is [`IpKey::Unknown`]. The
/// header, when one is named, is trusted only when the peer is loopback or private, that is, a
/// proxy in front of Galley. Its last line is used, and for `X-Forwarded-For` its last entry. A
/// missing, untrusted or unparsable header falls back to the peer.
pub fn client_ip(headers: &HeaderMap, peer: Option<IpAddr>, header: Option<&HeaderName>) -> IpKey {
    let Some(peer) = peer.map(|p| p.to_canonical()) else { return IpKey::Unknown };
    if let Some(name) = header {
        if is_trusted_proxy(peer) {
            if let Some(ip) = header_ip(headers, name) {
                return IpKey::from_ip(ip);
            }
        }
    }
    IpKey::from_ip(peer)
}

fn header_ip(headers: &HeaderMap, name: &HeaderName) -> Option<IpAddr> {
    let value = headers.get_all(name).iter().next_back()?.to_str().ok()?;
    let entry = if name.as_str().eq_ignore_ascii_case("x-forwarded-for") { value.rsplit(',').next()? } else { value };
    entry.trim().parse().ok()
}

/// Loopback, or a private network: 10/8, 172.16/12, 192.168/16, ::1 and fc00::/7 (Fly's proxy
/// connects from fdaa:).
fn is_trusted_proxy(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(a) => a.is_loopback() || a.is_private(),
        IpAddr::V6(a) => a.is_loopback() || (a.segments()[0] & 0xfe00) == 0xfc00,
    }
}

/// The `Retry-After` seconds for a wait: rounded up, and at least 1.
pub fn retry_after_secs(wait: Duration) -> u64 {
    wait.as_secs().saturating_add(u64::from(wait.subsec_nanos() > 0)).max(1)
}

#[derive(Debug, Clone, Copy)]
struct Window {
    start: Instant,
    count: u32,
}

#[derive(Clone, Copy)]
enum Op {
    /// Count, unless the window is full.
    Hit,
    /// Only look.
    Check,
    /// Count even when the window is full.
    Record,
}

#[derive(Default)]
struct Table {
    windows: HashMap<Key, Window>,
    /// Keys that arrived while `windows` was full, counted by network prefix or shard. Prefix
    /// keys stop at `max_entries`; past that they go to shards too, so this holds at most
    /// `max_entries + SHARDS` entries.
    overflow: HashMap<Key, Window>,
    last_prune: Option<Instant>,
    last_warn: Option<Instant>,
}

fn since(at: Option<Instant>, now: Instant) -> Option<Duration> {
    at.map(|at| now.saturating_duration_since(at))
}

impl Table {
    fn prune(&mut self, window: Duration, now: Instant) {
        let live = |w: &mut Window| w.start.checked_add(window).is_some_and(|end| end > now);
        self.windows.retain(|_, w| live(w));
        self.overflow.retain(|_, w| live(w));
        self.last_prune = Some(now);
    }

    fn apply(&mut self, id: RuleId, rule: Rule, max_entries: usize, key: Key, op: Op, now: Instant) -> Result<(), Duration> {
        let window = Duration::from_secs(rule.window_secs.min(MAX_WINDOW_SECS));
        if since(self.last_prune, now).is_none_or(|d| d >= PRUNE_EVERY) {
            self.prune(window, now);
        }
        if let Some(w) = self.windows.get_mut(&key) {
            return step(w, rule, window, op, now);
        }
        if matches!(op, Op::Check) {
            return match self.overflow.get_mut(&key.overflow()) {
                Some(w) => step(w, rule, window, op, now),
                None => Ok(()),
            };
        }
        if self.windows.len() >= max_entries && since(self.last_prune, now).is_none_or(|d| d >= FULL_PRUNE_EVERY) {
            self.prune(window, now);
        }
        if self.windows.len() < max_entries {
            let w = self.windows.entry(key).or_insert(Window { start: now, count: 0 });
            return step(w, rule, window, op, now);
        }
        if since(self.last_warn, now).is_none_or(|d| d >= WARN_EVERY) {
            self.last_warn = Some(now);
            tracing::warn!(rule = ?id, max_entries, "a rate limit map is full; new clients count in shared network or shard buckets");
        }
        let mut bucket = key.overflow();
        if matches!(bucket, Key::Prefix(_)) && !self.overflow.contains_key(&bucket) && self.overflow.len() >= max_entries {
            bucket = Key::Shard(shard_of(&bucket));
        }
        let w = self.overflow.entry(bucket).or_insert(Window { start: now, count: 0 });
        step(w, rule, window, op, now)
    }
}

/// One fixed window. `Err` is the time until it ends.
fn step(w: &mut Window, rule: Rule, window: Duration, op: Op, now: Instant) -> Result<(), Duration> {
    if w.start.checked_add(window).is_some_and(|end| now >= end) {
        *w = Window { start: now, count: 0 };
    }
    let wait = w.start.checked_add(window).map_or(window, |end| end.saturating_duration_since(now));
    match op {
        Op::Record => {
            w.count = w.count.saturating_add(1);
            Ok(())
        }
        _ if w.count >= rule.count => Err(wait),
        Op::Check => Ok(()),
        Op::Hit => {
            w.count = w.count.saturating_add(1);
            Ok(())
        }
    }
}

/// The counters for every rule. When `enabled` is false, every method allows and nothing is
/// stored. Each `*_at` form takes the time, for tests.
pub struct Limiter {
    config: LimitsConfig,
    ip_header: Option<HeaderName>,
    tables: Vec<Mutex<Table>>,
    /// The login lock: failures per (email, IP) and recent successes per (email, network).
    login: Mutex<LoginTable>,
    /// One queue per account for MCP requests, removed when idle.
    mcp: Mutex<McpQueues>,
}

impl Limiter {
    pub fn new(config: &LimitsConfig) -> Limiter {
        let name = config.client_ip_header.trim();
        let ip_header = if name.is_empty() {
            None
        } else {
            match HeaderName::from_bytes(name.as_bytes()) {
                Ok(h) => Some(h),
                Err(_) => {
                    tracing::warn!("limits.client_ip_header is not a valid header name; using the TCP peer address");
                    None
                }
            }
        };
        Limiter {
            config: config.clone(),
            ip_header,
            tables: RuleId::ALL.iter().map(|_| Mutex::default()).collect(),
            login: Mutex::default(),
            mcp: Mutex::default(),
        }
    }

    pub fn enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn config(&self) -> &LimitsConfig {
        &self.config
    }

    /// The client IP of a request, by the configured header and the rules of [`client_ip`].
    pub fn client_ip(&self, headers: &HeaderMap, peer: Option<IpAddr>) -> IpKey {
        client_ip(headers, peer, self.ip_header.as_ref())
    }

    fn apply(&self, id: RuleId, key: Key, op: Op, now: Instant) -> Result<(), Duration> {
        let rule = id.rule(&self.config);
        if !self.config.enabled || rule.count == 0 {
            return Ok(());
        }
        let mut table = self.tables[id.index()].lock().unwrap_or_else(PoisonError::into_inner);
        table.apply(id, rule, self.config.max_entries, key, op, now)
    }

    /// Count one request for `key`, unless the window is already full.
    pub fn hit(&self, id: RuleId, key: Key) -> Result<(), Duration> {
        self.hit_at(id, key, Instant::now())
    }

    pub fn hit_at(&self, id: RuleId, key: Key, now: Instant) -> Result<(), Duration> {
        self.apply(id, key, Op::Hit, now)
    }

    /// Whether a hit would be allowed, without counting it.
    pub fn check(&self, id: RuleId, key: Key) -> Result<(), Duration> {
        self.check_at(id, key, Instant::now())
    }

    pub fn check_at(&self, id: RuleId, key: Key, now: Instant) -> Result<(), Duration> {
        self.apply(id, key, Op::Check, now)
    }

    /// Count one event that already happened, even past the limit.
    pub fn record(&self, id: RuleId, key: Key) {
        self.record_at(id, key, Instant::now())
    }

    pub fn record_at(&self, id: RuleId, key: Key, now: Instant) {
        let _ = self.apply(id, key, Op::Record, now);
    }

    /// Stage 1 for a request in `group` from `ip`: the group's IP rule, its global rule, and
    /// `default_ip`. Every rule is checked first and nothing counts unless all allow, so a
    /// refused request does not use up a shared budget. The longest wait wins.
    pub fn stage1(&self, group: Group, ip: IpKey) -> Result<(), Duration> {
        self.stage1_at(group, ip, Instant::now())
    }

    pub fn stage1_at(&self, group: Group, ip: IpKey, now: Instant) -> Result<(), Duration> {
        if !self.config.enabled {
            return Ok(());
        }
        let mut hits: Vec<(RuleId, Key)> = Vec::with_capacity(3);
        if let Some(id) = group.ip_rule() {
            hits.push((id, Key::Ip(ip)));
        }
        if group == Group::Join {
            hits.push((RuleId::JoinGlobal, Key::Global));
        }
        hits.push((RuleId::DefaultIp, Key::Ip(ip)));
        let mut checks = hits.iter().map(|(id, key)| self.check_at(*id, key.clone(), now)).collect::<Vec<_>>();
        // Signups are recorded only when an account is created; here the budget is only looked at.
        if group == Group::Signup {
            checks.push(self.check_at(RuleId::SignupGlobal, Key::Global, now));
        }
        longest(checks)?;
        longest(hits.into_iter().map(|(id, key)| self.hit_at(id, key, now)))
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

/// An email as the login lock keys it: trimmed and lowercased like `store::create_user`, and cut
/// to [`MAX_EMAIL_KEY`] bytes.
pub fn email_key(email: &str) -> String {
    let mut e = email.trim().to_lowercase();
    if e.len() > MAX_EMAIL_KEY {
        let mut end = MAX_EMAIL_KEY;
        while !e.is_char_boundary(end) {
            end -= 1;
        }
        e.truncate(end);
    }
    e
}

/// The lock after `count` failures, once `count` has reached `threshold`: 1 s, doubling with each
/// further failure, up to [`MAX_LOGIN_LOCK`]. The shift is capped at 10, so it cannot overflow.
pub fn login_lock(count: u32, threshold: u32) -> Duration {
    let over = count.saturating_sub(threshold).min(10);
    Duration::from_secs(1u64 << over).min(MAX_LOGIN_LOCK)
}

/// Failed logins for one (email, client IP).
#[derive(Debug, Clone, Copy)]
struct Failures {
    count: u32,
    in_flight: u32,
    last: Instant,
}

impl Failures {
    /// The time left on this record's lock, if it is locked.
    fn locked_for(&self, threshold: u32, now: Instant) -> Option<Duration> {
        if threshold == 0 || self.count < threshold {
            return None;
        }
        let lock = login_lock(self.count, threshold);
        let elapsed = now.saturating_duration_since(self.last);
        (elapsed < lock).then(|| lock - elapsed)
    }
}

type LoginKey = (String, IpKey);

#[derive(Default)]
struct LoginTable {
    records: HashMap<LoginKey, Failures>,
    /// (email, network prefix) → the last successful sign-in from there.
    successes: HashMap<LoginKey, Instant>,
    last_prune: Option<Instant>,
    last_warn: Option<Instant>,
}

impl LoginTable {
    /// Drop idle records older than [`LOGIN_RECORD_TTL`] and successes older than
    /// [`LOGIN_SUCCESS_TTL`]. A record with a request in flight is always kept.
    fn prune(&mut self, now: Instant) {
        self.records.retain(|_, r| r.in_flight > 0 || now.saturating_duration_since(r.last) < LOGIN_RECORD_TTL);
        self.successes.retain(|_, at| now.saturating_duration_since(*at) < LOGIN_SUCCESS_TTL);
        self.last_prune = Some(now);
    }

    fn recent_success(&self, email: &str, prefix: IpKey, now: Instant) -> bool {
        self.successes
            .get(&(email.to_string(), prefix))
            .is_some_and(|at| now.saturating_duration_since(*at) < LOGIN_SUCCESS_TTL)
    }

    /// Make room for one more record when the map is full: prune, then evict the oldest eighth of
    /// the records that are neither locked nor in flight. Returns whether there is room.
    fn make_room(&mut self, threshold: u32, max_entries: usize, now: Instant) -> bool {
        if self.records.len() < max_entries {
            return true;
        }
        self.prune(now);
        if self.records.len() >= max_entries {
            let mut idle: Vec<(Instant, LoginKey)> = self
                .records
                .iter()
                .filter(|(_, r)| r.in_flight == 0 && r.locked_for(threshold, now).is_none())
                .map(|(k, r)| (r.last, k.clone()))
                .collect();
            if !idle.is_empty() {
                let evict = (idle.len() / 8).max(1);
                if evict < idle.len() {
                    idle.select_nth_unstable_by_key(evict - 1, |(last, _)| *last);
                }
                for (_, key) in idle.into_iter().take(evict) {
                    self.records.remove(&key);
                }
            }
        }
        if self.records.len() < max_entries {
            return true;
        }
        if since(self.last_warn, now).is_none_or(|d| d >= WARN_EVERY) {
            self.last_warn = Some(now);
            tracing::warn!(max_entries, "the login lock map is full of locked or active records; new attempts are not tracked per (email, IP)");
        }
        false
    }

    fn record_success(&mut self, key: LoginKey, max_entries: usize, now: Instant) {
        if !self.successes.contains_key(&key) && self.successes.len() >= max_entries {
            self.successes.retain(|_, at| now.saturating_duration_since(*at) < LOGIN_SUCCESS_TTL);
            if self.successes.len() >= max_entries {
                let mut all: Vec<(Instant, LoginKey)> = self.successes.iter().map(|(k, at)| (*at, k.clone())).collect();
                let evict = (all.len() / 8).max(1);
                if evict < all.len() {
                    all.select_nth_unstable_by_key(evict - 1, |(at, _)| *at);
                }
                for (_, k) in all.into_iter().take(evict) {
                    self.successes.remove(&k);
                }
            }
        }
        if max_entries > 0 {
            self.successes.insert(key, now);
        }
    }
}

/// How a login attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Failed,
    Succeeded,
    /// The attempt never checked a password (it timed out waiting for a password slot). It only
    /// gives back its place in flight.
    Released,
}

/// One login attempt that the lock let through. Settle it with [`LoginTicket::failed`],
/// [`LoginTicket::succeeded`] or [`LoginTicket::released`]. A ticket dropped unsettled (a client
/// that went away, an early return) counts as a failure.
pub struct LoginTicket {
    limiter: Option<Arc<Limiter>>,
    key: LoginKey,
    /// Whether `login_begin` counted this ticket in its record's `in_flight`.
    tracked: bool,
}

impl LoginTicket {
    pub fn failed(self) {
        self.failed_at(Instant::now())
    }

    pub fn failed_at(mut self, now: Instant) {
        self.settle(Outcome::Failed, now)
    }

    pub fn succeeded(self) {
        self.succeeded_at(Instant::now())
    }

    pub fn succeeded_at(mut self, now: Instant) {
        self.settle(Outcome::Succeeded, now)
    }

    /// Neither a failure nor a success: the attempt was refused before any password was checked
    /// (no password slot came free in time). It frees its place in flight and counts nothing, in
    /// the (email, IP) record or in the email-wide window, so a flood that fills the password
    /// slots cannot lock anyone out.
    pub fn released(mut self) {
        self.settle(Outcome::Released, Instant::now())
    }

    fn settle(&mut self, outcome: Outcome, now: Instant) {
        // Taking the limiter makes a second settle (from Drop) a no-op.
        if let Some(limiter) = self.limiter.take() {
            limiter.login_settle(&self.key, self.tracked, outcome, now);
        }
    }
}

/// Leaves the email out, so a ticket never reaches a log.
impl std::fmt::Debug for LoginTicket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginTicket").field("tracked", &self.tracked).finish_non_exhaustive()
    }
}

impl Drop for LoginTicket {
    fn drop(&mut self) {
        self.settle(Outcome::Failed, Instant::now());
    }
}

#[derive(Default)]
struct McpQueues {
    queues: HashMap<Key, Arc<Semaphore>>,
    last_warn: Option<Instant>,
}

/// An account's turn to run an MCP request. Dropping it lets the next request in, and removes the
/// account's queue when nobody else holds or waits for it.
pub struct McpTurn {
    limiter: Option<Arc<Limiter>>,
    key: Key,
    queue: Option<Arc<Semaphore>>,
    permit: Option<OwnedSemaphorePermit>,
}

impl std::fmt::Debug for McpTurn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpTurn").field("held", &self.permit.is_some()).finish_non_exhaustive()
    }
}

impl Drop for McpTurn {
    fn drop(&mut self) {
        drop(self.permit.take());
        let (Some(limiter), Some(queue)) = (self.limiter.take(), self.queue.take()) else { return };
        let mut queues = lock(&limiter.mcp);
        // Every clone of a queue is made under this lock, so two references (the map's and this
        // one) mean nobody else holds or waits for it.
        if queues.queues.get(&self.key).is_some_and(|q| Arc::ptr_eq(q, &queue)) && Arc::strong_count(&queue) == 2 {
            queues.queues.remove(&self.key);
        }
    }
}

impl Limiter {
    /// How long a login or a signup waits for a password slot before 429, or `None` (no limit) when limits
    /// are off.
    pub fn password_wait(&self) -> Option<Duration> {
        self.config
            .enabled
            .then(|| Duration::from_secs(self.config.password_wait_secs.min(MAX_WINDOW_SECS)))
    }

    /// Start a login attempt for `email` from `ip`, or refuse with the time to wait. In order:
    /// 1. the (email, IP) lock after `login_failures` failures, doubling from 1 s to 15 min;
    /// 2. attempts in flight: below the threshold, failures plus attempts in flight may not reach
    ///    it; at or past it, one attempt at a time;
    /// 3. the email-wide lock after `login_email_failures`, except from a network where this
    ///    email signed in successfully in the last 30 days.
    ///
    /// Unknown emails are tracked like known ones. A refusal is not a failure.
    pub fn login_begin(self: &Arc<Self>, email: &str, ip: IpKey) -> Result<LoginTicket, Duration> {
        self.login_begin_at(email, ip, Instant::now())
    }

    pub fn login_begin_at(self: &Arc<Self>, email: &str, ip: IpKey, now: Instant) -> Result<LoginTicket, Duration> {
        let email = email_key(email);
        if !self.config.enabled {
            return Ok(LoginTicket { limiter: None, key: (email, ip), tracked: false });
        }
        let threshold = self.config.login_failures;
        // Looked at before the login lock is taken, so the two locks are never held together.
        let email_full = self.check_at(RuleId::LoginEmailFailures, Key::Email(email.clone()), now).err();
        let key = (email, ip);
        let mut table = lock(&self.login);
        if since(table.last_prune, now).is_none_or(|d| d >= PRUNE_EVERY) {
            table.prune(now);
        }
        if let Some(r) = table.records.get(&key) {
            if let Some(wait) = r.locked_for(threshold, now) {
                return Err(wait);
            }
            if threshold > 0 && r.in_flight >= threshold.saturating_sub(r.count).max(1) {
                return Err(Duration::from_secs(1));
            }
        }
        if let Some(wait) = email_full {
            if !table.recent_success(&key.0, ip.prefix(), now) {
                return Err(wait);
            }
        }
        let tracked = threshold > 0
            && match table.records.get_mut(&key) {
                Some(r) => {
                    r.in_flight = r.in_flight.saturating_add(1);
                    true
                }
                None => {
                    let room = table.make_room(threshold, self.config.max_entries, now);
                    if room {
                        table.records.insert(key.clone(), Failures { count: 0, in_flight: 1, last: now });
                    }
                    room
                }
            };
        drop(table);
        Ok(LoginTicket { limiter: Some(Arc::clone(self)), key, tracked })
    }

    fn login_settle(&self, key: &LoginKey, tracked: bool, outcome: Outcome, now: Instant) {
        let threshold = self.config.login_failures;
        if outcome == Outcome::Released {
            let mut table = lock(&self.login);
            if let Some(r) = table.records.get_mut(key) {
                if tracked {
                    r.in_flight = r.in_flight.saturating_sub(1);
                }
                // A record that this attempt alone created holds nothing worth keeping.
                if r.count == 0 && r.in_flight == 0 {
                    table.records.remove(key);
                }
            }
            return;
        }
        let ok = outcome == Outcome::Succeeded;
        {
            let mut table = lock(&self.login);
            let room = table.records.len() < self.config.max_entries;
            match table.records.get_mut(key) {
                Some(r) => {
                    if tracked {
                        r.in_flight = r.in_flight.saturating_sub(1);
                    }
                    if ok {
                        r.count = 0;
                        if r.in_flight == 0 {
                            table.records.remove(key);
                        }
                    } else {
                        r.count = r.count.saturating_add(1);
                        r.last = now;
                    }
                }
                // The record is gone (cleared by a success, or never stored while the map was
                // full). A failure starts a new one when there is room.
                None if !ok && threshold > 0 && room => {
                    table.records.insert(key.clone(), Failures { count: 1, in_flight: 0, last: now });
                }
                None => {}
            }
            if ok {
                table.record_success((key.0.clone(), key.1.prefix()), self.config.max_entries, now);
            }
        }
        // A success does not clear the email-wide window.
        if !ok {
            self.record_at(RuleId::LoginEmailFailures, Key::Email(key.0.clone()), now);
        }
    }

    /// Wait for this account's turn to run an MCP request: `mcp_concurrent` at a time per
    /// account, waiting at most `mcp_wait_secs`. `Err` is the wait that ran out.
    pub async fn mcp_turn(self: &Arc<Self>, user_id: &str) -> Result<McpTurn, Duration> {
        let n = self.config.mcp_concurrent;
        if !self.config.enabled || n == 0 {
            return Ok(McpTurn { limiter: None, key: Key::Global, queue: None, permit: None });
        }
        let (key, queue) = {
            let mut queues = lock(&self.mcp);
            let mut key = Key::Account(user_id.to_string());
            if !queues.queues.contains_key(&key) && queues.queues.len() >= self.config.max_entries {
                key = key.overflow();
                let now = Instant::now();
                if since(queues.last_warn, now).is_none_or(|d| d >= WARN_EVERY) {
                    queues.last_warn = Some(now);
                    tracing::warn!(max_entries = self.config.max_entries, "the MCP queue map is full; new accounts share hashed queues");
                }
            }
            let permits = usize::try_from(n).unwrap_or(usize::MAX).min(Semaphore::MAX_PERMITS);
            let queue = Arc::clone(queues.queues.entry(key.clone()).or_insert_with(|| Arc::new(Semaphore::new(permits))));
            (key, queue)
        };
        let acquire = Arc::clone(&queue).acquire_owned();
        let mut turn = McpTurn { limiter: Some(Arc::clone(self)), key, queue: Some(queue), permit: None };
        let wait = Duration::from_secs(self.config.mcp_wait_secs.min(MAX_WINDOW_SECS));
        match tokio::time::timeout(wait, acquire).await {
            Ok(Ok(permit)) => {
                turn.permit = Some(permit);
                Ok(turn)
            }
            // The turn drops here and cleans up the queue if it is idle.
            _ => Err(wait),
        }
    }
}

/// `Ok` when every result is, else the longest wait.
fn longest(results: impl IntoIterator<Item = Result<(), Duration>>) -> Result<(), Duration> {
    // Every result is looked at (each hit has already counted), so this does not stop early.
    match results.into_iter().filter_map(Result::err).max() {
        Some(wait) => Err(wait),
        None => Ok(()),
    }
}

/// Stage 1, on the whole router. It runs before every route layer and extractor, resolves the
/// client IP once (left in the extensions as [`ClientIp`]), and answers 429 when the request's
/// group or `default_ip` is over its limit; the single sign-on group redirects to the sign-in
/// screen with the message instead. It reads no body and touches no database.
pub async fn enforce(State(app): State<AppState>, mut request: Request, next: Next) -> Response {
    let peer = request.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let ip = app.limits.client_ip(request.headers(), peer);
    request.extensions_mut().insert(ClientIp(ip));
    if app.limits.enabled() {
        let group = group_for(request.method(), request.extensions().get::<MatchedPath>().map(MatchedPath::as_str));
        if let Err(wait) = app.limits.stage1(group, ip) {
            // The single sign-on routes are browser navigations, not API calls: they go back to
            // the sign-in screen with the fixed message (SPEC 10.1), never to a page of JSON.
            if group == Group::Oidc {
                return crate::auth_routes::signin_error(TOO_MANY).into_response();
            }
            return AppError::TooManyRequests(wait).into_response();
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    fn config(adjust: impl FnOnce(&mut LimitsConfig)) -> LimitsConfig {
        let mut c = LimitsConfig::default();
        adjust(&mut c);
        c
    }

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    fn headers(name: &str, values: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        let name = HeaderName::from_bytes(name.as_bytes()).unwrap();
        for v in values {
            h.append(name.clone(), v.parse().unwrap());
        }
        h
    }

    #[test]
    fn a_window_refuses_past_its_count_and_opens_again() {
        let l = Limiter::new(&config(|c| c.login_ip = Rule::new(2, 10)));
        let t = Instant::now();
        let a = Key::Ip(IpKey::from_ip(ip("198.51.100.1")));
        assert!(l.hit_at(RuleId::LoginIp, a.clone(), t).is_ok());
        assert!(l.hit_at(RuleId::LoginIp, a.clone(), t + Duration::from_secs(1)).is_ok());
        let wait = l.hit_at(RuleId::LoginIp, a.clone(), t + Duration::from_secs(4)).unwrap_err();
        assert_eq!(wait, Duration::from_secs(6));
        assert!(l.check_at(RuleId::LoginIp, a.clone(), t + Duration::from_secs(4)).is_err());
        let b = Key::Ip(IpKey::from_ip(ip("198.51.100.2")));
        assert!(l.hit_at(RuleId::LoginIp, b, t + Duration::from_secs(4)).is_ok(), "another client has its own budget");
        assert!(l.hit_at(RuleId::LoginIp, a, t + Duration::from_secs(10)).is_ok(), "the window ended");
    }

    #[test]
    fn check_does_not_count_and_record_counts_past_the_limit() {
        let l = Limiter::new(&config(|c| c.signup_global = Rule::new(1, 60)));
        let t = Instant::now();
        for _ in 0..5 {
            assert!(l.check_at(RuleId::SignupGlobal, Key::Global, t).is_ok());
        }
        l.record_at(RuleId::SignupGlobal, Key::Global, t);
        assert!(l.check_at(RuleId::SignupGlobal, Key::Global, t).is_err());
        l.record_at(RuleId::SignupGlobal, Key::Global, t);
        assert!(l.hit_at(RuleId::SignupGlobal, Key::Global, t).is_err());
    }

    #[test]
    fn a_rule_with_count_zero_is_off_and_disabled_limits_store_nothing() {
        let l = Limiter::new(&config(|c| c.login_ip = Rule::new(0, 60)));
        let t = Instant::now();
        for _ in 0..100 {
            assert!(l.hit_at(RuleId::LoginIp, Key::Global, t).is_ok());
        }
        let off = Limiter::new(&config(|c| {
            c.enabled = false;
            c.login_ip = Rule::new(1, 60);
            c.default_ip = Rule::new(1, 60);
        }));
        for _ in 0..100 {
            assert!(off.hit_at(RuleId::LoginIp, Key::Global, t).is_ok());
            assert!(off.stage1_at(Group::Login, IpKey::Unknown, t).is_ok());
        }
        assert!(off.tables.iter().all(|t| {
            let t = t.lock().unwrap();
            t.windows.is_empty() && t.overflow.is_empty()
        }));
    }

    #[test]
    fn counts_saturate_instead_of_overflowing() {
        let l = Limiter::new(&config(|c| c.login_ip = Rule::new(u32::MAX, 60)));
        let t = Instant::now();
        {
            let mut table = l.tables[RuleId::LoginIp.index()].lock().unwrap();
            table.windows.insert(Key::Global, Window { start: t, count: u32::MAX - 1 });
        }
        assert!(l.hit_at(RuleId::LoginIp, Key::Global, t).is_ok());
        assert!(l.hit_at(RuleId::LoginIp, Key::Global, t).is_err());
        l.record_at(RuleId::LoginIp, Key::Global, t);
        assert_eq!(l.tables[RuleId::LoginIp.index()].lock().unwrap().windows[&Key::Global].count, u32::MAX);
        let huge = Limiter::new(&config(|c| c.login_ip = Rule::new(1, u64::MAX)));
        assert!(huge.hit_at(RuleId::LoginIp, Key::Global, t).is_ok());
        let wait = huge.hit_at(RuleId::LoginIp, Key::Global, t).unwrap_err();
        assert_eq!(wait, Duration::from_secs(MAX_WINDOW_SECS));
    }

    #[test]
    fn a_full_map_counts_new_ips_under_their_prefix_and_accounts_under_a_shard() {
        let l = Limiter::new(&config(|c| {
            c.max_entries = 2;
            c.login_ip = Rule::new(1, 600);
            c.build_account = Rule::new(1, 600);
            c.signup_ip = Rule::new(1, 600);
        }));
        let t = Instant::now();
        let key = |s: &str| Key::Ip(IpKey::from_ip(ip(s)));
        assert!(l.hit_at(RuleId::LoginIp, key("198.51.100.1"), t).is_ok());
        assert!(l.hit_at(RuleId::LoginIp, key("198.51.100.2"), t).is_ok());
        // The map is full: the third address counts under 198.51.0.0/16.
        assert!(l.hit_at(RuleId::LoginIp, key("198.51.7.3"), t).is_ok());
        assert!(l.hit_at(RuleId::LoginIp, key("198.51.9.9"), t).is_err(), "the same /16 shares the overflow bucket");
        assert!(l.hit_at(RuleId::LoginIp, key("203.0.113.9"), t).is_ok(), "another network has its own bucket");
        {
            let table = l.tables[RuleId::LoginIp.index()].lock().unwrap();
            assert_eq!(table.windows.len(), 2);
            assert!(table.overflow.contains_key(&Key::Prefix(IpKey::V4([198, 51, 0, 0]))));
        }
        // Clients already in the map keep their own windows.
        assert!(l.hit_at(RuleId::LoginIp, key("198.51.100.1"), t).is_err());

        assert!(l.hit_at(RuleId::BuildAccount, Key::Account("u1".into()), t).is_ok());
        assert!(l.hit_at(RuleId::BuildAccount, Key::Account("u2".into()), t).is_ok());
        assert!(l.hit_at(RuleId::BuildAccount, Key::Account("u3".into()), t).is_ok());
        assert!(l.hit_at(RuleId::BuildAccount, Key::Account("u3".into()), t).is_err(), "the shard counts");
        {
            let table = l.tables[RuleId::BuildAccount.index()].lock().unwrap();
            assert!(table.overflow.keys().all(|k| matches!(k, Key::Shard(s) if u64::from(*s) < SHARDS)));
        }

        // Another rule's map is untouched.
        assert!(l.hit_at(RuleId::SignupIp, key("198.51.9.9"), t).is_ok());

        // Once the windows end, a prune makes room again.
        let later = t + Duration::from_secs(601);
        assert!(l.hit_at(RuleId::LoginIp, key("192.0.2.1"), later).is_ok());
        let table = l.tables[RuleId::LoginIp.index()].lock().unwrap();
        assert_eq!(table.windows.len(), 1);
        assert!(table.overflow.is_empty());
    }

    #[test]
    fn the_overflow_map_is_bounded_too() {
        let l = Limiter::new(&config(|c| {
            c.max_entries = 1;
            c.login_ip = Rule::new(1000, 600);
        }));
        let t = Instant::now();
        for n in 0..50u16 {
            let addr = Ipv6Addr::new(0x2001, 0xdb8, n, 0, 0, 0, 0, 1);
            assert!(l.hit_at(RuleId::LoginIp, Key::Ip(IpKey::from_ip(IpAddr::V6(addr))), t).is_ok());
        }
        let table = l.tables[RuleId::LoginIp.index()].lock().unwrap();
        assert_eq!(table.windows.len(), 1);
        let prefixes = table.overflow.keys().filter(|k| matches!(k, Key::Prefix(_))).count();
        assert_eq!(prefixes, 1, "prefix buckets stop at max_entries");
        assert!(table.overflow.len() <= 1 + SHARDS as usize);
    }

    #[test]
    fn stage1_counts_nothing_when_one_rule_refuses_and_the_longest_wait_wins() {
        let l = Limiter::new(&config(|c| {
            c.join_ip = Rule::new(1, 3600);
            c.join_global = Rule::new(2, 600);
            c.default_ip = Rule::new(100, 60);
        }));
        let t = Instant::now();
        let a = IpKey::from_ip(ip("198.51.100.1"));
        assert!(l.stage1_at(Group::Join, a, t).is_ok());
        for _ in 0..10 {
            assert_eq!(l.stage1_at(Group::Join, a, t).unwrap_err(), Duration::from_secs(3600));
        }
        let b = IpKey::from_ip(ip("198.51.100.2"));
        assert!(l.stage1_at(Group::Join, b, t).is_ok(), "refused joins did not use up the global budget");
        // The global rule is now full for everyone.
        let c = IpKey::from_ip(ip("198.51.100.3"));
        assert_eq!(l.stage1_at(Group::Join, c, t).unwrap_err(), Duration::from_secs(600));
        // For `a` both rules refuse, and the longer wait is reported.
        assert_eq!(l.stage1_at(Group::Join, a, t).unwrap_err(), Duration::from_secs(3600));
        assert!(longest([Err(Duration::from_secs(5)), Ok(()), Err(Duration::from_secs(9)), Err(Duration::from_secs(1))]) == Err(Duration::from_secs(9)));
        assert!(longest([Ok(()), Ok(())]).is_ok());
    }

    #[test]
    fn stage1_checks_but_does_not_record_signup_global() {
        let l = Limiter::new(&config(|c| {
            c.signup_ip = Rule::new(100, 3600);
            c.signup_global = Rule::new(1, 3600);
        }));
        let t = Instant::now();
        let a = IpKey::from_ip(ip("198.51.100.1"));
        for _ in 0..5 {
            assert!(l.stage1_at(Group::Signup, a, t).is_ok());
        }
        l.record_at(RuleId::SignupGlobal, Key::Global, t);
        assert!(l.stage1_at(Group::Signup, a, t).is_err());
    }

    #[test]
    fn every_request_counts_against_default_ip() {
        let l = Limiter::new(&config(|c| c.default_ip = Rule::new(3, 60)));
        let t = Instant::now();
        let a = IpKey::from_ip(ip("198.51.100.1"));
        assert!(l.stage1_at(Group::Default, a, t).is_ok());
        assert!(l.stage1_at(Group::Login, a, t).is_ok());
        assert!(l.stage1_at(Group::TokenCreate, a, t).is_ok());
        assert!(l.stage1_at(Group::Default, a, t).is_err());
    }

    #[test]
    fn ips_are_canonical_and_ipv6_is_cut_to_its_slash_64() {
        assert_eq!(IpKey::from_ip(ip("::ffff:1.2.3.4")), IpKey::from_ip(ip("1.2.3.4")));
        assert_eq!(IpKey::from_ip(ip("2001:db8:1:2:aaaa::1")), IpKey::from_ip(ip("2001:db8:1:2:bbbb::ffff")));
        assert_ne!(IpKey::from_ip(ip("2001:db8:1:2::1")), IpKey::from_ip(ip("2001:db8:1:3::1")));
        assert_eq!(IpKey::from_ip(ip("2001:db8:1:2::1")).prefix(), IpKey::from_ip(ip("2001:db8:1:ffff::1")).prefix());
        assert_ne!(IpKey::from_ip(ip("2001:db8:1::1")).prefix(), IpKey::from_ip(ip("2001:db8:2::1")).prefix());
        assert_eq!(IpKey::from_ip(ip("10.1.2.3")).prefix(), IpKey::V4([10, 1, 0, 0]));
        assert_eq!(IpKey::Unknown.prefix(), IpKey::Unknown);
    }

    #[test]
    fn the_client_ip_header_is_trusted_only_from_a_private_peer() {
        let fly = HeaderName::from_static("fly-client-ip");
        let h = headers("Fly-Client-IP", &["198.51.100.7"]);
        let want = IpKey::from_ip(ip("198.51.100.7"));
        assert_eq!(client_ip(&h, Some(ip("127.0.0.1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("::ffff:127.0.0.1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("10.0.0.1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("172.31.0.1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("192.168.1.1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("::1")), Some(&fly)), want);
        assert_eq!(client_ip(&h, Some(ip("fdaa:0:1::2")), Some(&fly)), want);
        // A public peer, or none, is never believed.
        assert_eq!(client_ip(&h, Some(ip("203.0.113.5")), Some(&fly)), IpKey::from_ip(ip("203.0.113.5")));
        assert_eq!(client_ip(&h, Some(ip("172.32.0.1")), Some(&fly)), IpKey::from_ip(ip("172.32.0.1")));
        assert_eq!(client_ip(&h, Some(ip("2001:db8::1")), Some(&fly)), IpKey::from_ip(ip("2001:db8::1")));
        assert_eq!(client_ip(&h, None, Some(&fly)), IpKey::Unknown);
        // No header configured: the peer.
        assert_eq!(client_ip(&h, Some(ip("127.0.0.1")), None), IpKey::V4([127, 0, 0, 1]));
    }

    #[test]
    fn the_last_header_line_and_the_last_forwarded_entry_are_used() {
        let fly = HeaderName::from_static("fly-client-ip");
        let xff = HeaderName::from_static("x-forwarded-for");
        let peer = Some(ip("127.0.0.1"));
        let h = headers("Fly-Client-IP", &["198.51.100.1", " 198.51.100.2 "]);
        assert_eq!(client_ip(&h, peer, Some(&fly)), IpKey::from_ip(ip("198.51.100.2")));
        let h = headers("X-Forwarded-For", &["192.0.2.1", "203.0.113.1, 198.51.100.3"]);
        assert_eq!(client_ip(&h, peer, Some(&xff)), IpKey::from_ip(ip("198.51.100.3")));
        // Garbage, an empty last entry, or a missing header fall back to the peer.
        for bad in ["not an ip", "198.51.100.1, ", "198.51.100.1:80", ""] {
            let h = headers("X-Forwarded-For", &[bad]);
            assert_eq!(client_ip(&h, peer, Some(&xff)), IpKey::V4([127, 0, 0, 1]), "{bad:?}");
        }
        assert_eq!(client_ip(&HeaderMap::new(), peer, Some(&fly)), IpKey::V4([127, 0, 0, 1]));
        // A Fly header with a comma is taken whole, so it does not parse.
        let h = headers("Fly-Client-IP", &["198.51.100.1, 198.51.100.2"]);
        assert_eq!(client_ip(&h, peer, Some(&fly)), IpKey::V4([127, 0, 0, 1]));
    }

    #[test]
    fn an_invalid_header_name_falls_back_to_the_peer() {
        let l = Limiter::new(&config(|c| c.client_ip_header = "bad header".into()));
        let h = headers("Fly-Client-IP", &["198.51.100.7"]);
        assert_eq!(l.client_ip(&h, Some(IpAddr::V4(Ipv4Addr::LOCALHOST))), IpKey::V4([127, 0, 0, 1]));
        let l = Limiter::new(&config(|c| c.client_ip_header = " Fly-Client-IP ".into()));
        assert_eq!(l.client_ip(&h, Some(IpAddr::V4(Ipv4Addr::LOCALHOST))), IpKey::from_ip(ip("198.51.100.7")));
    }

    #[test]
    fn retry_after_rounds_up_to_at_least_one_second() {
        assert_eq!(retry_after_secs(Duration::from_millis(200)), 1);
        assert_eq!(retry_after_secs(Duration::from_millis(1500)), 2);
        assert_eq!(retry_after_secs(Duration::from_secs(600)), 600);
        assert_eq!(retry_after_secs(Duration::ZERO), 1);
        assert_eq!(retry_after_secs(Duration::MAX), u64::MAX);
    }

    #[test]
    fn the_table_sorts_methods_and_paths() {
        let g = |m: Method, p: &str| group_for(&m, Some(p));
        assert_eq!(g(Method::POST, "/api/auth/login"), Group::Login);
        assert_eq!(g(Method::GET, "/api/auth/login"), Group::Default);
        assert_eq!(g(Method::HEAD, "/api/share/{token}"), Group::SharePreview, "HEAD runs the GET handler");
        assert_eq!(g(Method::GET, "/api/projects/{id}/latexdiff/pdf"), Group::Default);
        assert_eq!(g(Method::GET, "/api/projects/{id}/build"), Group::Default);
        assert_eq!(g(Method::GET, "/api/projects/{id}/bib"), Group::Default);
        assert_eq!(g(Method::POST, "/api/projects/{id}/bib"), Group::Lookup);
        assert_eq!(g(Method::GET, "/mcp/{id}"), Group::Default);
        assert_eq!(group_for(&Method::GET, None), Group::Default);
        for group in [Group::TokenCreate, Group::ProjectCreate, Group::Invite, Group::Lookup, Group::Default] {
            assert_eq!(group.ip_rule(), None);
        }
        assert_eq!(Group::Socket.account_rule(), None);
        assert_eq!(Group::Mcp.account_rule(), None);
        for (i, id) in RuleId::ALL.iter().enumerate() {
            assert_eq!(id.index(), i);
        }
    }

    fn login_limiter(adjust: impl FnOnce(&mut LimitsConfig)) -> Arc<Limiter> {
        Arc::new(Limiter::new(&config(|c| {
            c.login_failures = 2;
            adjust(c);
        })))
    }

    fn record(l: &Limiter, email: &str, ip: IpKey) -> Option<Failures> {
        l.login.lock().unwrap().records.get(&(email_key(email), ip)).copied()
    }

    #[test]
    fn the_login_lock_counts_attempts_in_flight_and_doubles() {
        let l = login_limiter(|_| {});
        let a = IpKey::from_ip(ip("198.51.100.1"));
        let t = Instant::now();
        let one = l.login_begin_at("Ann@Example.org ", a, t).unwrap();
        let two = l.login_begin_at("ann@example.org", a, t).unwrap();
        assert_eq!(l.login_begin_at("ann@example.org", a, t).unwrap_err(), Duration::from_secs(1), "two in flight reach the threshold");
        one.failed_at(t);
        assert!(l.login_begin_at("ann@example.org", a, t).is_err(), "one failure and one in flight still reach it");
        two.failed_at(t);
        assert_eq!(record(&l, "ann@example.org", a).unwrap().in_flight, 0);
        assert_eq!(l.login_begin_at("ann@example.org", a, t + Duration::from_millis(999)).unwrap_err(), Duration::from_millis(1));
        // Past the lock, one attempt at a time.
        let three = l.login_begin_at("ann@example.org", a, t + Duration::from_secs(1)).unwrap();
        assert!(l.login_begin_at("ann@example.org", a, t + Duration::from_secs(1)).is_err());
        let t3 = t + Duration::from_secs(1);
        three.failed_at(t3);
        // The third failure doubles the lock to 2 s.
        assert!(l.login_begin_at("ann@example.org", a, t3 + Duration::from_millis(1999)).is_err());
        assert!(l.login_begin_at("ann@example.org", a, t3 + Duration::from_secs(2)).is_ok());
        // Another IP and another email are not locked.
        let b = IpKey::from_ip(ip("198.51.100.2"));
        assert!(l.login_begin_at("ann@example.org", b, t3).is_ok());
        assert!(l.login_begin_at("bob@example.org", a, t3).is_ok());
    }

    #[test]
    fn a_success_clears_the_record_but_not_the_email_window() {
        let l = login_limiter(|c| c.login_email_failures = Rule::new(1, 3600));
        let a = IpKey::from_ip(ip("198.51.100.1"));
        let t = Instant::now();
        l.login_begin_at("ann@example.org", a, t).unwrap().failed_at(t);
        assert_eq!(record(&l, "ann@example.org", a).unwrap().count, 1);
        // The email window is now full, but nothing has succeeded yet: refused everywhere.
        assert!(l.login_begin_at("ann@example.org", a, t).is_err());
        // Let one attempt through by hand and settle it as a success.
        l.login_begin_at("ann@example.org", a, t + Duration::from_secs(3600)).unwrap().succeeded_at(t + Duration::from_secs(3600));
        assert!(record(&l, "ann@example.org", a).is_none(), "the (email, IP) record is cleared");
        let t2 = t + Duration::from_secs(3600);
        l.login_begin_at("ann@example.org", a, t2).unwrap().failed_at(t2);
        assert!(l.check_at(RuleId::LoginEmailFailures, Key::Email("ann@example.org".into()), t2).is_err());
        l.login_begin_at("ann@example.org", a, t2).unwrap().succeeded_at(t2);
        assert!(l.check_at(RuleId::LoginEmailFailures, Key::Email("ann@example.org".into()), t2).is_err(), "a success does not clear the email window");
    }

    #[test]
    fn a_dropped_ticket_counts_as_one_failure() {
        let l = login_limiter(|_| {});
        let a = IpKey::from_ip(ip("198.51.100.1"));
        let t = Instant::now();
        drop(l.login_begin_at("ann@example.org", a, t).unwrap());
        let r = record(&l, "ann@example.org", a).unwrap();
        assert_eq!((r.count, r.in_flight), (1, 0));
        let email_window = |l: &Limiter| l.tables[RuleId::LoginEmailFailures.index()].lock().unwrap().windows[&Key::Email("ann@example.org".into())].count;
        assert_eq!(email_window(&l), 1, "the email-wide window counts it too");
        // A settled ticket is not counted again when it drops.
        l.login_begin_at("ann@example.org", a, t).unwrap().failed_at(t);
        let r = record(&l, "ann@example.org", a).unwrap();
        assert_eq!((r.count, r.in_flight), (2, 0));
        assert_eq!(email_window(&l), 2);
    }

    #[test]
    fn a_released_ticket_frees_its_place_and_counts_nothing() {
        let l = login_limiter(|c| c.login_email_failures = Rule::new(1, 3600));
        let a = IpKey::from_ip(ip("198.51.100.1"));
        let t = Instant::now();
        let email_window = |l: &Limiter| {
            l.tables[RuleId::LoginEmailFailures.index()].lock().unwrap().windows.get(&Key::Email("ann@example.org".into())).map(|w| w.count)
        };
        // Alone, a released ticket leaves no record and no email-wide count.
        l.login_begin_at("ann@example.org", a, t).unwrap().released();
        assert!(record(&l, "ann@example.org", a).is_none());
        assert_eq!(email_window(&l), None);
        // With a failure on record, it keeps the count and only gives back its place in flight.
        l.login_begin_at("bob@example.org", a, t).unwrap().failed_at(t);
        let one = l.login_begin_at("Bob@example.org", a, t + Duration::from_secs(3600)).unwrap();
        assert_eq!(record(&l, "bob@example.org", a).unwrap().in_flight, 1);
        one.released();
        let r = record(&l, "bob@example.org", a).unwrap();
        assert_eq!((r.count, r.in_flight), (1, 0));
        // Many released tickets in a row never lock the email or the (email, IP).
        for _ in 0..20 {
            l.login_begin_at("ann@example.org", a, t).unwrap().released();
        }
        l.login_begin_at("ann@example.org", a, t).expect("not locked").released();
        assert_eq!(email_window(&l), None);
        // The settle runs once: the Drop after a release counts nothing.
        let mut ticket = l.login_begin_at("ann@example.org", a, t).unwrap();
        ticket.settle(Outcome::Released, t);
        drop(ticket);
        assert!(record(&l, "ann@example.org", a).is_none());
        assert_eq!(email_window(&l), None);
    }

    #[test]
    fn huge_failure_counts_do_not_overflow() {
        assert_eq!(login_lock(5, 5), Duration::from_secs(1));
        assert_eq!(login_lock(6, 5), Duration::from_secs(2));
        assert_eq!(login_lock(14, 5), Duration::from_secs(512));
        assert_eq!(login_lock(15, 5), MAX_LOGIN_LOCK);
        assert_eq!(login_lock(1000, 2), MAX_LOGIN_LOCK);
        assert_eq!(login_lock(u32::MAX, 0), MAX_LOGIN_LOCK);
        let l = login_limiter(|_| {});
        let a = IpKey::from_ip(ip("198.51.100.1"));
        let t = Instant::now();
        l.login.lock().unwrap().records.insert((email_key("ann@example.org"), a), Failures { count: u32::MAX, in_flight: u32::MAX, last: t });
        assert_eq!(l.login_begin_at("ann@example.org", a, t).unwrap_err(), MAX_LOGIN_LOCK);
        l.login.lock().unwrap().records.insert((email_key("ann@example.org"), a), Failures { count: 1000, in_flight: 0, last: t });
        assert_eq!(l.login_begin_at("ann@example.org", a, t).unwrap_err(), MAX_LOGIN_LOCK);
        let ticket = l.login_begin_at("ann@example.org", a, t + MAX_LOGIN_LOCK).unwrap();
        l.login.lock().unwrap().records.get_mut(&(email_key("ann@example.org"), a)).unwrap().count = u32::MAX;
        ticket.failed_at(t + MAX_LOGIN_LOCK);
        assert_eq!(record(&l, "ann@example.org", a).unwrap().count, u32::MAX);
    }

    #[test]
    fn the_email_lock_spares_networks_with_a_recent_success() {
        let l = login_limiter(|c| {
            c.login_failures = 100;
            c.login_email_failures = Rule::new(3, 3600);
        });
        let t = Instant::now();
        let home = IpKey::from_ip(ip("192.0.2.10"));
        l.login_begin_at("ann@example.org", home, t).unwrap().succeeded_at(t);
        for n in 1..=3 {
            let attacker = IpKey::from_ip(ip(&format!("203.0.113.{n}")));
            l.login_begin_at("ann@example.org", attacker, t).unwrap().failed_at(t);
        }
        let fourth = IpKey::from_ip(ip("198.51.100.4"));
        assert!(l.login_begin_at("ann@example.org", fourth, t).unwrap_err() > Duration::from_secs(3000));
        // Another address in the home /16 is spared.
        let nearby = IpKey::from_ip(ip("192.0.77.1"));
        assert!(l.login_begin_at("ann@example.org", nearby, t).is_ok());
        // Other emails are not locked.
        assert!(l.login_begin_at("bob@example.org", fourth, t).is_ok());
        // A success older than 30 days no longer counts.
        let later = t + LOGIN_SUCCESS_TTL;
        for n in 1..=3 {
            let attacker = IpKey::from_ip(ip(&format!("203.0.113.{n}")));
            l.login_begin_at("ann@example.org", attacker, later).unwrap().failed_at(later);
        }
        assert!(l.login_begin_at("ann@example.org", nearby, later).is_err());
    }

    #[test]
    fn a_full_login_map_evicts_idle_records_and_never_locked_ones() {
        let l = login_limiter(|c| c.max_entries = 2);
        let t = Instant::now();
        let a = IpKey::from_ip(ip("198.51.100.1"));
        // Lock one record, and keep one attempt in flight on another.
        for _ in 0..2 {
            l.login_begin_at("locked@example.org", a, t).unwrap().failed_at(t);
        }
        let held = l.login_begin_at("busy@example.org", a, t).unwrap();
        // The map is full of a locked and a busy record: a new email is allowed but not tracked.
        let untracked = l.login_begin_at("new@example.org", a, t).unwrap();
        assert!(!untracked.tracked);
        drop(untracked);
        assert_eq!(l.login.lock().unwrap().records.len(), 2);
        assert!(l.login_begin_at("locked@example.org", a, t).is_err(), "a locked record is never evicted");
        // Once the busy attempt fails and the lock ends, idle unlocked records give way.
        held.failed_at(t);
        let later = t + MAX_LOGIN_LOCK;
        let fresh = l.login_begin_at("new@example.org", a, later).unwrap();
        assert!(fresh.tracked);
        assert_eq!(l.login.lock().unwrap().records.len(), 2);
        drop(fresh);
        // Huge emails are cut before they become keys.
        let huge = "x".repeat(10_000);
        assert_eq!(email_key(&huge).len(), MAX_EMAIL_KEY);
        assert_eq!(email_key(&"é".repeat(400)).len(), MAX_EMAIL_KEY);
    }

    #[test]
    fn disabled_limits_give_inert_login_tickets() {
        let l = Arc::new(Limiter::new(&config(|c| {
            c.enabled = false;
            c.login_failures = 1;
        })));
        let a = IpKey::from_ip(ip("198.51.100.1"));
        for _ in 0..10 {
            l.login_begin("ann@example.org", a).unwrap().failed();
        }
        assert!(l.login.lock().unwrap().records.is_empty());
        assert_eq!(l.password_wait(), None);
    }

    #[tokio::test]
    async fn mcp_requests_take_turns_per_account_and_idle_queues_go_away() {
        let l = Arc::new(Limiter::new(&config(|c| c.mcp_wait_secs = 0)));
        let first = l.mcp_turn("u1").await.unwrap();
        assert!(l.mcp_turn("u1").await.is_err(), "the second request waits, here for no time");
        let other = l.mcp_turn("u2").await.unwrap();
        assert_eq!(l.mcp.lock().unwrap().queues.len(), 2);
        drop(first);
        drop(other);
        assert!(l.mcp.lock().unwrap().queues.is_empty(), "idle queues are removed");
        let again = l.mcp_turn("u1").await.unwrap();
        drop(again);
        assert!(l.mcp.lock().unwrap().queues.is_empty());

        // A waiter that gets its turn when the first finishes.
        let l = Arc::new(Limiter::new(&config(|c| c.mcp_wait_secs = 5)));
        let first = l.mcp_turn("u1").await.unwrap();
        let waiter = {
            let l = Arc::clone(&l);
            tokio::spawn(async move { l.mcp_turn("u1").await.map(drop) })
        };
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(!waiter.is_finished());
        drop(first);
        assert!(waiter.await.unwrap().is_ok());
        assert!(l.mcp.lock().unwrap().queues.is_empty());

        // A full map puts new accounts in hashed queues, which are cleaned up too.
        let l = Arc::new(Limiter::new(&config(|c| {
            c.mcp_wait_secs = 0;
            c.max_entries = 1;
        })));
        let a = l.mcp_turn("u1").await.unwrap();
        let b = l.mcp_turn("u2").await.unwrap();
        assert!(matches!(b.key, Key::Shard(_)));
        drop((a, b));
        assert!(l.mcp.lock().unwrap().queues.is_empty());
    }
}
