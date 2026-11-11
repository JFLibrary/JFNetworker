// traffic -- traffic management of the lab (SA6): the policies the balancer (lb.rs, kinds "lb" and
// "proxy") applies per pool or route and the load generator's retries (httpsvc.rs), each a small piece
// of deterministic state on virtual time; what it guards against is in docs/REFERENCE.md
//
//   RetryCfg::of(&Value) / delay(attempt, rng) / repeats(method, Outcome)   retries with exponential
//                     backoff (base_ms doubled per attempt, at most max_backoff_ms) and jitter ("full":
//                     anything from 0 to that, "equal": half of it plus up to the other half, "none"), for
//                     the failures "on" names, only for what is safe to repeat (idempotent; a request that
//                     never reached a backend always)
//                     {"max": 2, "backoff_ms": 25, "max_backoff_ms": 1000, "jitter": "full", "budget": 0.2,
//                      "min_per_s": 0, "on": ["connect", "timeout", "reset", "5xx"]}
//   Budget            the first attempts and the retries of the last BUDGET_SECS seconds: a retry goes only
//                     while retries + 1 <= budget * requests + min_per_s * BUDGET_SECS
//   BreakerCfg::of / Breaker   a circuit breaker per backend: closed; open after `failures` failures in a
//                     row, for open_ms; then half-open, `probes` requests go through; that many successes
//                     close it, one failure opens it again.  {"failures": 5, "open_ms": 5000, "probes": 1}
//   OutlierCfg::of    passive ejection: `consecutive` failures in a row eject a backend for base_ms times the
//                     number of its ejections, at most max_ms; never more than max_ejected of a pool's
//                     backends at once.  {"consecutive": 5, "base_ms": 30000, "max_ms": 300000,
//                     "max_ejected": 0.5}
//   LimitCfg::of / Limiter   a token bucket per key ("all", "client" (its address), "header:NAME" or
//                     "route"): `rate` tokens per second (decimals allowed), `burst` at most; a request
//                     without a whole token gets `status` (429) with Retry-After (whole seconds until the
//                     next token).  Tokens are counted in millionths of millionths, so the arithmetic is
//                     exact: from a full bucket, burst + rate * T requests pass in a span of T.
//                     {"rate": 100, "burst": 20, "key": "client", "status": 429}
//   StickyCfg::of     sticky sessions: {"type": "cookie", "name": "JFNLB"} (the balancer sets the cookie, its
//                     value is the backend's name) or {"type": "client", "ttl_ms": 0} (the client's address)
//   TlsCost::of       what TLS costs a proxy's CPU, at a reference core (assumptions of the lab):
//                     handshake_us 400 for a full handshake, resume_us 50 for one taken as resumed,
//                     tls_kb_us 1 per KiB through it; "session_reuse": a new connection of an address that
//                     did a full handshake within session_ttl_ms counts as resumed (modelled: the bytes stay
//                     a full TLS 1.3 handshake, netpkt has no resumption)
//   Outcome / outcome_of(&Result<Resp, Fail>)   how an attempt ended
//   idempotent(method) safe to repeat (RFC 9110 9.2.2)
//   TrafficNote       the decisions as notes (Note::Traffic) with their sentences
//   Engine::traffic_problems   ProblemKind::TrafficSetup: what cannot work (problems.rs)
//
// Nothing here draws a random number unless a jitter asks for one (the caller's generator).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use agent_core::jmap::Value;
use base::rng::Pcg32;
use jfn_model::msg;
use jfn_model::msg::Msg;
use netpkt::decode::{P_HTTP, P_IPV4, P_TCP};

use crate::http::{Fail, Resp};
use crate::kernel::Engine;
use crate::httpsvc::{decimal, rate_ppm};
use crate::kernel::{Ns, MS, SEC, US};

/// seconds a retry budget looks back
pub const BUDGET_SECS: u64 = 10;
/// buckets a limiter keeps at most (full ones go first, then the first by key)
pub const MAX_BUCKETS: usize = 10_000;
/// one token in the limiter's unit
const TOKEN: u128 = 1_000_000_000_000;

/// safe to repeat: the methods RFC 9110 calls idempotent
pub fn idempotent(method: &str) -> bool {
    matches!(method, "GET" | "HEAD" | "OPTIONS" | "TRACE" | "PUT" | "DELETE")
}

/// how an attempt ended
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// an answer below 500
    Ok,
    /// an answer of 500 or more
    Status(u16),
    /// it never reached the backend: refused, unreachable, no route, connect timeout
    Connect,
    /// no answer in time
    Timeout,
    /// the connection closed or broke without a whole answer
    Reset,
}

impl Outcome {
    pub fn failed(self) -> bool {
        self != Outcome::Ok
    }
}

/// the outcome of an agent's request
pub fn outcome_of(r: &Result<Resp, Fail>) -> Outcome {
    match r {
        Ok(resp) if resp.head.status >= 500 => Outcome::Status(resp.head.status),
        Ok(_) => Outcome::Ok,
        Err(Fail::Connect(_) | Fail::NoRoute(_) | Fail::ConnectTimeout | Fail::Resolve(_)) => Outcome::Connect,
        Err(Fail::Timeout { .. }) => Outcome::Timeout,
        Err(_) => Outcome::Reset,
    }
}

fn ms_of(v: Option<&Value>, d: Ns) -> Ns {
    v.and_then(|x| decimal(x, MS)).unwrap_or(d)
}

fn u32_of(v: Option<&Value>, d: u32, max: u32) -> u32 {
    v.and_then(|x| x.as_u64()).map_or(d, |n| n.min(max as u64) as u32)
}

/// an object or a refusal naming its key
fn object<'a>(v: &'a Value, key: &str) -> Result<&'a Value, Msg> {
    if v.as_object().is_some() { Ok(v) } else { Err(msg!("\u{201e}{}\u{201c} braucht ein Objekt.", key)) }
}

// ---------------------------------------------------------------- retries

/// how the backoff is spread
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Jitter {
    None,
    Full,
    Equal,
}

impl Jitter {
    pub fn name(self) -> &'static str {
        match self {
            Jitter::None => "none",
            Jitter::Full => "full",
            Jitter::Equal => "equal",
        }
    }
}

/// a retry policy
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetryCfg {
    /// attempts after the first
    pub max: u32,
    pub base: Ns,
    pub cap: Ns,
    pub jitter: Jitter,
    /// retries per mille of the requests at most (None: no budget)
    pub budget_pm: Option<u32>,
    pub min_per_s: u32,
    pub on_connect: bool,
    pub on_timeout: bool,
    pub on_reset: bool,
    pub on_5xx: bool,
}

impl Default for RetryCfg {
    fn default() -> RetryCfg {
        RetryCfg { max: 2, base: 25 * MS, cap: SEC, jitter: Jitter::Full, budget_pm: None, min_per_s: 0, on_connect: true, on_timeout: true, on_reset: true, on_5xx: true }
    }
}

impl RetryCfg {
    pub fn of(v: &Value) -> Result<RetryCfg, Msg> {
        let v = object(v, "retry")?;
        let mut r = RetryCfg { max: u32_of(v.get("max"), 2, 10), ..RetryCfg::default() };
        r.base = ms_of(v.get("backoff_ms"), r.base);
        r.cap = ms_of(v.get("max_backoff_ms"), r.cap).max(r.base);
        if let Some(j) = v.get("jitter").and_then(|x| x.as_str()) {
            r.jitter = match j {
                "none" | "off" => Jitter::None,
                "full" => Jitter::Full,
                "equal" => Jitter::Equal,
                x => return Err(msg!("\u{201e}retry.jitter\u{201c} kennt {} nicht (none, full, equal).", x)),
            };
        }
        if let Some(b) = v.get("budget") {
            let ppm = rate_ppm(b).ok_or_else(|| msg!("\u{201e}retry.budget\u{201c} ist ein Anteil zwischen 0 und 1."))?;
            r.budget_pm = Some(ppm / 1000);
        }
        r.min_per_s = u32_of(v.get("min_per_s"), 0, 1_000_000);
        if let Some(on) = v.get("on") {
            let words: Vec<&str> = match on {
                Value::String(s) => s.split(',').map(|w| w.trim()).filter(|w| !w.is_empty()).collect(),
                Value::Array(a) => a.iter().filter_map(|w| w.as_str()).collect(),
                _ => return Err(msg!("\u{201e}retry.on\u{201c} ist eine Liste aus connect, timeout, reset, 5xx.")),
            };
            r.on_connect = false;
            r.on_timeout = false;
            r.on_reset = false;
            r.on_5xx = false;
            for w in words {
                match w {
                    "connect" => r.on_connect = true,
                    "timeout" => r.on_timeout = true,
                    "reset" => r.on_reset = true,
                    "5xx" => r.on_5xx = true,
                    x => return Err(msg!("\u{201e}retry.on\u{201c} kennt {} nicht (connect, timeout, reset, 5xx).", x)),
                }
            }
        }
        Ok(r)
    }
    /// the policy as its config writes it
    pub fn value(&self) -> Value {
        let mut on = Vec::new();
        for (b, w) in [(self.on_connect, "connect"), (self.on_timeout, "timeout"), (self.on_reset, "reset"), (self.on_5xx, "5xx")] {
            if b {
                on.push(Value::string(w));
            }
        }
        let mut o = Value::object([
            ("max", Value::u64(self.max as u64)),
            ("backoff_ms", crate::svcform::ms_of(self.base)),
            ("max_backoff_ms", crate::svcform::ms_of(self.cap)),
            ("jitter", Value::string(self.jitter.name())),
            ("min_per_s", Value::u64(self.min_per_s as u64)),
            ("on", Value::Array(on)),
        ]);
        if let Some(b) = self.budget_pm {
            o.insert("budget", crate::svcform::rate_of(b * 1000));
        }
        o
    }
    /// this attempt's outcome may be repeated for this method
    pub fn repeats(&self, method: &str, o: Outcome) -> bool {
        match o {
            Outcome::Ok => false,
            Outcome::Connect => self.on_connect,
            Outcome::Timeout => self.on_timeout && idempotent(method),
            Outcome::Reset => self.on_reset && idempotent(method),
            Outcome::Status(_) => self.on_5xx && idempotent(method),
        }
    }
    /// the wait before retry number `attempt` (1: the first)
    pub fn delay(&self, attempt: u32, rng: &mut Pcg32) -> Ns {
        let shift = attempt.saturating_sub(1).min(40);
        let d = self.base.saturating_mul(1u64 << shift).min(self.cap);
        if d == 0 {
            return 0;
        }
        match self.jitter {
            Jitter::None => d,
            Jitter::Full => rng.range(0, d as i64) as Ns,
            Jitter::Equal => d / 2 + rng.range(0, (d - d / 2) as i64) as Ns,
        }
    }
    /// it draws random numbers
    pub fn random(&self) -> bool {
        self.jitter != Jitter::None && self.base > 0
    }
}

/// first attempts and retries of the last BUDGET_SECS seconds
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Budget {
    /// (second, requests, retries)
    slots: [(u64, u64, u64); BUDGET_SECS as usize],
}

impl Budget {
    fn slot(&mut self, now: Ns) -> &mut (u64, u64, u64) {
        let sec = now / SEC;
        let s = &mut self.slots[(sec % BUDGET_SECS) as usize];
        if s.0 != sec {
            *s = (sec, 0, 0);
        }
        s
    }
    /// the requests and retries of the window ending now
    pub fn sums(&self, now: Ns) -> (u64, u64) {
        let sec = now / SEC;
        self.slots.iter().filter(|s| s.0 + BUDGET_SECS > sec && s.0 <= sec).fold((0, 0), |a, s| (a.0 + s.1, a.1 + s.2))
    }
    /// a first attempt
    pub fn request(&mut self, now: Ns) {
        self.slot(now).1 += 1;
    }
    /// whether a retry may go now, and if so it counts
    pub fn take(&mut self, now: Ns, cfg: &RetryCfg) -> bool {
        if let Some(pm) = cfg.budget_pm {
            let (req, ret) = self.sums(now);
            if (ret + 1) * 1000 > req * pm as u64 + cfg.min_per_s as u64 * BUDGET_SECS * 1000 {
                return false;
            }
        }
        self.slot(now).2 += 1;
        true
    }
}

// ---------------------------------------------------------------- circuit breaker

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BreakerCfg {
    pub failures: u32,
    pub open: Ns,
    pub probes: u32,
}

impl BreakerCfg {
    pub fn of(v: &Value) -> Result<BreakerCfg, Msg> {
        let v = object(v, "breaker")?;
        Ok(BreakerCfg { failures: u32_of(v.get("failures"), 5, 1_000_000).max(1), open: ms_of(v.get("open_ms"), 5 * SEC).max(MS), probes: u32_of(v.get("probes"), 1, 1000).max(1) })
    }
    pub fn value(&self) -> Value {
        Value::object([("failures", Value::u64(self.failures as u64)), ("open_ms", crate::svcform::ms_of(self.open)), ("probes", Value::u64(self.probes as u64))])
    }
}

/// the state of a breaker
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BState {
    Closed,
    Open,
    HalfOpen,
}

impl BState {
    /// 0 closed, 1 open, 2 half-open (a metric)
    pub fn code(self) -> i64 {
        match self {
            BState::Closed => 0,
            BState::Open => 1,
            BState::HalfOpen => 2,
        }
    }
    pub fn word(self) -> Msg {
        match self {
            BState::Closed => msg!("geschlossen"),
            BState::Open => msg!("offen"),
            BState::HalfOpen => msg!("halb offen"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Breaker {
    pub state: BState,
    /// failures in a row while closed
    pub fails: u32,
    /// open until then
    pub until: Ns,
    /// probes in flight and their successes (half-open)
    pub probing: u32,
    pub ok: u32,
    /// times it opened
    pub opened: u32,
    /// when its state last changed
    pub since: Ns,
}

impl Default for Breaker {
    fn default() -> Breaker {
        Breaker { state: BState::Closed, fails: 0, until: 0, probing: 0, ok: 0, opened: 0, since: 0 }
    }
}

impl Breaker {
    /// an open breaker whose time is over goes half-open; true when it did
    pub fn refresh(&mut self, now: Ns) -> bool {
        if self.state == BState::Open && now >= self.until {
            self.state = BState::HalfOpen;
            self.probing = 0;
            self.ok = 0;
            self.since = now;
            return true;
        }
        false
    }
    /// it lets a request through now
    pub fn admits(&self, cfg: &BreakerCfg) -> bool {
        match self.state {
            BState::Closed => true,
            BState::Open => false,
            BState::HalfOpen => self.probing < cfg.probes,
        }
    }
    /// a request went through: whether it is a probe
    pub fn sent(&mut self) -> bool {
        if self.state == BState::HalfOpen {
            self.probing += 1;
            return true;
        }
        false
    }
    /// how a request ended (`probe`: it went while half-open); the new state when it changed
    pub fn result(&mut self, now: Ns, cfg: &BreakerCfg, ok: bool, probe: bool) -> Option<BState> {
        match self.state {
            BState::Closed => {
                if ok {
                    self.fails = 0;
                    return None;
                }
                self.fails += 1;
                if self.fails >= cfg.failures {
                    self.open(now, cfg);
                    return Some(BState::Open);
                }
                None
            }
            BState::HalfOpen if probe => {
                self.probing = self.probing.saturating_sub(1);
                if !ok {
                    self.open(now, cfg);
                    return Some(BState::Open);
                }
                self.ok += 1;
                if self.ok >= cfg.probes {
                    self.state = BState::Closed;
                    self.fails = 0;
                    self.since = now;
                    return Some(BState::Closed);
                }
                None
            }
            _ => None,
        }
    }
    fn open(&mut self, now: Ns, cfg: &BreakerCfg) {
        self.state = BState::Open;
        self.until = now + cfg.open;
        self.opened += 1;
        self.fails = 0;
        self.probing = 0;
        self.ok = 0;
        self.since = now;
    }
}

// ---------------------------------------------------------------- outlier ejection

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutlierCfg {
    pub consecutive: u32,
    pub base: Ns,
    pub max: Ns,
    /// ejected backends of a pool at most, per mille of its backends
    pub max_ejected_pm: u32,
}

impl OutlierCfg {
    pub fn of(v: &Value) -> Result<OutlierCfg, Msg> {
        let v = object(v, "outlier")?;
        let base = ms_of(v.get("base_ms"), 30 * SEC).max(MS);
        let max_pm = match v.get("max_ejected") {
            Some(x) => rate_ppm(x).ok_or_else(|| msg!("\u{201e}outlier.max_ejected\u{201c} ist ein Anteil zwischen 0 und 1."))? / 1000,
            None => 500,
        };
        Ok(OutlierCfg { consecutive: u32_of(v.get("consecutive"), 5, 1_000_000).max(1), base, max: ms_of(v.get("max_ms"), 300 * SEC).max(base), max_ejected_pm: max_pm })
    }
    pub fn value(&self) -> Value {
        Value::object([
            ("consecutive", Value::u64(self.consecutive as u64)),
            ("base_ms", crate::svcform::ms_of(self.base)),
            ("max_ms", crate::svcform::ms_of(self.max)),
            ("max_ejected", crate::svcform::rate_of(self.max_ejected_pm * 1000)),
        ])
    }
    /// how long the ejection number `n` (1: the first) lasts
    pub fn time(&self, n: u32) -> Ns {
        self.base.saturating_mul(n.max(1) as u64).min(self.max)
    }
    /// backends that may be ejected at once out of `of`
    pub fn cap(&self, of: usize) -> usize {
        of * self.max_ejected_pm as usize / 1000
    }
}

// ---------------------------------------------------------------- rate limits

/// what a rate limit counts by
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LimitKey {
    All,
    Client,
    Header(String),
    Route,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LimitCfg {
    /// tokens per second in thousandths
    pub rate_m: u64,
    pub burst: u32,
    pub key: LimitKey,
    pub status: u16,
}

impl LimitCfg {
    pub fn of(v: &Value) -> Result<LimitCfg, Msg> {
        let v = object(v, "limit")?;
        let rate_m = match v.get("rate") {
            Some(r) => decimal(r, 1000).ok_or_else(|| msg!("\u{201e}limit.rate\u{201c} ist eine Zahl je Sekunde."))?,
            None => return Err(msg!("\u{201e}limit\u{201c} braucht \u{201e}rate\u{201c} (Anfragen je Sekunde).")),
        };
        let burst = u32_of(v.get("burst"), ((rate_m + 999) / 1000).max(1) as u32, 1_000_000).max(1);
        let key = match v.get("key").and_then(|x| x.as_str()).unwrap_or("client") {
            "all" => LimitKey::All,
            "client" => LimitKey::Client,
            "route" => LimitKey::Route,
            x if x.starts_with("header:") && x.len() > 7 => LimitKey::Header(String::from(x[7..].trim())),
            x => return Err(msg!("\u{201e}limit.key\u{201c} kennt {} nicht (all, client, route, header:NAME).", x)),
        };
        let status = match v.get("status").and_then(|x| x.as_u64()).unwrap_or(429) {
            s @ 400..=599 => s as u16,
            s => return Err(msg!("\u{201e}limit.status\u{201c}: {} ist kein Fehlerstatus (400 bis 599).", s)),
        };
        Ok(LimitCfg { rate_m, burst, key, status })
    }
    pub fn key_name(&self) -> String {
        match &self.key {
            LimitKey::All => String::from("all"),
            LimitKey::Client => String::from("client"),
            LimitKey::Route => String::from("route"),
            LimitKey::Header(h) => alloc::format!("header:{}", h),
        }
    }
    pub fn value(&self) -> Value {
        Value::object([("rate", crate::svcform::thousandths(self.rate_m)), ("burst", Value::u64(self.burst as u64)), ("key", Value::String(self.key_name())), ("status", Value::u64(self.status as u64))])
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Bucket {
    /// tokens in millionths of millionths
    p: u128,
    at: Ns,
}

/// the buckets of one rate limit
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Limiter {
    buckets: BTreeMap<String, Bucket>,
    pub passed: u64,
    pub limited: u64,
}

impl Limiter {
    fn refill(b: &mut Bucket, now: Ns, cfg: &LimitCfg) {
        let cap = cfg.burst as u128 * TOKEN;
        if now > b.at {
            b.p = (b.p + (now - b.at) as u128 * cfg.rate_m as u128).min(cap);
        }
        b.at = now;
    }
    /// a token for this key, or the seconds until there is one
    pub fn take(&mut self, now: Ns, cfg: &LimitCfg, key: &str) -> Result<(), u64> {
        if !self.buckets.contains_key(key) {
            if self.buckets.len() >= MAX_BUCKETS {
                self.prune(now, cfg);
            }
            self.buckets.insert(String::from(key), Bucket { p: cfg.burst as u128 * TOKEN, at: now });
        }
        let Some(b) = self.buckets.get_mut(key) else {
            return Err(1);
        };
        Limiter::refill(b, now, cfg);
        if b.p >= TOKEN {
            b.p -= TOKEN;
            self.passed += 1;
            return Ok(());
        }
        self.limited += 1;
        if cfg.rate_m == 0 {
            return Err(60);
        }
        // picotokens / (picotokens per ns) = ns, in whole seconds rounded up
        let ns = (TOKEN - b.p).div_ceil(cfg.rate_m as u128);
        Err((ns.div_ceil(SEC as u128) as u64).max(1))
    }
    /// room for a new bucket: the full ones go, else the first
    fn prune(&mut self, now: Ns, cfg: &LimitCfg) {
        let cap = cfg.burst as u128 * TOKEN;
        let full: Vec<String> = self
            .buckets
            .iter()
            .filter(|(_, b)| {
                let mut c = (*b).clone();
                Limiter::refill(&mut c, now, cfg);
                c.p >= cap
            })
            .map(|(k, _)| k.clone())
            .collect();
        for k in full {
            self.buckets.remove(&k);
        }
        if self.buckets.len() >= MAX_BUCKETS
            && let Some(k) = self.buckets.keys().next().cloned()
        {
            self.buckets.remove(&k);
        }
    }
    /// buckets kept now
    pub fn len(&self) -> usize {
        self.buckets.len()
    }
    pub fn is_empty(&self) -> bool {
        self.buckets.is_empty()
    }
    /// the bytes it holds (a keyframe's footprint)
    pub fn footprint(&self) -> usize {
        self.buckets.keys().map(|k| k.len() + 48).sum()
    }
}

// ---------------------------------------------------------------- sticky sessions

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StickyCfg {
    /// the balancer sets this cookie to the backend's name
    Cookie { name: String },
    /// the client's address, forgotten after ttl without a request (0: never)
    Client { ttl: Ns },
}

impl StickyCfg {
    pub fn of(v: &Value) -> Result<StickyCfg, Msg> {
        let v = object(v, "sticky")?;
        match v.get("type").and_then(|x| x.as_str()).unwrap_or("cookie") {
            "cookie" => {
                let name = v.get("name").and_then(|x| x.as_str()).unwrap_or("JFNLB").trim();
                if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') {
                    return Err(msg!("\u{201e}sticky.name\u{201c}: {} taugt nicht als Name eines Cookies.", String::from(name)));
                }
                Ok(StickyCfg::Cookie { name: String::from(name) })
            }
            "client" => Ok(StickyCfg::Client { ttl: ms_of(v.get("ttl_ms"), 0) }),
            x => Err(msg!("\u{201e}sticky.type\u{201c} kennt {} nicht (cookie, client).", x)),
        }
    }
    pub fn value(&self) -> Value {
        match self {
            StickyCfg::Cookie { name } => Value::object([("type", Value::string("cookie")), ("name", Value::String(name.clone()))]),
            StickyCfg::Client { ttl } => Value::object([("type", Value::string("client")), ("ttl_ms", crate::svcform::ms_of(*ttl))]),
        }
    }
}

/// the value of a cookie in a Cookie header
pub fn cookie_value<'a>(header: &'a str, name: &str) -> Option<&'a str> {
    header.split(';').find_map(|p| {
        let (k, v) = p.trim().split_once('=')?;
        if k.trim() == name { Some(v.trim()) } else { None }
    })
}

// ---------------------------------------------------------------- TLS

/// what TLS costs a proxy's CPU (at a reference core)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TlsCost {
    pub handshake: Ns,
    pub resume: Ns,
    pub per_kb: Ns,
    pub reuse: bool,
    pub session_ttl: Ns,
}

impl Default for TlsCost {
    fn default() -> TlsCost {
        TlsCost { handshake: 400 * US, resume: 50 * US, per_kb: US, reuse: false, session_ttl: 300 * SEC }
    }
}

impl TlsCost {
    pub fn of(c: &Value) -> TlsCost {
        let d = TlsCost::default();
        let us = |k: &str, d: Ns| c.get(k).and_then(|v| decimal(v, US)).unwrap_or(d);
        TlsCost {
            handshake: us("handshake_us", d.handshake),
            resume: us("resume_us", d.resume),
            per_kb: us("tls_kb_us", d.per_kb),
            reuse: c.get("session_reuse").and_then(|v| v.as_bool()).unwrap_or(false),
            session_ttl: ms_of(c.get("session_ttl_ms"), d.session_ttl),
        }
    }
    /// the CPU of `bytes` through TLS
    pub fn bytes(&self, bytes: u64) -> Ns {
        (self.per_kb as u128 * bytes as u128 / 1024) as Ns
    }
}

// ---------------------------------------------------------------- problems

impl Engine {
    /// traffic management that cannot do what it says (problems.rs, ProblemKind::TrafficSetup)
    pub(crate) fn traffic_problems(&self, out: &mut Vec<crate::problems::Problem>) {
        use crate::problems::{Problem, ProblemKind, Severity};
        let mut warn = |dev: u32, m: Msg| out.push(Problem { kind: ProblemKind::TrafficSetup, severity: Severity::Warning, dev, port: None, link: None, others: Vec::new(), text: m.text(), msg: m });
        for dev in self.devices() {
            let Some(ni) = self.node_index(dev) else { continue };
            let Some(os) = self.os(ni) else { continue };
            for s in os.svcs.iter().filter(|s| s.enabled && !s.console) {
                let Some(svc) = s.svc.as_ref() else { continue };
                if let Some(lb) = svc.as_any().downcast_ref::<crate::lb::Lb>() {
                    for p in &lb.pools {
                        if p.slow_start > 0 && matches!(p.algo, crate::lb::Algo::LeastConn | crate::lb::Algo::Hash) {
                            warn(dev, msg!("Dienst {}, Pool {}: der langsame Start wirkt nur bei round-robin und weighted, nicht bei {}.", s.name.clone(), p.name.clone(), p.algo.name()));
                        }
                        let n = lb.backends.iter().filter(|b| b.pool_name_is(&lb.pools, &p.name)).count();
                        if let Some(o) = &p.outlier
                            && n > 0
                            && o.cap(n) == 0
                        {
                            warn(dev, msg!("Dienst {}, Pool {}: bei {} Backends darf mit max_ejected {} ‰ keines ausgeschlossen werden.", s.name.clone(), p.name.clone(), n, o.max_ejected_pm));
                        }
                    }
                    let retries = lb.retry.iter().chain(lb.routes.iter().filter_map(|r| r.retry.as_ref()));
                    if retries.into_iter().any(|r| r.max > 0 && r.budget_pm == Some(0) && r.min_per_s == 0) {
                        warn(dev, msg!("Dienst {}: ein Retry-Budget von 0 lässt keine Wiederholung zu.", s.name.clone()));
                    }
                }
                if let Some(p) = svc.as_any().downcast_ref::<crate::dbpool::DbPool>() {
                    let (addr, port) = p.db;
                    let db = self.devices().find_map(|d| {
                        let n = self.node_index(d)?;
                        if !self.is_mine_ip(n, addr) {
                            return None;
                        }
                        self.os(n)?.svcs.iter().filter(|x| x.enabled).find_map(|x| x.svc.as_ref()?.as_any().downcast_ref::<crate::db::Db>().filter(|d| d.cfg.port == port))
                    });
                    match db {
                        None => warn(dev, msg!("Dienst {}: auf {}:{} läuft keine Datenbank.", s.name.clone(), addr, port)),
                        Some(d) if d.cfg.max_conns > 0 && d.cfg.max_conns < p.size => {
                            warn(dev, msg!("Dienst {}: der Pooler öffnet bis zu {} Verbindungen, die Datenbank auf {} nimmt nur {}.", s.name.clone(), p.size, addr, d.cfg.max_conns))
                        }
                        Some(_) => {}
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------- notes

/// a decision of traffic management (Note::Traffic)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TrafficNote {
    /// a route of the proxy (or the balancer's paths) took the request to a pool
    Routed { route: String, pool: String },
    /// every usable backend of the pool is at max_conns: the request waits
    Queued { pool: String, waiting: u32 },
    /// the queue is full (or there is none): 503 at once
    Shed { pool: String },
    /// it waited longer than queue_timeout_ms: 503
    QueueTimeout { pool: String },
    /// no token for this key: answered with this status
    Limited { key: String, status: u16, retry_after: u64 },
    /// a backend's breaker changed its state
    Breaker { backend: String, state: BState },
    /// every breaker of the pool is open: 503 without trying
    FailFast { pool: String },
    /// a backend failed too often in a row and is ejected for a while
    Ejected { backend: String, ms: u64 },
    /// an ejected backend takes requests again
    Returned { backend: String },
    /// the cap on ejected backends kept this one in
    NotEjected { backend: String },
    /// an attempt failed: the next one after a wait
    Retry { backend: String, attempt: u32, ms: u64 },
    /// the retry budget is used up: no retry
    BudgetDenied,
    /// a sticky session's backend is gone: it moves
    StickyMoved { from: String, to: String },
    /// answered from the cache (stale: while it revalidates)
    CacheServed { key: String, stale: bool },
    /// a backend's answer is kept for this long
    CacheStored { key: String, ms: u64 },
    /// a TLS handshake took CPU (resumed: modelled as a resumption)
    Handshake { resumed: bool, us: u64 },
    /// the database pooler: a statement waits for a connection
    PoolWait { waiting: u32 },
    /// the database pooler gave a connection to a transaction / took it back
    PoolTxn { txn: u64, held: bool },
}

impl TrafficNote {
    pub fn message(&self) -> Msg {
        match self {
            TrafficNote::Routed { route, pool } => msg!("Proxy: Route {} führt zum Pool {}.", route, pool),
            TrafficNote::Queued { pool, waiting } => msg!("Pool {}: alle Backends am Limit, die Anfrage wartet ({} in der Warteschlange).", pool, waiting),
            TrafficNote::Shed { pool } => msg!("Pool {}: Warteschlange voll, 503 sofort (Load Shedding).", pool),
            TrafficNote::QueueTimeout { pool } => msg!("Pool {}: zu lange gewartet, 503.", pool),
            TrafficNote::Limited { key, status, retry_after } => msg!("Rate-Limit für {}: kein Token frei, {} mit Retry-After {} s.", key, status, retry_after),
            TrafficNote::Breaker { backend, state } => msg!("Circuit Breaker von {}: jetzt {}.", backend, state.word()),
            TrafficNote::FailFast { pool } => msg!("Pool {}: alle Circuit Breaker offen, 503 ohne Versuch.", pool),
            TrafficNote::Ejected { backend, ms } => msg!("{} fällt zu oft aus und ist für {} ms ausgeschlossen.", backend, ms),
            TrafficNote::Returned { backend } => msg!("{} nimmt wieder Anfragen an.", backend),
            TrafficNote::NotEjected { backend } => msg!("{} fällt aus, bleibt aber drin: mehr Backends dürfen nicht ausgeschlossen sein.", backend),
            TrafficNote::Retry { backend, attempt, ms } => msg!("Versuch bei {} gescheitert, Wiederholung {} nach {} ms.", backend, attempt, ms),
            TrafficNote::BudgetDenied => msg!("Retry-Budget erschöpft: keine Wiederholung."),
            TrafficNote::StickyMoved { from, to } => msg!("Sticky Session: {} ist weg, die Sitzung zieht zu {}.", from, to),
            TrafficNote::CacheServed { key, stale: false } => msg!("Proxy: {} aus dem Cache beantwortet.", key),
            TrafficNote::CacheServed { key, stale: true } => msg!("Proxy: {} abgelaufen aus dem Cache beantwortet, wird im Hintergrund erneuert.", key),
            TrafficNote::CacheStored { key, ms } => msg!("Proxy: Antwort für {} für {} ms im Cache.", key, ms),
            TrafficNote::Handshake { resumed: false, us } => msg!("TLS-Handshake kostet {} µs CPU.", us),
            TrafficNote::Handshake { resumed: true, us } => msg!("TLS-Sitzung wieder aufgenommen: {} µs CPU.", us),
            TrafficNote::PoolWait { waiting } => msg!("DB-Pooler: keine Verbindung frei, die Anweisung wartet ({} in der Warteschlange).", waiting),
            TrafficNote::PoolTxn { txn, held: true } => msg!("DB-Pooler: Transaktion {} hält eine Verbindung.", txn),
            TrafficNote::PoolTxn { txn, held: false } => msg!("DB-Pooler: Transaktion {} gibt ihre Verbindung zurück.", txn),
        }
    }
    /// something was refused
    pub fn is_drop(&self) -> bool {
        matches!(self, TrafficNote::Shed { .. } | TrafficNote::QueueTimeout { .. } | TrafficNote::Limited { .. } | TrafficNote::FailFast { .. } | TrafficNote::BudgetDenied)
    }
    pub fn mask(&self) -> u32 {
        P_IPV4 | P_TCP | P_HTTP
    }
}
