// lb -- the load balancer of the lab: HTTP (layer 7: every request goes to a backend over
// kept-alive connections and its answer back) or TCP (layer 4: every connection is spliced to
// a backend byte for byte); round robin, least connections, weighted round robin and
// consistent hashing with virtual nodes; health checks with interval, timeout and thresholds;
// backends that drain before they go; numbers per backend.  Since SA6 also the reverse proxy
// (kind "proxy", the same type) and the traffic management both share (traffic.rs)
//
//   kind  config (JSON; every member but "backends" may be left out)
//   lb    {"mode": "http", "port": 80, "algorithm": "round-robin", "hash": "path", "vnodes": 64,
//          "backends": [{"name": "web1", "addr": "10.0.2.11", "port": 80, "weight": 1}],
//          "health": {"type": "http", "path": "/", "interval_ms": 2000, "timeout_ms": 1000,
//          "rise": 2, "fall": 3}, "retries": 1, "timeout_ms": 10000, "connect_timeout_ms": 1000,
//          "drain_ms": 30000, "max_conns": 0, "keepalive": true, "tag": true, "cpu_us": 50, "mem_mb": 16,
//          "conn_kb": 64}
//          SA6, HTTP only, each may be left out (and then nothing of it happens):
//          "queue": 100, "queue_timeout_ms": 1000, "sticky": {...}, "slow_start_ms": 30000,
//          "outlier": {...}, "breaker": {...}, "retry": {...}, "limit": {...} (traffic.rs has the members),
//          "pools": [{"name": "api", "backends": [...], "algorithm": .., "hash": .., "max_conns": ..,
//          "queue": .., "queue_timeout_ms": .., "sticky": .., "slow_start_ms": .., "outlier": ..,
//          "breaker": ..}], "routes": [{"host": "", "path": "/api/", "pool": "api", "retry": ..,
//          "limit": ..}]
//   proxy {"port": 80, "tls": {...} (tlssim.rs; the port is then 443), "handshake_us": 400, "resume_us": 50,
//          "tls_kb_us": 1, "session_reuse": false, "session_ttl_ms": 300000, "max_requests": 0,
//          "keepalive_ms": 75000, "cache": {"entries": 1000, "max_kb": 65536, "ttl_ms": 30000,
//          "stale_ms": 0, "hit_us": 20}, the members of an HTTP lb, routes with "strip": "/api",
//          "add": "/v1", "set_headers": {"X-Env": "lab"}, "remove_headers": ["Cookie"], "cache": true}
//
// Algorithms ("algorithm"): "round-robin" takes the backends in turn; "least-conn" the one
// with the fewest requests (HTTP) or connections (TCP) in flight, in turn among equals;
// "weighted" is smooth weighted round robin (a backend of weight 3 gets three of every four
// picks against one of weight 1, spread out); "hash" puts the backends on a ring with "vnodes"
// tokens each and takes the first usable one from the hash of the key: "path" (HTTP), "client"
// (the client's address, the default for TCP) or "header:NAME".  A backend is usable while its
// health checks hold it up and it is neither draining nor disabled (TCP: and below max_conns).
//
// Health checks ("health"): every interval_ms, starting at once; "tcp" opens a connection,
// "http" sends GET path (2xx and 3xx count as success), "none" switches them off; a check
// fails when it gets no success within timeout_ms.  A backend goes down after "fall" failed
// checks in a row and up again after "rise" successful ones; it starts up.
//
// HTTP: a request that cannot reach its backend (refused, unreachable, connect timeout) goes
// to another one, at most "retries" times, and so does a GET or HEAD whose backend did not
// answer in time or closed the connection on it (they change nothing); then 502, 504 for a
// request that timed out at its backend, 503 when no backend is usable.  "tag" adds X-Served-By with the backend's name.
// TCP: a connection that cannot reach its backend tries the next one the same way, then
// closes.  Removing a backend (Action::LbBackend) drains it: it gets nothing new, and once
// what it has is done (or drain_ms passed) it is gone.
//
// Traffic management (SA6, HTTP).  Pools: the top-level backends are the pool "default", "pools" adds
// more, each with its own algorithm and policies; "routes" take a request to a pool by the longest
// matching path prefix (a route naming the Host first); no route: "default", which a proxy may leave
// empty (404).  "queue": the pool's backends take at most max_conns requests at once, the others wait
// in order, at most `queue` of them and queue_timeout_ms long; then 503 with "Retry-After: 1" (load
// shedding).  Without "queue" max_conns stays what it was: connections per backend, requests wait
// for them without a bound.  "sticky": a request with the cookie (or from the address) goes to its
// backend while that one is usable, else to the next by the algorithm, and the session moves
// (sticky_moved).  "slow_start_ms": a backend that comes up (checks, added, enabled, back from
// ejection) gets its weight rising from nothing to full over that time (round-robin and weighted;
// least-conn and hash ignore it).  "outlier", "breaker": per backend, see traffic.rs; a failure is
// an answer of 500 or more or no answer.  "retry" replaces "retries": the next attempt after a
// backoff, to another backend while there is one, within the budget; the answer of the last
// attempt goes back.  "limit": checked before anything else, 429 with Retry-After.
//
// Proxy: everything above, plus per route the Host (without port, any when empty) and the
// rewriting: "strip" takes a prefix off the path, "add" puts one in front, set_headers and
// remove_headers change what goes on; X-Forwarded-For, -Proto and -Host.  With "tls" it
// terminates TLS: on a device with hardware a full handshake costs handshake_us of CPU (resume_us
// when "session_reuse" counts it as resumed), every KiB through it tls_kb_us; without hardware
// none of it costs anything, as everywhere.  "max_requests": requests per client connection (the
// keep-alive reuse factor).  "cache": GET and HEAD of routes with "cache" (default on) are kept by
// method, route and target: answers 200, 203, 204, 301, 404, 410 for ttl_ms or what Cache-Control
// says (max-age; no-store, no-cache, private keep nothing), "stale_ms" or stale-while-revalidate
// answer from an expired entry while one request renews it in the background (X-Cache: HIT, STALE,
// MISS); LRU over entries and max_kb, a hit costs hit_us of CPU (a plain time without hardware).
//
// Metrics (Service::metrics_at): requests (HTTP) or connections (TCP), responses, active,
// errors, no_backend, retries, status_2xx..5xx, backends, backends_up, p50_us, p95_us, p99_us
// (HTTP: arrival to answer; TCP: connect time), and per backend in the order of the
// configuration b1_requests, b1_active, b1_up, b1_errors .. b8_*.  With anything of SA6 also
// limited, queued, waiting, shed, queue_timeouts, budget_denied, fail_fast, breakers_open,
// ejected, ejections, sticky_moved, wait_p99_us and b1_breaker, b1_ejected .. b8_*; a proxy also
// cache_hits, cache_misses, cache_stale, hit_pm, bytes_saved, tls_handshakes, tls_resumed,
// tls_cpu_us.  Engine::lb_view gives every backend with its state, checks and latencies (the lab),
// the pools and routes; lbctl and proxyctl show it on the console.  The time series (series.rs)
// take p50_us, p95_us, p99_us of each interval from the window.  The status line is a message for
// the app's catalogue (status_msg; status() is its German text).
//
// On a device with hardware (res.rs) every HTTP request costs "cpu_us" at a reference core before a
// backend is picked: a balancer can be the bottleneck.  The bytes of a TCP pipe cost no CPU.
//
// Service::reconfigure takes a whole new config while it runs, all but its mode, port, kind of
// health check, certificate and the pools and routes themselves (their names, prefixes, hosts):
// backends that are new start now, those that are gone drain first, another address of a backend
// needs another name; policies change from the next request on.

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use agent_core::jmap::Value;
use base::hashring::{hash64, Ring};
use base::lru::Lru;
use base::metrics::Window;
use base::rng::Pcg32;
use jfn_model::msg;
use jfn_model::msg::Msg;
use netpkt::http::{Headers, Version};
use netpkt::{IpAddr, Ipv4};

use crate::cache::{max_age, Cached};
use crate::http::{Agent, Done, Fail, Reply, Req, ReqIn, Serve, ServeCfg, Url, Wheel};
use crate::httpsvc::decimal;
use crate::kernel::{Engine, Ns, MS, SEC, US};
use crate::os::{Metric, OsApi, Service, SvcEvent};
use crate::series::{LatWindow, LAT};
use crate::tcp::{SockOpts, TcpEnd};
use crate::trace::Note;
use crate::traffic::{cookie_value, outcome_of, BState, Breaker, BreakerCfg, Budget, LimitCfg, LimitKey, Limiter, Outcome, OutlierCfg, RetryCfg, StickyCfg, TlsCost, TrafficNote};

/// timer keys: its own deadlines, the server side, the forwarding agent, the check agent
const K_OWN: u32 = 0;
const K_SERVE: u32 = 1;
const K_AGENT: u32 = 2;
const K_CHECK: u32 = 3;
/// bytes a TCP pipe buffers per direction before it stops reading
const PIPE_BUF: usize = 64 * 1024;
/// the ids of revalidations in the background start here (above every id of the server side)
const BG: u64 = 1 << 62;
/// client addresses a sticky pool or the TLS sessions remember at most
const CLIENTS_MAX: usize = 65_536;
/// the bytes of a head that TLS counts besides the body
const HEAD_BYTES: u64 = 200;

/// HTTP or TCP
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LbMode {
    Http,
    Tcp,
}

/// how a backend is picked
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Algo {
    RoundRobin,
    LeastConn,
    Weighted,
    Hash,
}

impl Algo {
    pub fn name(self) -> &'static str {
        match self {
            Algo::RoundRobin => "round-robin",
            Algo::LeastConn => "least-conn",
            Algo::Weighted => "weighted",
            Algo::Hash => "hash",
        }
    }
    pub fn parse(s: &str) -> Option<Algo> {
        Some(match s {
            "round-robin" | "roundrobin" | "rr" => Algo::RoundRobin,
            "least-conn" | "leastconn" | "lc" => Algo::LeastConn,
            "weighted" | "wrr" => Algo::Weighted,
            "hash" | "consistent-hash" => Algo::Hash,
            _ => return None,
        })
    }
}

/// what the hash of the "hash" algorithm is made of
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HashKey {
    Path,
    Client,
    Header(String),
}

/// the kind of health check
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckKind {
    None,
    Tcp,
    Http,
}

/// the health checks of the backends
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HealthCfg {
    pub kind: CheckKind,
    pub path: String,
    pub interval: Ns,
    pub timeout: Ns,
    pub rise: u32,
    pub fall: u32,
}

/// what the administrator wants of a backend
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admin {
    Active,
    /// gets nothing new; gone (`remove`) or disabled when what it has is done
    Draining { since: Ns, remove: bool },
    /// gets nothing (drained)
    Disabled,
    Removed,
}

/// a backend and its numbers
#[derive(Clone, Debug)]
pub struct Backend {
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
    pub weight: u32,
    pub admin: Admin,
    /// the health checks hold it up
    pub up: bool,
    /// successes and failures in a row
    pub ok_run: u32,
    pub fail_run: u32,
    /// the last check: when it ended and whether it succeeded
    pub last_check: Option<(Ns, bool)>,
    pub checks: u64,
    pub check_fails: u64,
    /// when its health or admin state last changed
    pub since: Ns,
    /// times the checks took it down
    pub downs: u32,
    /// requests (HTTP) or connections (TCP) in flight / sent to it
    pub active: u32,
    pub total: u64,
    /// requests or connections that failed at it
    pub errors: u64,
    /// answers by class 1xx .. 5xx
    pub by_class: [u64; 5],
    pub bytes_in: u64,
    pub bytes_out: u64,
    /// HTTP: sent to answer; TCP: connect time
    lat: Window,
    /// smooth weighted round robin
    cw: i64,
    /// a check running now
    checking: bool,
    /// the wheel entry of its next check
    check_t: Option<u64>,
    /// its pool (0: "default")
    pub pool: usize,
    /// SA6: its circuit breaker, failures in a row for ejection, ejected until then, times ejected,
    /// since when its weight rises (slow start)
    pub breaker: Breaker,
    pub out_fails: u32,
    pub ejected_until: Option<Ns>,
    pub ejections: u32,
    pub ramp_from: Option<Ns>,
}

impl Backend {
    fn new(name: &str, addr: IpAddr, port: u16, weight: u32, now: Ns) -> Backend {
        Backend {
            name: String::from(name),
            addr,
            port,
            weight: weight.max(1),
            admin: Admin::Active,
            up: true,
            ok_run: 0,
            fail_run: 0,
            last_check: None,
            checks: 0,
            check_fails: 0,
            since: now,
            downs: 0,
            active: 0,
            total: 0,
            errors: 0,
            by_class: [0; 5],
            bytes_in: 0,
            bytes_out: 0,
            lat: Window::new(300, SEC),
            cw: 0,
            checking: false,
            check_t: None,
            pool: 0,
            breaker: Breaker::default(),
            out_fails: 0,
            ejected_until: None,
            ejections: 0,
            ramp_from: None,
        }
    }
    fn gone(&self) -> bool {
        self.admin == Admin::Removed
    }
    /// it belongs to the pool of this name and is not gone (problems.rs)
    pub(crate) fn pool_name_is(&self, pools: &[Pool], name: &str) -> bool {
        !self.gone() && pools.get(self.pool).is_some_and(|p| p.name == name)
    }
}

/// a group of backends with its algorithm and policies (SA6; the top-level backends are pool 0,
/// "default")
#[derive(Clone, Debug)]
pub struct Pool {
    pub name: String,
    pub algo: Algo,
    pub hash_key: HashKey,
    turn: usize,
    ring: Ring<u32>,
    /// HTTP with a queue: requests in flight per backend at most (0: no limit)
    pub max_conns: u32,
    /// requests that may wait (None: no queue of the balancer's own)
    pub queue: Option<u32>,
    pub queue_timeout: Ns,
    pub sticky: Option<StickyCfg>,
    pub slow_start: Ns,
    pub outlier: Option<OutlierCfg>,
    pub breaker: Option<BreakerCfg>,
    /// the requests waiting, in order
    waiting: VecDeque<u64>,
    /// sticky by address: the backend and the last request
    clients: BTreeMap<IpAddr, (usize, Ns)>,
    pub queued: u64,
    pub shed: u64,
    pub queue_timeouts: u64,
    pub max_waiting: u32,
    pub sticky_moved: u64,
    pub fail_fast: u64,
    /// how long the queued requests waited
    wait: Window,
}

impl Pool {
    fn new(name: &str, mode: LbMode) -> Pool {
        Pool {
            name: String::from(name),
            algo: Algo::RoundRobin,
            hash_key: if mode == LbMode::Http { HashKey::Path } else { HashKey::Client },
            turn: 0,
            ring: Ring::new(),
            max_conns: 0,
            queue: None,
            queue_timeout: SEC,
            sticky: None,
            slow_start: 0,
            outlier: None,
            breaker: None,
            waiting: VecDeque::new(),
            clients: BTreeMap::new(),
            queued: 0,
            shed: 0,
            queue_timeouts: 0,
            max_waiting: 0,
            sticky_moved: 0,
            fail_fast: 0,
            wait: Window::new(300, SEC),
        }
    }
    /// it has a policy of SA6
    fn ext(&self) -> bool {
        self.queue.is_some() || self.sticky.is_some() || self.slow_start > 0 || self.outlier.is_some() || self.breaker.is_some()
    }
    /// the same settings (its numbers and state aside)
    fn same_cfg(&self, o: &Pool) -> bool {
        self.name == o.name && self.algo == o.algo && self.hash_key == o.hash_key && self.max_conns == o.max_conns && self.queue == o.queue && self.queue_timeout == o.queue_timeout && self.sticky == o.sticky && self.slow_start == o.slow_start && self.outlier == o.outlier && self.breaker == o.breaker
    }
}

/// where requests go (SA6): by Host and path prefix to a pool, with what a proxy rewrites
#[derive(Clone, Debug)]
pub struct Route {
    /// "" any
    pub host: String,
    pub prefix: String,
    pub pool: usize,
    pub strip: String,
    pub add: String,
    pub set_headers: Vec<(String, String)>,
    pub remove_headers: Vec<String>,
    pub cache: bool,
    pub retry: Option<RetryCfg>,
    pub limit: Option<LimitCfg>,
    limiter: Limiter,
    budget: Budget,
    pub requests: u64,
}

impl Route {
    /// "host/prefix" or the prefix
    pub fn label(&self) -> String {
        if self.host.is_empty() { self.prefix.clone() } else { format!("{}{}", self.host, self.prefix) }
    }
    fn same_place(&self, o: &Route) -> bool {
        self.host.eq_ignore_ascii_case(&o.host) && self.prefix == o.prefix && self.pool == o.pool
    }
}

/// what a proxy's cache keeps
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheCfg {
    pub entries: usize,
    pub max_kb: u64,
    pub ttl: Ns,
    pub stale: Ns,
    pub hit: Ns,
}

/// a change of a running load balancer's backends (Action::LbBackend)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LbOp {
    /// a new backend (or a removed one back, with this address)
    Add { name: String, addr: IpAddr, port: u16, weight: u32 },
    /// drains it, then it is gone
    Remove { name: String },
    /// drains it, then it is disabled
    Drain { name: String },
    /// takes a draining or disabled backend back
    Enable { name: String },
    Weight { name: String, weight: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LTimer {
    /// the next health check of a backend
    Check(usize),
    /// a TCP check takes too long (its socket)
    CheckTimeout(u32),
    /// a TCP pipe's connect takes too long
    Connect(u32),
    /// the drain time of a backend is over
    Drain(usize),
    /// a request's CPU work is done (res.rs)
    Gate(u64),
    /// SA6: the backoff of a request is over (its id)
    Retry(u64),
    /// a queued request waited too long (its id)
    Wait(u64),
    /// an answer's CPU work (TLS, a cache hit) is done (the outbox's id)
    Out(u64),
    /// an ejection or an open breaker may be over
    Wake,
}

/// an HTTP request on its way to a backend (and, SA6, between attempts or in a queue)
#[derive(Clone, Debug)]
struct Fwd {
    req: ReqIn,
    key: u64,
    b: usize,
    tried: Vec<usize>,
    sent: Ns,
    /// SA6: its route and pool, retries so far, sent as a breaker's probe, the cookie to set,
    /// the cache key of a miss, a revalidation in the background (its cache key)
    route: Option<usize>,
    pool: usize,
    attempt: u32,
    probe: bool,
    cookie: bool,
    cache_key: Option<String>,
    bg: bool,
}

impl Fwd {
    fn new(req: ReqIn, key: u64) -> Fwd {
        Fwd { req, key, b: 0, tried: Vec::new(), sent: 0, route: None, pool: 0, attempt: 0, probe: false, cookie: false, cache_key: None, bg: false }
    }
}

/// an answer that waits for its CPU work
#[derive(Clone, Debug)]
struct Outgoing {
    sid: u64,
    rep: Reply,
    at: Ns,
}

/// a TCP connection spliced to a backend
#[derive(Clone, Debug)]
struct Pipe {
    client: u32,
    back: Option<u32>,
    b: usize,
    key: u64,
    tried: Vec<usize>,
    connected: bool,
    /// bytes of the client for the backend, of the backend for the client
    up: Vec<u8>,
    down: Vec<u8>,
    c_eof: bool,
    b_eof: bool,
    c_shut: bool,
    b_shut: bool,
    c_closed: bool,
    b_closed: bool,
    start: Ns,
    timer: Option<u64>,
    /// counted as active at its backend
    counted: bool,
}

/// what sticky sessions say for a request
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stick {
    None,
    Hit(usize),
    /// its backend is not usable
    Moved(usize),
}

/// the load balancer (and the reverse proxy)
#[derive(Clone, Debug)]
pub struct Lb {
    /// a request's CPU work, the requests that wait for it, what it holds (res.rs)
    pub cpu: Ns,
    gate: crate::res::CpuGate<ReqIn>,
    pub mem: crate::res::SvcMem,
    pub mode: LbMode,
    pub port: u16,
    pub vnodes: u32,
    pub health: HealthCfg,
    pub retries: u32,
    pub timeout: Ns,
    pub connect_timeout: Ns,
    pub drain: Ns,
    pub max_conns: u32,
    pub keepalive: bool,
    pub tag: bool,
    pub backends: Vec<Backend>,
    /// the pools: 0 is "default", the top-level backends and their algorithm
    pub pools: Vec<Pool>,
    serve: Serve,
    agent: Agent,
    checker: Agent,
    listener: Option<u32>,
    wheel: Wheel<LTimer>,
    fwd: BTreeMap<u64, Fwd>,
    by_agent: BTreeMap<u64, u64>,
    hchecks: BTreeMap<u64, usize>,
    tchecks: BTreeMap<u32, (usize, u64)>,
    pipes: BTreeMap<u32, Pipe>,
    socks: BTreeMap<u32, (u32, bool)>,
    next_pipe: u32,
    pub requests: u64,
    pub responses: u64,
    pub errors: u64,
    pub no_backend: u64,
    pub retried: u64,
    pub by_class: [u64; 5],
    lat: Window,
    seen: Ns,
    // ---- SA6
    /// a policy of SA6 is set: the paths of traffic management run (else exactly the old ones)
    pub ext: bool,
    /// kind "proxy"
    pub proxy: bool,
    pub routes: Vec<Route>,
    pub retry: Option<RetryCfg>,
    pub limit: Option<LimitCfg>,
    limiter: Limiter,
    budget: Budget,
    rng: Pcg32,
    pub tls: Option<crate::tlssim::TlsSpec>,
    pub tls_cost: TlsCost,
    pub cache: Option<CacheCfg>,
    store: Lru<String, Cached>,
    revalidating: BTreeSet<String>,
    held: BTreeMap<u64, (Fwd, u64, Ns)>,
    later: BTreeMap<u64, Fwd>,
    outbox: BTreeMap<u64, Outgoing>,
    next_out: u64,
    next_bg: u64,
    tls_socks: BTreeSet<u32>,
    sessions: BTreeMap<IpAddr, Ns>,
    wake: Option<(Ns, u64)>,
    pub limited: u64,
    pub budget_denied: u64,
    pub ejections: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_stale: u64,
    pub bytes_saved: u64,
    pub handshakes: u64,
    pub resumed: u64,
    pub tls_cpu: Ns,
}

/// the names of the per-backend metrics (by position)
const M_REQ: [&str; 8] = ["b1_requests", "b2_requests", "b3_requests", "b4_requests", "b5_requests", "b6_requests", "b7_requests", "b8_requests"];
const M_ACT: [&str; 8] = ["b1_active", "b2_active", "b3_active", "b4_active", "b5_active", "b6_active", "b7_active", "b8_active"];
const M_UP: [&str; 8] = ["b1_up", "b2_up", "b3_up", "b4_up", "b5_up", "b6_up", "b7_up", "b8_up"];
const M_ERR: [&str; 8] = ["b1_errors", "b2_errors", "b3_errors", "b4_errors", "b5_errors", "b6_errors", "b7_errors", "b8_errors"];
const M_BRK: [&str; 8] = ["b1_breaker", "b2_breaker", "b3_breaker", "b4_breaker", "b5_breaker", "b6_breaker", "b7_breaker", "b8_breaker"];
const M_EJ: [&str; 8] = ["b1_ejected", "b2_ejected", "b3_ejected", "b4_ejected", "b5_ejected", "b6_ejected", "b7_ejected", "b8_ejected"];

/// headers that belong to one connection, not to the message
fn hop_by_hop(name: &str) -> bool {
    ["connection", "keep-alive", "proxy-connection", "transfer-encoding", "te", "trailer", "upgrade", "content-length"].iter().any(|h| name.eq_ignore_ascii_case(h))
}

/// how long an answer may be served stale while it is renewed (stale-while-revalidate)
fn stale_of(h: &Headers) -> Option<Ns> {
    let cc = h.get("cache-control")?.to_ascii_lowercase();
    cc.split(',').find_map(|p| p.trim().strip_prefix("stale-while-revalidate=")?.trim().parse::<u64>().ok()).map(|s| s * SEC)
}

impl Lb {
    /// the CPU work of a request at a reference core unless the config says otherwise (res.rs)
    const CPU: Ns = 50 * crate::kernel::US;

    pub fn new(mode: LbMode, port: u16) -> Lb {
        let cfg = ServeCfg { port, server: String::new(), max_requests: 0, max_body: 64 << 20, ..ServeCfg::default() };
        Lb {
            mode,
            port,
            vnodes: 64,
            health: HealthCfg { kind: if mode == LbMode::Http { CheckKind::Http } else { CheckKind::Tcp }, path: String::from("/"), interval: 2 * SEC, timeout: SEC, rise: 2, fall: 3 },
            retries: 1,
            timeout: 10 * SEC,
            connect_timeout: SEC,
            drain: 30 * SEC,
            max_conns: 0,
            keepalive: true,
            tag: true,
            cpu: Lb::CPU,
            gate: crate::res::CpuGate::default(),
            mem: crate::res::SvcMem::default(),
            backends: Vec::new(),
            pools: alloc::vec![Pool::new("default", mode)],
            serve: Serve::new(cfg, K_SERVE),
            agent: Agent::new(K_AGENT),
            checker: Agent::new(K_CHECK),
            listener: None,
            wheel: Wheel::new(K_OWN),
            fwd: BTreeMap::new(),
            by_agent: BTreeMap::new(),
            hchecks: BTreeMap::new(),
            tchecks: BTreeMap::new(),
            pipes: BTreeMap::new(),
            socks: BTreeMap::new(),
            next_pipe: 0,
            requests: 0,
            responses: 0,
            errors: 0,
            no_backend: 0,
            retried: 0,
            by_class: [0; 5],
            lat: Window::new(300, SEC),
            seen: 0,
            ext: false,
            proxy: false,
            routes: Vec::new(),
            retry: None,
            limit: None,
            limiter: Limiter::default(),
            budget: Budget::default(),
            rng: Pcg32::new(0, 0),
            tls: None,
            tls_cost: TlsCost::default(),
            cache: None,
            store: Lru::new(0, 0),
            revalidating: BTreeSet::new(),
            held: BTreeMap::new(),
            later: BTreeMap::new(),
            outbox: BTreeMap::new(),
            next_out: 0,
            next_bg: 0,
            tls_socks: BTreeSet::new(),
            sessions: BTreeMap::new(),
            wake: None,
            limited: 0,
            budget_denied: 0,
            ejections: 0,
            cache_hits: 0,
            cache_misses: 0,
            cache_stale: 0,
            bytes_saved: 0,
            handshakes: 0,
            resumed: 0,
            tls_cpu: 0,
        }
    }

    /// the algorithm of the pool "default"
    pub fn algo(&self) -> Algo {
        self.pools[0].algo
    }

    /// adds a backend to the configuration (before it runs)
    pub fn add_backend(&mut self, name: &str, addr: impl Into<IpAddr>, port: u16, weight: u32) {
        self.add_backend_to(name, addr.into(), port, weight, 0);
    }

    fn add_backend_to(&mut self, name: &str, addr: IpAddr, port: u16, weight: u32, pool: usize) {
        let mut b = Backend::new(name, addr, port, weight, 0);
        b.pool = pool;
        self.backends.push(b);
        self.rering();
    }

    fn rering(&mut self) {
        for p in 0..self.pools.len() {
            let mut r = Ring::new();
            for (i, b) in self.backends.iter().enumerate() {
                if !b.gone() && b.pool == p {
                    r.add(i as u32, &b.name, self.vnodes);
                }
            }
            self.pools[p].ring = r;
        }
    }

    /// whether anything of SA6 is configured
    fn compute_ext(&mut self) {
        self.ext = self.proxy || !self.routes.is_empty() || self.pools.len() > 1 || self.pools.iter().any(|p| p.ext()) || self.retry.is_some() || self.limit.is_some();
    }

    /// the backend takes something new
    fn usable(&self, i: usize) -> bool {
        let b = &self.backends[i];
        let base = b.admin == Admin::Active && b.up && (self.mode == LbMode::Http || self.max_conns == 0 || b.active < self.max_conns);
        if !base || !self.ext {
            return base;
        }
        let pl = &self.pools[b.pool];
        if b.ejected_until.is_some() {
            return false;
        }
        if let Some(c) = &pl.breaker
            && !b.breaker.admits(c)
        {
            return false;
        }
        !(pl.queue.is_some() && pl.max_conns > 0 && b.active >= pl.max_conns)
    }

    /// usable but for max_conns of its pool (a request may wait for it)
    fn busy_only(&self, i: usize) -> bool {
        let b = &self.backends[i];
        let pl = &self.pools[b.pool];
        b.admin == Admin::Active && b.up && b.ejected_until.is_none() && pl.breaker.as_ref().is_none_or(|c| b.breaker.admits(c)) && pl.max_conns > 0 && b.active >= pl.max_conns
    }

    /// slow start: the weight of backend i now, in thousandths
    fn weight_now(&self, i: usize, now: Ns) -> i64 {
        let b = &self.backends[i];
        let pl = &self.pools[b.pool];
        let w = if pl.algo == Algo::RoundRobin { 1000 } else { b.weight as i64 * 1000 };
        match b.ramp_from {
            Some(t) if pl.slow_start > 0 && now < t + pl.slow_start => (w * (now.saturating_sub(t)) as i64 / pl.slow_start as i64).max(1),
            _ => w,
        }
    }

    /// a backend of pool p by its algorithm, none of `skip`
    fn pick(&mut self, p: usize, key: u64, skip: &[usize]) -> Option<usize> {
        let n = self.backends.len();
        let ok: Vec<bool> = (0..n).map(|i| self.backends[i].pool == p && self.usable(i) && !skip.contains(&i)).collect();
        if !ok.iter().any(|&x| x) {
            return None;
        }
        let now = self.seen;
        let algo = self.pools[p].algo;
        if self.pools[p].slow_start > 0 && matches!(algo, Algo::RoundRobin | Algo::Weighted) && (0..n).any(|i| ok[i] && self.weight_now(i, now) < if algo == Algo::RoundRobin { 1000 } else { self.backends[i].weight as i64 * 1000 }) {
            // smooth weighted round robin on the rising weights
            let w: Vec<i64> = (0..n).map(|i| if ok[i] { self.weight_now(i, now) } else { 0 }).collect();
            let total: i64 = w.iter().sum();
            let mut best: Option<usize> = None;
            for i in 0..n {
                if !ok[i] {
                    continue;
                }
                self.backends[i].cw += w[i];
                let cw = self.backends[i].cw;
                if best.is_none_or(|x| cw > self.backends[x].cw) {
                    best = Some(i);
                }
            }
            if let Some(x) = best {
                self.backends[x].cw -= total;
            }
            return best;
        }
        match algo {
            Algo::RoundRobin => {
                for k in 0..n {
                    let i = (self.pools[p].turn + k) % n;
                    if ok[i] {
                        self.pools[p].turn = i + 1;
                        return Some(i);
                    }
                }
                None
            }
            Algo::LeastConn => {
                let mut best: Option<usize> = None;
                for k in 0..n {
                    let i = (self.pools[p].turn + k) % n;
                    if ok[i] && best.is_none_or(|b| self.backends[i].active < self.backends[b].active) {
                        best = Some(i);
                    }
                }
                if let Some(b) = best {
                    self.pools[p].turn = b + 1;
                }
                best
            }
            Algo::Weighted => {
                let total: i64 = (0..n).filter(|&i| ok[i]).map(|i| self.backends[i].weight as i64).sum();
                let mut best: Option<usize> = None;
                for i in 0..n {
                    if !ok[i] {
                        continue;
                    }
                    self.backends[i].cw += self.backends[i].weight as i64;
                    let cw = self.backends[i].cw;
                    if best.is_none_or(|x| cw > self.backends[x].cw) {
                        best = Some(i);
                    }
                }
                if let Some(x) = best {
                    self.backends[x].cw -= total;
                }
                best
            }
            Algo::Hash => self.pools[p].ring.preference_by(key, 1, |m| ok.get(*m as usize).copied().unwrap_or(false)).first().map(|m| *m as usize),
        }
    }

    fn key_of(&self, r: &ReqIn, p: usize) -> u64 {
        match &self.pools[p].hash_key {
            HashKey::Path => hash64(r.head.target.as_bytes()),
            HashKey::Client => hash64(&ip_bytes(r.peer.0)),
            HashKey::Header(h) => hash64(r.head.header(h).unwrap_or("").as_bytes()),
        }
    }

    // ---------------------------------------------------------------- health checks

    fn start_check(&mut self, os: &mut OsApi, i: usize) {
        let now = os.now();
        let b = &self.backends[i];
        if b.gone() || b.checking || self.health.kind == CheckKind::None {
            return;
        }
        let (addr, port) = (b.addr, b.port);
        self.backends[i].checking = true;
        match self.health.kind {
            CheckKind::Http => {
                let url = Url { host: format!("{}", addr), port, target: self.health.path.clone() };
                let mut h = Headers::new();
                h.add("User-Agent", "jfn-lb-check/1.0");
                let req = Req { method: String::from("GET"), url, version: Version::Http11, headers: h, body: Vec::new(), timeout: self.health.timeout, connect_timeout: self.health.timeout, reuse: false, keep_body: false };
                let id = self.checker.submit(os, req);
                self.hchecks.insert(id, i);
            }
            CheckKind::Tcp => match os.tcp_connect(addr, port, SockOpts::default()) {
                Ok(s) => {
                    let t = self.wheel.add(now + self.health.timeout, LTimer::CheckTimeout(s));
                    self.tchecks.insert(s, (i, t));
                }
                Err(_) => self.check_done(os, i, false),
            },
            CheckKind::None => {}
        }
    }

    /// a check of backend i ended
    fn check_done(&mut self, os: &mut OsApi, i: usize, ok: bool) {
        let now = os.now();
        let (rise, fall) = (self.health.rise.max(1), self.health.fall.max(1));
        let Some(b) = self.backends.get_mut(i) else {
            return;
        };
        b.checking = false;
        b.checks += 1;
        b.last_check = Some((now, ok));
        let mut change = None;
        if ok {
            b.fail_run = 0;
            b.ok_run += 1;
            if !b.up && b.ok_run >= rise {
                b.up = true;
                b.since = now;
                change = Some(true);
            }
        } else {
            b.check_fails += 1;
            b.ok_run = 0;
            b.fail_run += 1;
            if b.up && b.fail_run >= fall {
                b.up = false;
                b.since = now;
                b.downs += 1;
                change = Some(false);
            }
        }
        if let Some(up) = change {
            let name = b.name.clone();
            let n = if up { b.ok_run } else { b.fail_run };
            os.note(Note::LbHealth { backend: name.clone(), up });
            let text = if up { format!("Backend {} ist wieder verfügbar ({} Prüfungen erfolgreich)", name, n) } else { format!("Backend {} fällt aus ({} Prüfungen ohne Erfolg)", name, n) };
            os.log(if up { 5 } else { 4 }, "lb", &text);
            if up && self.ext {
                self.ramp(i, now);
                let p = self.backends[i].pool;
                self.dequeue(os, p);
            }
        }
    }

    /// slow start begins for backend i
    fn ramp(&mut self, i: usize, now: Ns) {
        let p = self.backends[i].pool;
        if self.pools.get(p).is_some_and(|pl| pl.slow_start > 0) {
            self.backends[i].ramp_from = Some(now);
        }
    }

    // ---------------------------------------------------------------- HTTP

    fn arrive(&mut self, os: &mut OsApi, r: ReqIn) {
        self.requests += 1;
        // its work on the request first: the backend is picked after it (res.rs)
        let extra = if self.tls.is_some() { self.tls_in(os, &r) } else { 0 };
        let (now, at) = (os.now(), os.cpu(self.cpu + extra));
        if at > now {
            let id = self.gate.hold(r);
            self.wheel.add(at, LTimer::Gate(id));
            return;
        }
        self.pass(os, r);
    }

    /// TLS on the way in: a handshake for a new connection, the bytes of the request (CPU at a
    /// reference core; it costs nothing without hardware, but counts)
    fn tls_in(&mut self, os: &mut OsApi, r: &ReqIn) -> Ns {
        let now = os.now();
        let mut cost = self.tls_cost.bytes(r.head.len as u64 + r.body.len() as u64);
        if self.tls_socks.insert(r.sock) {
            if self.tls_socks.len() > 2 * self.serve.conns() + 16 {
                let serve = &self.serve;
                self.tls_socks.retain(|s| serve.owns(*s));
                self.tls_socks.insert(r.sock);
            }
            let peer = r.peer.0;
            let resumed = self.tls_cost.reuse && self.sessions.get(&peer).is_some_and(|t| now < t + self.tls_cost.session_ttl);
            let hs = if resumed { self.tls_cost.resume } else { self.tls_cost.handshake };
            if resumed {
                self.resumed += 1;
            } else {
                self.handshakes += 1;
                if self.tls_cost.reuse {
                    if self.sessions.len() >= CLIENTS_MAX && !self.sessions.contains_key(&peer) {
                        let ttl = self.tls_cost.session_ttl;
                        self.sessions.retain(|_, t| now < *t + ttl);
                        if self.sessions.len() >= CLIENTS_MAX
                            && let Some(k) = self.sessions.keys().next().copied()
                        {
                            self.sessions.remove(&k);
                        }
                    }
                    self.sessions.insert(peer, now);
                }
            }
            cost += hs;
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::Handshake { resumed, us: if os.metered() { hs / US } else { 0 } }));
            }
        }
        if os.metered() {
            self.tls_cpu += cost;
            cost
        } else {
            0
        }
    }

    /// a backend for the request, or 503
    fn pass(&mut self, os: &mut OsApi, r: ReqIn) {
        if self.ext {
            self.pass_ext(os, r);
            return;
        }
        let key = self.key_of(&r, 0);
        match self.pick(0, key, &[]) {
            Some(b) => self.forward(os, Fwd::new(r, key), b),
            None => {
                self.no_backend += 1;
                os.note(Note::LbNoBackend);
                self.reply_error(os, &r, 503);
            }
        }
    }

    fn forward(&mut self, os: &mut OsApi, mut f: Fwd, b: usize) {
        let now = os.now();
        let be = &self.backends[b];
        let r = &f.req;
        let mut h = Headers::new();
        let route = f.route.and_then(|i| self.routes.get(i));
        for (k, v) in &r.head.headers.0 {
            if !hop_by_hop(k) && !k.eq_ignore_ascii_case("x-forwarded-for") && !(self.proxy && (k.eq_ignore_ascii_case("x-forwarded-proto") || k.eq_ignore_ascii_case("x-forwarded-host"))) && !route.is_some_and(|rt| rt.remove_headers.iter().any(|x| x.eq_ignore_ascii_case(k))) {
                h.add(k, v);
            }
        }
        let xff = match r.head.header("x-forwarded-for") {
            Some(x) => format!("{}, {}", x, r.peer.0),
            None => format!("{}", r.peer.0),
        };
        h.add("X-Forwarded-For", &xff);
        let mut target = r.head.target.clone();
        if self.proxy {
            h.add("X-Forwarded-Proto", if self.tls.is_some() { "https" } else { "http" });
            if let Some(host) = r.head.header("host") {
                h.add("X-Forwarded-Host", host);
            }
            if let Some(rt) = route {
                for (k, v) in &rt.set_headers {
                    h.set(k, v);
                }
                if !rt.strip.is_empty()
                    && let Some(rest) = target.strip_prefix(rt.strip.as_str())
                {
                    target = if rest.starts_with('/') || rest.starts_with('?') { String::from(rest) } else { format!("/{}", rest) };
                }
                if !rt.add.is_empty() {
                    target = format!("{}{}", rt.add.trim_end_matches('/'), target);
                }
            }
        }
        let url = Url { host: format!("{}", be.addr), port: be.port, target };
        let req = Req { method: r.head.method.clone(), url, version: Version::Http11, headers: h, body: r.body.clone(), timeout: self.timeout, connect_timeout: self.connect_timeout, reuse: self.keepalive, keep_body: true };
        if os.recording() {
            let algo = self.pools[be.pool].algo.name();
            os.note(Note::LbPick { backend: be.name.clone(), algo });
        }
        let aid = self.agent.submit(os, req);
        let be = &mut self.backends[b];
        be.active += 1;
        be.total += 1;
        be.bytes_out += f.req.body.len() as u64;
        if self.ext {
            f.probe = self.pools[be.pool].breaker.is_some() && be.breaker.sent();
        }
        let sid = f.req.id;
        f.b = b;
        f.sent = now;
        self.by_agent.insert(aid, sid);
        self.fwd.insert(sid, f);
    }

    fn reply_error(&mut self, os: &mut OsApi, r: &ReqIn, status: u16) {
        let now = os.now();
        self.errors += 1;
        let mut rep = Reply::status_page(status);
        if status == 503 {
            rep.headers.add("Retry-After", "1");
        }
        if self.serve.respond(os, r.id, rep) {
            self.responses += 1;
            self.by_class[4] += 1;
            self.lat.record(now, now - r.at);
        }
    }

    fn answered(&mut self, os: &mut OsApi, d: Done) {
        let now = os.now();
        let Some(sid) = self.by_agent.remove(&d.id) else {
            return;
        };
        let Some(mut f) = self.fwd.remove(&sid) else {
            return;
        };
        let b = f.b;
        if let Some(be) = self.backends.get_mut(b) {
            be.active = be.active.saturating_sub(1);
        }
        if self.ext {
            self.answered_ext(os, d, f);
            return;
        }
        match d.result {
            Ok(resp) => {
                let st = resp.head.status;
                let class = ((st / 100).clamp(1, 5) - 1) as usize;
                if let Some(be) = self.backends.get_mut(b) {
                    be.by_class[class] += 1;
                    be.bytes_in += resp.body.len() as u64;
                    be.lat.record(now, now - f.sent);
                    if st >= 500 {
                        be.errors += 1;
                    }
                }
                let mut h = Headers::new();
                for (k, v) in &resp.head.headers.0 {
                    if !hop_by_hop(k) {
                        h.add(k, v);
                    }
                }
                if self.tag
                    && let Some(be) = self.backends.get(b)
                {
                    h.set("X-Served-By", &be.name);
                }
                let rep = Reply { status: st, headers: h, body: resp.body, chunk: 0, close: false };
                if self.serve.respond(os, sid, rep) {
                    self.responses += 1;
                    self.by_class[class] += 1;
                    self.lat.record(now, now - f.req.at);
                }
            }
            Err(fail) => {
                if let Some(be) = self.backends.get_mut(b) {
                    be.errors += 1;
                }
                let idempotent = matches!(f.req.head.method.as_str(), "GET" | "HEAD");
                let retry = matches!(fail, Fail::Connect(_) | Fail::NoRoute(_) | Fail::ConnectTimeout) || (idempotent && matches!(fail, Fail::Timeout { .. } | Fail::Reset(_) | Fail::Empty));
                f.tried.push(b);
                if retry && f.tried.len() <= self.retries as usize {
                    if let Some(nb) = self.pick(0, f.key, &f.tried) {
                        self.retried += 1;
                        let name = self.backends[b].name.clone();
                        os.note(Note::LbRetry { backend: name });
                        self.forward(os, f, nb);
                        self.drained(os, b);
                        return;
                    }
                }
                let status = if matches!(fail, Fail::Timeout { .. }) { 504 } else { 502 };
                self.reply_error(os, &f.req, status);
            }
        }
        self.drained(os, b);
    }

    // ---------------------------------------------------------------- HTTP with traffic management (SA6)

    /// the route of a request: the longest prefix, a route with the request's Host before one without
    fn route_of(&self, r: &ReqIn) -> Option<usize> {
        let host = r.head.header("host").map(|h| h.rsplit_once(':').filter(|(a, p)| !a.ends_with(']') && !a.is_empty() && p.bytes().all(|c| c.is_ascii_digit())).map_or(h, |(a, _)| a)).unwrap_or("");
        let path = r.head.target.as_str();
        let mut best: Option<(usize, (bool, usize))> = None;
        for (i, rt) in self.routes.iter().enumerate() {
            let host_ok = rt.host.is_empty() || rt.host.eq_ignore_ascii_case(host);
            if host_ok && path.starts_with(rt.prefix.as_str()) {
                let rank = (!rt.host.is_empty(), rt.prefix.len());
                if best.is_none_or(|(_, r)| rank > r) {
                    best = Some((i, rank));
                }
            }
        }
        best.map(|(i, _)| i)
    }

    /// the retry policy of a request: its route's, else the top-level one
    fn retry_of(&self, route: Option<usize>) -> Option<&RetryCfg> {
        route.and_then(|i| self.routes[i].retry.as_ref()).or(self.retry.as_ref())
    }

    fn budget_of(&mut self, route: Option<usize>) -> &mut Budget {
        match route {
            Some(i) if self.routes[i].retry.is_some() => &mut self.routes[i].budget,
            _ => &mut self.budget,
        }
    }

    /// limits, routes, the cache and then a backend
    fn pass_ext(&mut self, os: &mut OsApi, r: ReqIn) {
        let now = os.now();
        self.refresh(os);
        // the rate limits: the top-level one, then the route's
        if let Some(cfg) = self.limit.clone() {
            let key = limit_key(&cfg, &r, "");
            if let Err(after) = self.limiter.take(now, &cfg, &key) {
                self.refuse_limited(os, &r, &cfg, &key, after);
                return;
            }
        }
        let route = self.route_of(&r);
        if let Some(i) = route {
            self.routes[i].requests += 1;
            if let Some(cfg) = self.routes[i].limit.clone() {
                let key = limit_key(&cfg, &r, &self.routes[i].label());
                if let Err(after) = self.routes[i].limiter.take(now, &cfg, &key) {
                    self.refuse_limited(os, &r, &cfg, &key, after);
                    return;
                }
            }
        } else if !self.routes.is_empty() && !self.backends.iter().any(|b| b.pool == 0 && !b.gone()) {
            self.errors += 1;
            let mut rep = Reply::text(404, "404 Not Found: no route of the proxy takes this request\n");
            rep.headers.add("X-Jfn-Proxy", "no-route");
            self.send_reply(os, r.id, r.at, rep, 0);
            return;
        }
        let pool = route.map_or(0, |i| self.routes[i].pool);
        if os.recording()
            && let Some(i) = route
        {
            os.note(Note::Traffic(TrafficNote::Routed { route: self.routes[i].label(), pool: self.pools[pool].name.clone() }));
        }
        // the cache
        let cache_key = if self.cache.is_some() && route.is_none_or(|i| self.routes[i].cache) && matches!(r.head.method.as_str(), "GET" | "HEAD") {
            let key = format!("{} {}:{}", r.head.method, route.map_or(0, |i| i + 1), r.head.target);
            if self.cache_lookup(os, &r, &key, route, pool) {
                return;
            }
            Some(key)
        } else {
            None
        };
        if self.retry_of(route).is_some() {
            self.budget_of(route).request(now);
        }
        let key = self.key_of(&r, pool);
        let mut f = Fwd::new(r, key);
        f.route = route;
        f.pool = pool;
        f.cache_key = cache_key;
        self.dispatch(os, f);
    }

    /// answers from the cache when it can (true), starts a revalidation for a stale entry
    fn cache_lookup(&mut self, os: &mut OsApi, r: &ReqIn, key: &str, route: Option<usize>, pool: usize) -> bool {
        let now = os.now();
        let Some(cfg) = self.cache.clone() else {
            return false;
        };
        let k = String::from(key);
        let (fresh, stale) = match self.store.get(&k) {
            Some(c) if c.expires > now => (Some(c.clone()), false),
            Some(c) => {
                let window = stale_of(&c.headers).unwrap_or(cfg.stale);
                if now < c.expires + window { (Some(c.clone()), true) } else { (None, false) }
            }
            None => (None, false),
        };
        let Some(c) = fresh else {
            if self.store.peek(&k).is_some() {
                self.store.remove(&k);
            }
            self.cache_misses += 1;
            return false;
        };
        if stale {
            self.cache_stale += 1;
            if self.revalidating.insert(k.clone()) {
                self.next_bg += 1;
                let mut req = r.clone();
                req.id = BG + self.next_bg;
                let key = self.key_of(&req, pool);
                let mut f = Fwd::new(req, key);
                f.route = route;
                f.pool = pool;
                f.cache_key = Some(k.clone());
                f.bg = true;
                self.dispatch(os, f);
            }
        } else {
            self.cache_hits += 1;
        }
        self.bytes_saved += c.body.len() as u64;
        if os.recording() {
            os.note(Note::Traffic(TrafficNote::CacheServed { key: k.clone(), stale }));
        }
        let mut h = Headers::new();
        for (name, v) in &c.headers.0 {
            if !hop_by_hop(name) && !name.eq_ignore_ascii_case("x-cache") && !name.eq_ignore_ascii_case("age") {
                h.add(name, v);
            }
        }
        h.add("X-Cache", if stale { "STALE" } else { "HIT" });
        h.add("Age", &format!("{}", now.saturating_sub(c.stored) / SEC));
        let rep = Reply { status: c.status, headers: h, body: if r.head.method == "HEAD" { Vec::new() } else { c.body }, chunk: 0, close: false };
        self.send_reply(os, r.id, r.at, rep, cfg.hit);
        true
    }

    /// what sticky sessions say for a request
    fn stick(&self, f: &Fwd) -> Stick {
        let p = f.pool;
        match &self.pools[p].sticky {
            Some(StickyCfg::Cookie { name }) => {
                let Some(v) = f.req.head.header("cookie").and_then(|c| cookie_value(c, name)) else {
                    return Stick::None;
                };
                match self.backends.iter().position(|b| b.pool == p && !b.gone() && b.name == v) {
                    Some(i) if self.usable(i) && !f.tried.contains(&i) => Stick::Hit(i),
                    Some(i) => Stick::Moved(i),
                    None => Stick::None,
                }
            }
            Some(StickyCfg::Client { ttl }) => match self.pools[p].clients.get(&f.req.peer.0) {
                Some(&(i, last)) if *ttl == 0 || self.seen < last + ttl => {
                    if i < self.backends.len() && self.backends[i].pool == p && !self.backends[i].gone() && self.usable(i) && !f.tried.contains(&i) {
                        Stick::Hit(i)
                    } else {
                        Stick::Moved(i)
                    }
                }
                _ => Stick::None,
            },
            None => Stick::None,
        }
    }

    /// a backend for a request of the new paths, or what happens without one
    fn dispatch(&mut self, os: &mut OsApi, mut f: Fwd) {
        let p = f.pool;
        let st = self.stick(&f);
        let policy = self.retry_of(f.route).is_some();
        let pick = match st {
            Stick::Hit(i) => Some(i),
            _ => {
                let first = self.pick(p, f.key, &f.tried);
                // with a retry policy the same backend again when it is the only one left
                if first.is_none() && policy && !f.tried.is_empty() { self.pick(p, f.key, &[]) } else { first }
            }
        };
        let Some(b) = pick else {
            self.no_pick(os, f);
            return;
        };
        if let Stick::Moved(from) = st {
            self.pools[p].sticky_moved += 1;
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::StickyMoved { from: self.backends[from].name.clone(), to: self.backends[b].name.clone() }));
            }
        }
        match self.pools[p].sticky.clone() {
            Some(StickyCfg::Cookie { name }) => {
                f.cookie = f.req.head.header("cookie").and_then(|c| cookie_value(c, &name)) != Some(self.backends[b].name.as_str());
            }
            Some(StickyCfg::Client { ttl }) => {
                let now = os.now();
                let pl = &mut self.pools[p];
                if pl.clients.len() >= CLIENTS_MAX && !pl.clients.contains_key(&f.req.peer.0) {
                    pl.clients.retain(|_, (_, last)| ttl > 0 && now < *last + ttl);
                    if pl.clients.len() >= CLIENTS_MAX
                        && let Some(k) = pl.clients.keys().next().copied()
                    {
                        pl.clients.remove(&k);
                    }
                }
                pl.clients.insert(f.req.peer.0, (b, now));
            }
            None => {}
        }
        self.forward(os, f, b);
    }

    /// no backend can take it now: the queue, or 503
    fn no_pick(&mut self, os: &mut OsApi, f: Fwd) {
        let now = os.now();
        let p = f.pool;
        let n = self.backends.len();
        let waits = self.pools[p].queue.is_some() && (0..n).any(|i| self.backends[i].pool == p && self.busy_only(i));
        if waits {
            let q = self.pools[p].queue.unwrap_or(0);
            if (self.pools[p].waiting.len() as u64) < q as u64 {
                let sid = f.req.id;
                let t = self.wheel.add(now + self.pools[p].queue_timeout, LTimer::Wait(sid));
                let pl = &mut self.pools[p];
                pl.waiting.push_back(sid);
                pl.queued += 1;
                pl.max_waiting = pl.max_waiting.max(pl.waiting.len() as u32);
                let waiting = pl.waiting.len() as u32;
                if os.recording() {
                    os.note(Note::Traffic(TrafficNote::Queued { pool: self.pools[p].name.clone(), waiting }));
                }
                self.held.insert(sid, (f, t, now));
                return;
            }
            self.pools[p].shed += 1;
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::Shed { pool: self.pools[p].name.clone() }));
            }
            self.give_up(os, f, 503, "503 Service Unavailable: every backend is at max_conns and the queue is full\n");
            return;
        }
        let alive: Vec<usize> = (0..n).filter(|&i| self.backends[i].pool == p && self.backends[i].admin == Admin::Active && self.backends[i].up && self.backends[i].ejected_until.is_none()).collect();
        if let Some(c) = self.pools[p].breaker.clone()
            && !alive.is_empty()
            && alive.iter().all(|&i| !self.backends[i].breaker.admits(&c))
        {
            self.pools[p].fail_fast += 1;
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::FailFast { pool: self.pools[p].name.clone() }));
            }
            self.give_up(os, f, 503, "503 Service Unavailable: every circuit breaker of the pool is open\n");
            return;
        }
        if !f.bg {
            self.no_backend += 1;
            os.note(Note::LbNoBackend);
        }
        self.give_up(os, f, 503, "503 Service Unavailable\n");
    }

    /// a request ends with an error of the balancer's own
    fn give_up(&mut self, os: &mut OsApi, f: Fwd, status: u16, text: &str) {
        if f.bg {
            if let Some(k) = &f.cache_key {
                self.revalidating.remove(k);
            }
            return;
        }
        self.errors += 1;
        let mut rep = Reply::text(status, text);
        if status == 503 {
            rep.headers.add("Retry-After", "1");
        }
        self.send_reply(os, f.req.id, f.req.at, rep, 0);
    }

    fn refuse_limited(&mut self, os: &mut OsApi, r: &ReqIn, cfg: &LimitCfg, key: &str, after: u64) {
        self.limited += 1;
        if os.recording() {
            os.note(Note::Traffic(TrafficNote::Limited { key: String::from(key), status: cfg.status, retry_after: after }));
        }
        self.errors += 1;
        let mut rep = Reply::text(cfg.status, &format!("{} {}: rate limit, try again in {} s\n", cfg.status, netpkt::http::reason(cfg.status), after));
        rep.headers.add("Retry-After", &format!("{}", after));
        self.send_reply(os, r.id, r.at, rep, 0);
    }

    /// an answer goes back: after its CPU work (TLS bytes, `work` for a cache hit), counted
    fn send_reply(&mut self, os: &mut OsApi, sid: u64, at: Ns, rep: Reply, work: Ns) {
        let now = os.now();
        let tls = if self.tls.is_some() && os.metered() { self.tls_cost.bytes(rep.body.len() as u64 + HEAD_BYTES) } else { 0 };
        self.tls_cpu += tls;
        let ready = if os.metered() { os.cpu(work + tls) } else { now + work };
        if ready > now {
            self.next_out += 1;
            let id = self.next_out;
            self.outbox.insert(id, Outgoing { sid, rep, at });
            self.wheel.add(ready, LTimer::Out(id));
            return;
        }
        self.respond_now(os, sid, at, rep);
    }

    fn respond_now(&mut self, os: &mut OsApi, sid: u64, at: Ns, rep: Reply) {
        let now = os.now();
        let class = ((rep.status / 100).clamp(1, 5) - 1) as usize;
        if self.serve.respond(os, sid, rep) {
            self.responses += 1;
            self.by_class[class] += 1;
            self.lat.record(now, now - at);
        }
    }

    /// breaker and outlier ejection learn how an attempt at backend b ended
    fn judge(&mut self, os: &mut OsApi, b: usize, out: Outcome, probe: bool) {
        let now = os.now();
        let Some(p) = self.backends.get(b).map(|x| x.pool) else {
            return;
        };
        let failed = out.failed();
        if let Some(cfg) = self.pools[p].breaker.clone()
            && let Some(st) = self.backends[b].breaker.result(now, &cfg, !failed, probe)
        {
            let name = self.backends[b].name.clone();
            os.note(Note::Traffic(TrafficNote::Breaker { backend: name.clone(), state: st }));
            os.log(if st == BState::Open { 4 } else { 5 }, "lb", &format!("Circuit Breaker von {}: {}", name, st.word().text()));
            if st == BState::Open {
                self.wake_at(os, self.backends[b].breaker.until);
            } else if st == BState::Closed {
                self.dequeue(os, p);
            }
        }
        if let Some(cfg) = self.pools[p].outlier.clone() {
            if !failed {
                self.backends[b].out_fails = 0;
            } else if self.backends[b].ejected_until.is_none() {
                self.backends[b].out_fails += 1;
                if self.backends[b].out_fails >= cfg.consecutive {
                    self.backends[b].out_fails = 0;
                    let live = self.backends.iter().filter(|x| x.pool == p && !x.gone()).count();
                    let ejected = self.backends.iter().filter(|x| x.pool == p && !x.gone() && x.ejected_until.is_some()).count();
                    let name = self.backends[b].name.clone();
                    if ejected < cfg.cap(live) {
                        let be = &mut self.backends[b];
                        be.ejections += 1;
                        let t = cfg.time(be.ejections);
                        be.ejected_until = Some(now + t);
                        self.ejections += 1;
                        os.note(Note::Traffic(TrafficNote::Ejected { backend: name.clone(), ms: t / MS }));
                        os.log(4, "lb", &format!("Backend {} für {} ms ausgeschlossen (Ausreißer)", name, t / MS));
                        self.wake_at(os, now + t);
                    } else if os.recording() {
                        os.note(Note::Traffic(TrafficNote::NotEjected { backend: name }));
                    }
                }
            }
        }
    }

    /// a timer for the next moment an ejection or an open breaker ends
    fn wake_at(&mut self, _os: &mut OsApi, at: Ns) {
        if let Some((t, id)) = self.wake {
            if t <= at {
                return;
            }
            self.wheel.cancel(id);
        }
        let id = self.wheel.add(at, LTimer::Wake);
        self.wake = Some((at, id));
    }

    /// ejections and open breakers whose time is over
    fn refresh(&mut self, os: &mut OsApi) {
        let now = os.now();
        let mut next: Option<Ns> = None;
        let mut back: Vec<usize> = Vec::new();
        for i in 0..self.backends.len() {
            let p = self.backends[i].pool;
            if let Some(t) = self.backends[i].ejected_until {
                if t <= now {
                    self.backends[i].ejected_until = None;
                    self.ramp(i, t);
                    back.push(i);
                    if os.recording() {
                        os.note(Note::Traffic(TrafficNote::Returned { backend: self.backends[i].name.clone() }));
                    }
                } else {
                    next = Some(next.map_or(t, |x| x.min(t)));
                }
            }
            if self.pools[p].breaker.is_some() {
                if self.backends[i].breaker.refresh(now) {
                    back.push(i);
                    os.note(Note::Traffic(TrafficNote::Breaker { backend: self.backends[i].name.clone(), state: BState::HalfOpen }));
                } else if self.backends[i].breaker.state == BState::Open {
                    let t = self.backends[i].breaker.until;
                    next = Some(next.map_or(t, |x| x.min(t)));
                }
            }
        }
        if let Some(t) = next {
            self.wake_at(os, t);
        }
        let mut pools: Vec<usize> = back.iter().map(|&i| self.backends[i].pool).collect();
        pools.dedup();
        for p in pools {
            self.dequeue(os, p);
        }
    }

    /// queued requests of pool p go while its backends take them
    fn dequeue(&mut self, os: &mut OsApi, p: usize) {
        let now = os.now();
        while let Some(&sid) = self.pools.get(p).and_then(|pl| pl.waiting.front()) {
            let n = self.backends.len();
            if !(0..n).any(|i| self.backends[i].pool == p && self.usable(i)) {
                return;
            }
            self.pools[p].waiting.pop_front();
            let Some((f, t, since)) = self.held.remove(&sid) else {
                continue;
            };
            self.wheel.cancel(t);
            self.pools[p].wait.record(now, now - since);
            self.dispatch(os, f);
        }
    }

    fn answered_ext(&mut self, os: &mut OsApi, d: Done, mut f: Fwd) {
        let now = os.now();
        let b = f.b;
        let out = outcome_of(&d.result);
        self.judge(os, b, out, f.probe);
        f.probe = false;
        let method = f.req.head.method.clone();
        // a retry by the policy
        if let Some(cfg) = self.retry_of(f.route).cloned() {
            if out.failed() && cfg.repeats(&method, out) && f.attempt < cfg.max {
                if self.budget_of(f.route).take(now, &cfg) {
                    if let Some(be) = self.backends.get_mut(b) {
                        be.errors += 1;
                        if let Outcome::Status(st) = out {
                            be.by_class[((st / 100).clamp(1, 5) - 1) as usize] += 1;
                        }
                    }
                    f.attempt += 1;
                    f.tried.push(b);
                    let delay = cfg.delay(f.attempt, &mut self.rng);
                    self.retried += 1;
                    if os.recording() {
                        os.note(Note::Traffic(TrafficNote::Retry { backend: self.backends[b].name.clone(), attempt: f.attempt, ms: delay / MS }));
                    }
                    let sid = f.req.id;
                    self.wheel.add(now + delay, LTimer::Retry(sid));
                    self.later.insert(sid, f);
                    self.after_backend(os, b);
                    return;
                }
                self.budget_denied += 1;
                if os.recording() {
                    os.note(Note::Traffic(TrafficNote::BudgetDenied));
                }
            }
        }
        match d.result {
            Ok(resp) => {
                let st = resp.head.status;
                let class = ((st / 100).clamp(1, 5) - 1) as usize;
                if let Some(be) = self.backends.get_mut(b) {
                    be.by_class[class] += 1;
                    be.bytes_in += resp.body.len() as u64;
                    be.lat.record(now, now - f.sent);
                    if st >= 500 {
                        be.errors += 1;
                    }
                }
                let mut h = Headers::new();
                for (k, v) in &resp.head.headers.0 {
                    if !hop_by_hop(k) {
                        h.add(k, v);
                    }
                }
                if self.tag
                    && let Some(be) = self.backends.get(b)
                {
                    h.set("X-Served-By", &be.name);
                }
                if f.cookie
                    && let Some(StickyCfg::Cookie { name }) = &self.pools[f.pool].sticky
                {
                    h.add("Set-Cookie", &format!("{}={}; Path=/", name, self.backends[b].name));
                }
                if let Some(k) = f.cache_key.clone() {
                    self.cache_store(os, &k, st, &resp.head.headers, &resp.body, f.bg);
                    if !f.bg {
                        h.set("X-Cache", "MISS");
                    }
                }
                if !f.bg {
                    let body = if method == "HEAD" { Vec::new() } else { resp.body };
                    self.send_reply(os, f.req.id, f.req.at, Reply { status: st, headers: h, body, chunk: 0, close: false }, 0);
                }
            }
            Err(fail) => {
                if let Some(be) = self.backends.get_mut(b) {
                    be.errors += 1;
                }
                if self.retry_of(f.route).is_none() {
                    // the old immediate retries ("retries")
                    let idempotent = matches!(method.as_str(), "GET" | "HEAD");
                    let retry = matches!(fail, Fail::Connect(_) | Fail::NoRoute(_) | Fail::ConnectTimeout) || (idempotent && matches!(fail, Fail::Timeout { .. } | Fail::Reset(_) | Fail::Empty));
                    f.tried.push(b);
                    if retry && f.tried.len() <= self.retries as usize {
                        let p = f.pool;
                        if let Some(nb) = self.pick(p, f.key, &f.tried) {
                            self.retried += 1;
                            os.note(Note::LbRetry { backend: self.backends[b].name.clone() });
                            self.forward(os, f, nb);
                            self.after_backend(os, b);
                            return;
                        }
                    }
                }
                if f.bg {
                    if let Some(k) = &f.cache_key {
                        self.revalidating.remove(k);
                    }
                } else {
                    let status = if matches!(fail, Fail::Timeout { .. }) { 504 } else { 502 };
                    self.errors += 1;
                    let mut rep = Reply::text(status, &fail.gateway_body(status));
                    rep.headers.add("X-Jfn-Lb", "backend-failed");
                    self.send_reply(os, f.req.id, f.req.at, rep, 0);
                }
            }
        }
        self.after_backend(os, b);
    }

    /// backend b has one request less: drains, the queue of its pool
    fn after_backend(&mut self, os: &mut OsApi, b: usize) {
        self.drained(os, b);
        if let Some(p) = self.backends.get(b).map(|x| x.pool) {
            self.dequeue(os, p);
        }
    }

    /// keeps a backend's answer when it may
    fn cache_store(&mut self, os: &mut OsApi, key: &str, st: u16, h: &Headers, body: &[u8], bg: bool) {
        let now = os.now();
        let Some(cfg) = self.cache.clone() else {
            return;
        };
        if bg {
            self.revalidating.remove(key);
        }
        let age = max_age(h);
        let keep = matches!(st, 200 | 203 | 204 | 301 | 404 | 410) && age != Some(0);
        let ttl = age.unwrap_or(cfg.ttl);
        if keep && ttl > 0 {
            let c = Cached { status: st, headers: h.clone(), body: Vec::from(body), stored: now, expires: now + ttl };
            self.store.insert(String::from(key), c, body.len() as u64 + 64);
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::CacheStored { key: String::from(key), ms: ttl / MS }));
            }
        }
    }

    // ---------------------------------------------------------------- TCP

    fn accept(&mut self, os: &mut OsApi, listener: u32) {
        while let Some(c) = os.tcp_accept(listener) {
            let now = os.now();
            let peer = os.tcp_info(c).map(|r| r.remote.0).unwrap_or(IpAddr::V4(Ipv4::UNSPECIFIED));
            self.requests += 1;
            self.next_pipe += 1;
            let id = self.next_pipe;
            let key = match &self.pools[0].hash_key {
                HashKey::Client => hash64(&ip_bytes(peer)),
                _ => hash64(&id.to_be_bytes()),
            };
            self.socks.insert(c, (id, false));
            self.pipes.insert(id, Pipe { client: c, back: None, b: 0, key, tried: Vec::new(), connected: false, up: Vec::new(), down: Vec::new(), c_eof: false, b_eof: false, c_shut: false, b_shut: false, c_closed: false, b_closed: false, start: now, timer: None, counted: false });
            self.pipe_connect(os, id);
        }
    }

    /// connects a pipe to the next backend, or gives up
    fn pipe_connect(&mut self, os: &mut OsApi, id: u32) {
        let now = os.now();
        let Some(p) = self.pipes.get(&id) else {
            return;
        };
        let (key, tried, client) = (p.key, p.tried.clone(), p.client);
        let first = tried.is_empty();
        let pick = if first || tried.len() <= self.retries as usize { self.pick(0, key, &tried) } else { None };
        let Some(b) = pick else {
            if first {
                self.no_backend += 1;
                os.note(Note::LbNoBackend);
            }
            self.errors += 1;
            os.tcp_abort(client);
            self.socks.remove(&client);
            self.pipes.remove(&id);
            return;
        };
        let (addr, port, name) = (self.backends[b].addr, self.backends[b].port, self.backends[b].name.clone());
        if !first
            && let Some(prev) = tried.last()
        {
            os.note(Note::LbRetry { backend: self.backends[*prev].name.clone() });
            self.retried += 1;
        }
        os.note(Note::LbPick { backend: name, algo: self.pools[0].algo.name() });
        match os.tcp_connect(addr, port, SockOpts::default()) {
            Ok(s) => {
                let t = self.wheel.add(now + self.connect_timeout, LTimer::Connect(id));
                let be = &mut self.backends[b];
                be.active += 1;
                be.total += 1;
                if let Some(p) = self.pipes.get_mut(&id) {
                    p.back = Some(s);
                    p.b = b;
                    p.timer = Some(t);
                    p.counted = true;
                    p.start = now;
                }
                self.socks.insert(s, (id, true));
            }
            Err(_) => {
                self.backends[b].errors += 1;
                if let Some(p) = self.pipes.get_mut(&id) {
                    p.tried.push(b);
                }
                self.pipe_connect(os, id);
            }
        }
    }

    /// the backend connection of a pipe failed before it was set up: the next backend
    fn pipe_retry(&mut self, os: &mut OsApi, id: u32) {
        let Some(p) = self.pipes.get_mut(&id) else {
            return;
        };
        let b = p.b;
        if let Some(t) = p.timer.take() {
            self.wheel.cancel(t);
        }
        if let Some(s) = p.back.take() {
            self.socks.remove(&s);
            os.tcp_close(s);
        }
        p.tried.push(b);
        if p.counted {
            p.counted = false;
            if let Some(be) = self.backends.get_mut(b) {
                be.active = be.active.saturating_sub(1);
                be.errors += 1;
            }
        }
        self.pipe_connect(os, id);
        self.drained(os, b);
    }

    /// moves what can go: client bytes to the backend, backend bytes to the client, FINs
    fn pump(&mut self, os: &mut OsApi, id: u32) {
        let Some(p) = self.pipes.get_mut(&id) else {
            return;
        };
        let b = p.b;
        let (mut to_back, mut to_client) = (0u64, 0u64);
        if !p.c_closed && p.up.len() < PIPE_BUF {
            let d = os.tcp_recv(p.client, PIPE_BUF - p.up.len());
            p.up.extend_from_slice(&d);
        }
        if p.connected && !p.b_closed {
            if let Some(s) = p.back
                && p.down.len() < PIPE_BUF
            {
                let d = os.tcp_recv(s, PIPE_BUF - p.down.len());
                p.down.extend_from_slice(&d);
            }
            if let Some(s) = p.back
                && !p.up.is_empty()
            {
                let n = os.tcp_send(s, &p.up);
                p.up.drain(..n);
                to_back = n as u64;
            }
            if p.up.is_empty()
                && p.c_eof
                && !p.b_shut
                && let Some(s) = p.back
            {
                p.b_shut = true;
                os.tcp_shutdown(s);
            }
        }
        if !p.c_closed && !p.down.is_empty() {
            let n = os.tcp_send(p.client, &p.down);
            p.down.drain(..n);
            to_client = n as u64;
        }
        if p.down.is_empty() && p.b_eof && !p.c_shut && !p.c_closed {
            p.c_shut = true;
            os.tcp_shutdown(p.client);
        }
        if let Some(be) = self.backends.get_mut(b) {
            be.bytes_out += to_back;
            be.bytes_in += to_client;
        }
    }

    /// a socket of a pipe closed
    fn pipe_closed(&mut self, os: &mut OsApi, sock: u32, back: bool, end: TcpEnd) {
        let Some((id, b)) = self.pipes.iter().find(|(_, p)| if back { p.back == Some(sock) } else { p.client == sock }).map(|(i, p)| (*i, p.b)) else {
            self.socks.remove(&sock);
            os.tcp_close(sock);
            return;
        };
        let connected = self.pipes.get(&id).is_some_and(|p| p.connected);
        if back && !connected {
            // refused or unreachable: the next backend (pipe_retry closes the socket)
            self.pipe_retry(os, id);
            return;
        }
        let reset = !matches!(end, TcpEnd::Closed);
        if back
            && !reset
            && let Some(p) = self.pipes.get_mut(&id)
        {
            // an orderly end: what the backend sent last still goes to the client, then its
            // FIN; the pipe ends when the client's side closes
            let rest = os.tcp_recv(sock, usize::MAX);
            p.down.extend_from_slice(&rest);
            p.b_eof = true;
            p.b_closed = true;
            self.socks.remove(&sock);
            os.tcp_close(sock);
            let done = p.c_closed;
            self.pump(os, id);
            if done {
                self.pipe_end(os, id, b);
            }
            return;
        }
        self.socks.remove(&sock);
        os.tcp_close(sock);
        let mut finished = false;
        if let Some(p) = self.pipes.get_mut(&id) {
            if back {
                // reset: the client's connection is reset too
                p.b_closed = true;
                if !p.c_closed {
                    os.tcp_abort(p.client);
                    p.c_closed = true;
                    self.socks.remove(&p.client);
                }
            } else {
                p.c_closed = true;
                if let Some(s) = p.back
                    && !p.b_closed
                {
                    if reset || !p.up.is_empty() {
                        os.tcp_abort(s);
                    } else {
                        os.tcp_close(s);
                    }
                    p.b_closed = true;
                    self.socks.remove(&s);
                }
            }
            if p.c_closed && p.b_closed {
                finished = true;
            }
        }
        if finished {
            self.pipe_end(os, id, b);
        }
    }

    fn pipe_end(&mut self, os: &mut OsApi, id: u32, b: usize) {
        if let Some(p) = self.pipes.remove(&id) {
            if let Some(t) = p.timer {
                self.wheel.cancel(t);
            }
            if p.counted
                && let Some(be) = self.backends.get_mut(b)
            {
                be.active = be.active.saturating_sub(1);
            }
            self.responses += 1;
        }
        self.drained(os, b);
    }

    /// closes the pipes of a backend whose drain time ran out
    fn cut_pipes(&mut self, os: &mut OsApi, b: usize) {
        let ids: Vec<u32> = self.pipes.iter().filter(|(_, p)| p.b == b && p.back.is_some()).map(|(i, _)| *i).collect();
        for id in ids {
            if let Some(p) = self.pipes.get(&id) {
                if !p.c_closed {
                    os.tcp_abort(p.client);
                    self.socks.remove(&p.client);
                }
                if let Some(s) = p.back
                    && !p.b_closed
                {
                    os.tcp_abort(s);
                    self.socks.remove(&s);
                }
            }
            self.pipe_end(os, id, b);
        }
    }

    // ---------------------------------------------------------------- draining

    /// a draining backend with nothing left is gone (or disabled)
    fn drained(&mut self, os: &mut OsApi, b: usize) {
        let Some(be) = self.backends.get(b) else {
            return;
        };
        if let Admin::Draining { remove, .. } = be.admin
            && be.active == 0
        {
            self.finish_drain(os, b, remove);
        }
    }

    fn finish_drain(&mut self, os: &mut OsApi, b: usize, remove: bool) {
        let now = os.now();
        let Some(be) = self.backends.get_mut(b) else {
            return;
        };
        be.admin = if remove { Admin::Removed } else { Admin::Disabled };
        be.since = now;
        let name = be.name.clone();
        os.note(Note::LbDrained { backend: name.clone() });
        os.log(5, "lb", &format!("Backend {} ist leer gelaufen und {}", name, if remove { "entfernt" } else { "abgeschaltet" }));
        if remove {
            self.rering();
        }
    }

    /// changes the backends while it runs (Action::LbBackend); new ones go to the pool "default"
    pub fn change(&mut self, os: &mut OsApi, op: LbOp) -> Result<(), Msg> {
        self.change_in(os, op, 0)
    }

    fn change_in(&mut self, os: &mut OsApi, op: LbOp, pool: usize) -> Result<(), Msg> {
        let now = os.now();
        self.seen = now;
        let find = |s: &Lb, n: &str| s.backends.iter().position(|b| b.name.eq_ignore_ascii_case(n));
        match op {
            LbOp::Add { name, addr, port, weight } => {
                match find(self, &name) {
                    Some(i) if !self.backends[i].gone() => return Err(msg!("Das Backend {} gibt es schon.", name)),
                    Some(i) => {
                        // its old chain of checks ends here
                        if let Some(t) = self.backends[i].check_t {
                            self.wheel.cancel(t);
                        }
                        let mut b = Backend::new(&name, addr, port, weight, now);
                        b.downs = self.backends[i].downs;
                        b.pool = pool;
                        self.backends[i] = b;
                    }
                    None => {
                        let mut b = Backend::new(&name, addr, port, weight, now);
                        b.pool = pool;
                        self.backends.push(b);
                    }
                }
                self.rering();
                let i = find(self, &name).unwrap_or(0);
                if self.health.kind != CheckKind::None {
                    self.backends[i].check_t = Some(self.wheel.add(now, LTimer::Check(i)));
                }
                os.log(5, "lb", &format!("Backend {} ({}:{}) hinzugefügt", name, addr, port));
                if self.ext {
                    self.ramp(i, now);
                    self.dequeue(os, pool);
                }
            }
            LbOp::Remove { name } | LbOp::Drain { name } if find(self, &name).is_none_or(|i| self.backends[i].gone()) => {
                return Err(msg!("Das Backend {} gibt es nicht.", name));
            }
            LbOp::Remove { ref name } | LbOp::Drain { ref name } => {
                let remove = matches!(op, LbOp::Remove { .. });
                let i = find(self, name).unwrap_or(0);
                let be = &mut self.backends[i];
                if be.admin == Admin::Disabled && remove {
                    be.admin = Admin::Removed;
                    be.since = now;
                    self.rering();
                } else if be.admin != Admin::Disabled {
                    be.admin = Admin::Draining { since: now, remove };
                    be.since = now;
                    os.log(5, "lb", &format!("Backend {} leert sich", be.name));
                    self.wheel.add(now + self.drain, LTimer::Drain(i));
                    self.drained(os, i);
                }
            }
            LbOp::Enable { name } => {
                let i = find(self, &name).filter(|&i| !self.backends[i].gone()).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].admin = Admin::Active;
                self.backends[i].since = now;
                if self.ext {
                    self.ramp(i, now);
                    let p = self.backends[i].pool;
                    self.dequeue(os, p);
                }
            }
            LbOp::Weight { name, weight } => {
                let i = find(self, &name).filter(|&i| !self.backends[i].gone()).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].weight = weight.max(1);
            }
        }
        self.wheel.arm(os);
        Ok(())
    }

    /// changes the backends of a load balancer that does not run (they count from its start)
    pub fn change_stopped(&mut self, op: LbOp) -> Result<(), Msg> {
        self.change_stopped_in(op, 0)
    }

    fn change_stopped_in(&mut self, op: LbOp, pool: usize) -> Result<(), Msg> {
        let find = |s: &Lb, n: &str| s.backends.iter().position(|b| b.name.eq_ignore_ascii_case(n) && !b.gone());
        match op {
            LbOp::Add { name, addr, port, weight } => {
                if find(self, &name).is_some() {
                    return Err(msg!("Das Backend {} gibt es schon.", name));
                }
                let mut b = Backend::new(&name, addr, port, weight, 0);
                b.pool = pool;
                match self.backends.iter().position(|b| b.name.eq_ignore_ascii_case(&name)) {
                    Some(i) => self.backends[i] = b,
                    None => self.backends.push(b),
                }
            }
            LbOp::Remove { name } => {
                let i = find(self, &name).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].admin = Admin::Removed;
            }
            LbOp::Drain { name } => {
                let i = find(self, &name).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].admin = Admin::Disabled;
            }
            LbOp::Enable { name } => {
                let i = find(self, &name).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].admin = Admin::Active;
            }
            LbOp::Weight { name, weight } => {
                let i = find(self, &name).ok_or_else(|| msg!("Das Backend {} gibt es nicht.", name))?;
                self.backends[i].weight = weight.max(1);
            }
        }
        self.rering();
        Ok(())
    }

    /// the algorithm while it runs (Action::LbAlgorithm): every pool takes it
    pub fn set_algo(&mut self, algo: Algo) {
        for p in self.pools.iter_mut() {
            p.algo = algo;
        }
        for b in self.backends.iter_mut() {
            b.cw = 0;
        }
    }

    // ---------------------------------------------------------------- timers

    fn timers(&mut self, os: &mut OsApi) {
        self.wheel.fired();
        let now = os.now();
        for (id, t) in self.wheel.due(now) {
            match t {
                LTimer::Gate(id) => {
                    if let Some(r) = self.gate.take(id) {
                        self.pass(os, r);
                    }
                }
                LTimer::Check(i) => {
                    if self.backends.get(i).is_some_and(|b| !b.gone()) && self.health.kind != CheckKind::None {
                        self.start_check(os, i);
                        let t = self.wheel.add(now + self.health.interval, LTimer::Check(i));
                        self.backends[i].check_t = Some(t);
                    }
                }
                LTimer::CheckTimeout(s) => {
                    if let Some((i, _)) = self.tchecks.remove(&s) {
                        os.tcp_abort(s);
                        self.check_done(os, i, false);
                    }
                }
                LTimer::Connect(id) => {
                    let late = self.pipes.get(&id).is_some_and(|p| !p.connected && p.back.is_some());
                    if late {
                        if let Some(p) = self.pipes.get(&id)
                            && let Some(s) = p.back
                        {
                            os.tcp_abort(s);
                        }
                        self.pipe_retry(os, id);
                    }
                }
                LTimer::Drain(i) => {
                    if let Some(Admin::Draining { since, remove }) = self.backends.get(i).map(|b| b.admin)
                        && since + self.drain <= now
                    {
                        if self.mode == LbMode::Tcp {
                            self.cut_pipes(os, i);
                        }
                        if matches!(self.backends.get(i).map(|b| b.admin), Some(Admin::Draining { .. })) {
                            self.finish_drain(os, i, remove);
                        }
                    }
                }
                LTimer::Retry(sid) => {
                    if let Some(f) = self.later.remove(&sid) {
                        self.refresh(os);
                        self.dispatch(os, f);
                    }
                }
                LTimer::Wait(sid) => {
                    if let Some((f, _, _)) = self.held.remove(&sid) {
                        let p = f.pool;
                        self.pools[p].waiting.retain(|x| *x != sid);
                        self.pools[p].queue_timeouts += 1;
                        if os.recording() {
                            os.note(Note::Traffic(TrafficNote::QueueTimeout { pool: self.pools[p].name.clone() }));
                        }
                        self.give_up(os, f, 503, "503 Service Unavailable: waited too long for a backend\n");
                    }
                }
                LTimer::Out(o) => {
                    if let Some(out) = self.outbox.remove(&o) {
                        self.respond_now(os, out.sid, out.at, out.rep);
                    }
                }
                LTimer::Wake => {
                    if self.wake.is_some_and(|(_, w)| w == id) {
                        self.wake = None;
                    }
                    self.refresh(os);
                }
            }
        }
        self.wheel.arm(os);
    }

    /// TCP events of check sockets, pipes and the listener; true when it was one of them
    fn tcp_event(&mut self, os: &mut OsApi, ev: &SvcEvent) -> bool {
        match *ev {
            SvcEvent::TcpConnected { sock, .. } if self.tchecks.contains_key(&sock) => {
                if let Some((i, t)) = self.tchecks.remove(&sock) {
                    self.wheel.cancel(t);
                    os.tcp_abort(sock);
                    self.check_done(os, i, true);
                }
                true
            }
            SvcEvent::TcpClosed { sock, .. } if self.tchecks.contains_key(&sock) => {
                if let Some((i, t)) = self.tchecks.remove(&sock) {
                    self.wheel.cancel(t);
                    os.tcp_close(sock);
                    self.check_done(os, i, false);
                }
                true
            }
            SvcEvent::TcpAccept { listener, .. } if Some(listener) == self.listener => {
                self.accept(os, listener);
                true
            }
            SvcEvent::TcpConnected { sock, .. } if self.socks.contains_key(&sock) => {
                let now = os.now();
                if let Some(&(id, _)) = self.socks.get(&sock)
                    && let Some(p) = self.pipes.get_mut(&id)
                {
                    p.connected = true;
                    if let Some(t) = p.timer.take() {
                        self.wheel.cancel(t);
                    }
                    let (b, start) = (p.b, p.start);
                    if let Some(be) = self.backends.get_mut(b) {
                        be.lat.record(now, now - start);
                    }
                    self.lat.record(now, now - start);
                    self.pump(os, id);
                }
                true
            }
            SvcEvent::TcpData { sock, .. } | SvcEvent::TcpWritable { sock } if self.socks.contains_key(&sock) => {
                if let Some(&(id, _)) = self.socks.get(&sock) {
                    self.pump(os, id);
                }
                true
            }
            SvcEvent::TcpEof { sock, .. } if self.socks.contains_key(&sock) => {
                if let Some(&(id, back)) = self.socks.get(&sock) {
                    if let Some(p) = self.pipes.get_mut(&id) {
                        if back {
                            p.b_eof = true;
                        } else {
                            p.c_eof = true;
                        }
                    }
                    self.pump(os, id);
                }
                true
            }
            SvcEvent::TcpClosed { sock, end } if self.socks.contains_key(&sock) => {
                if let Some(&(_, back)) = self.socks.get(&sock) {
                    self.pipe_closed(os, sock, back, end);
                }
                true
            }
            _ => false,
        }
    }

    // ---------------------------------------------------------------- views

    /// what the lab shows
    pub fn view(&self, now: Ns) -> LbView {
        let shares: Vec<Vec<(u32, u32)>> = self.pools.iter().map(|p| p.ring.shares()).collect();
        LbView {
            mode: self.mode,
            port: self.port,
            algo: self.pools[0].algo,
            listening: if self.mode == LbMode::Http { self.serve.listening() } else { self.listener.is_some() },
            requests: self.requests,
            responses: self.responses,
            active: self.active(),
            errors: self.errors,
            no_backend: self.no_backend,
            retried: self.retried,
            by_class: self.by_class,
            p50: self.lat.p50(now),
            p95: self.lat.p95(now),
            p99: self.lat.p99(now),
            backends: self
                .backends
                .iter()
                .enumerate()
                .filter(|(_, b)| !b.gone())
                .map(|(i, b)| BackendView {
                    name: b.name.clone(),
                    addr: b.addr,
                    port: b.port,
                    weight: b.weight,
                    admin: b.admin,
                    up: b.up,
                    since: b.since,
                    last_check: b.last_check,
                    checks: b.checks,
                    check_fails: b.check_fails,
                    downs: b.downs,
                    active: b.active,
                    total: b.total,
                    errors: b.errors,
                    by_class: b.by_class,
                    bytes_in: b.bytes_in,
                    bytes_out: b.bytes_out,
                    p50: b.lat.p50(now),
                    p95: b.lat.p95(now),
                    p99: b.lat.p99(now),
                    share_ppm: shares.get(b.pool).and_then(|s| s.iter().find(|s| s.0 as usize == i)).map(|s| s.1).unwrap_or(0),
                    pool: self.pools.get(b.pool).map(|p| p.name.clone()).unwrap_or_default(),
                    breaker: b.breaker.state,
                    ejected_until: b.ejected_until,
                    ejections: b.ejections,
                    weight_pm: if self.pools.get(b.pool).is_some_and(|p| p.slow_start > 0) { self.weight_now(i, now) as u32 } else { b.weight * 1000 },
                })
                .collect(),
            ext: self.ext,
            proxy: self.proxy,
            tls: self.tls.is_some(),
            pools: self
                .pools
                .iter()
                .map(|p| PoolView {
                    name: p.name.clone(),
                    algo: p.algo,
                    max_conns: p.max_conns,
                    queue: p.queue,
                    waiting: p.waiting.len() as u32,
                    max_waiting: p.max_waiting,
                    queued: p.queued,
                    shed: p.shed,
                    queue_timeouts: p.queue_timeouts,
                    sticky_moved: p.sticky_moved,
                    fail_fast: p.fail_fast,
                    wait_p99: p.wait.p99(now),
                    policies: pool_words(p),
                })
                .collect(),
            routes: self.routes.iter().map(|r| RouteView { label: r.label(), pool: self.pools.get(r.pool).map(|p| p.name.clone()).unwrap_or_default(), requests: r.requests, limited: r.limiter.limited, cache: r.cache && self.cache.is_some(), rewrite: rewrite_text(r) }).collect(),
            limited: self.limited,
            budget_denied: self.budget_denied,
            ejections: self.ejections,
            cache_hits: self.cache_hits,
            cache_misses: self.cache_misses,
            cache_stale: self.cache_stale,
            cache_entries: self.store.len() as u64,
            bytes_saved: self.bytes_saved,
            handshakes: self.handshakes,
            resumed: self.resumed,
            tls_cpu: self.tls_cpu,
        }
    }

    /// requests (HTTP) or connections (TCP) in flight
    pub fn active(&self) -> u64 {
        if self.mode == LbMode::Http { (self.fwd.len() + self.held.len() + self.later.len()) as u64 } else { self.pipes.len() as u64 }
    }

    /// the cache's entries, the least recently used first (proxyctl cache)
    pub fn cached(&self) -> Vec<(String, Cached)> {
        self.store.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
    }

    /// hits per mille of the cached requests (proxy)
    pub fn hit_pm(&self) -> u64 {
        let all = self.cache_hits + self.cache_stale + self.cache_misses;
        if all == 0 { 0 } else { (self.cache_hits + self.cache_stale) * 1000 / all }
    }
}

/// the key of a rate limit for a request
fn limit_key(cfg: &LimitCfg, r: &ReqIn, route: &str) -> String {
    match &cfg.key {
        LimitKey::All => String::new(),
        LimitKey::Client => format!("{}", r.peer.0),
        LimitKey::Header(h) => String::from(r.head.header(h).unwrap_or("")),
        LimitKey::Route => String::from(route),
    }
}

/// a pool's policies in short (lbctl)
fn pool_words(p: &Pool) -> String {
    let mut w: Vec<String> = Vec::new();
    if let Some(q) = p.queue {
        w.push(format!("max_conns {} queue {}", p.max_conns, q));
    }
    match &p.sticky {
        Some(StickyCfg::Cookie { name }) => w.push(format!("sticky cookie {}", name)),
        Some(StickyCfg::Client { .. }) => w.push(String::from("sticky client")),
        None => {}
    }
    if p.slow_start > 0 {
        w.push(format!("slow_start {} ms", p.slow_start / MS));
    }
    if let Some(o) = &p.outlier {
        w.push(format!("outlier {}x", o.consecutive));
    }
    if let Some(b) = &p.breaker {
        w.push(format!("breaker {}x {} ms", b.failures, b.open / MS));
    }
    w.join(", ")
}

/// what a route rewrites, in short
fn rewrite_text(r: &Route) -> String {
    let mut w: Vec<String> = Vec::new();
    if !r.strip.is_empty() {
        w.push(format!("-{}", r.strip));
    }
    if !r.add.is_empty() {
        w.push(format!("+{}", r.add));
    }
    if !r.set_headers.is_empty() || !r.remove_headers.is_empty() {
        w.push(format!("headers {}", r.set_headers.len() + r.remove_headers.len()));
    }
    w.join(" ")
}

/// a backend as the lab shows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackendView {
    pub name: String,
    pub addr: IpAddr,
    pub port: u16,
    pub weight: u32,
    pub admin: Admin,
    pub up: bool,
    /// when its state last changed
    pub since: Ns,
    pub last_check: Option<(Ns, bool)>,
    pub checks: u64,
    pub check_fails: u64,
    pub downs: u32,
    pub active: u32,
    pub total: u64,
    pub errors: u64,
    pub by_class: [u64; 5],
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub p50: Option<Ns>,
    pub p95: Option<Ns>,
    pub p99: Option<Ns>,
    /// its part of the hash ring (parts per million)
    pub share_ppm: u32,
    /// SA6: its pool, breaker, ejection and the weight now (thousandths; slow start)
    pub pool: String,
    pub breaker: BState,
    pub ejected_until: Option<Ns>,
    pub ejections: u32,
    pub weight_pm: u32,
}

/// a pool as the lab shows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PoolView {
    pub name: String,
    pub algo: Algo,
    pub max_conns: u32,
    pub queue: Option<u32>,
    pub waiting: u32,
    pub max_waiting: u32,
    pub queued: u64,
    pub shed: u64,
    pub queue_timeouts: u64,
    pub sticky_moved: u64,
    pub fail_fast: u64,
    pub wait_p99: Option<Ns>,
    pub policies: String,
}

/// a route as the lab shows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouteView {
    pub label: String,
    pub pool: String,
    pub requests: u64,
    pub limited: u64,
    pub cache: bool,
    pub rewrite: String,
}

/// a load balancer as the lab shows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LbView {
    pub mode: LbMode,
    pub port: u16,
    pub algo: Algo,
    pub listening: bool,
    pub requests: u64,
    pub responses: u64,
    pub active: u64,
    pub errors: u64,
    pub no_backend: u64,
    pub retried: u64,
    pub by_class: [u64; 5],
    pub p50: Option<Ns>,
    pub p95: Option<Ns>,
    pub p99: Option<Ns>,
    pub backends: Vec<BackendView>,
    /// SA6
    pub ext: bool,
    pub proxy: bool,
    pub tls: bool,
    pub pools: Vec<PoolView>,
    pub routes: Vec<RouteView>,
    pub limited: u64,
    pub budget_denied: u64,
    pub ejections: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub cache_stale: u64,
    pub cache_entries: u64,
    pub bytes_saved: u64,
    pub handshakes: u64,
    pub resumed: u64,
    pub tls_cpu: Ns,
}

fn us(v: Option<u64>) -> i64 {
    v.map(|x| (x / 1000) as i64).unwrap_or(0)
}

impl Lb {
    /// its settings in the units of its config (svcform.rs)
    pub(crate) fn settings(&self) -> Value {
        use crate::svcform::{mem_into, ms_of, us_of, word};
        let mut o = agent_core::jmap::empty_object();
        if !self.proxy {
            o.insert("mode", word(if self.mode == LbMode::Http { "http" } else { "tcp" }));
        }
        o.insert("port", Value::u64(self.port as u64));
        o.insert("algorithm", word(self.pools[0].algo.name()));
        o.insert("vnodes", Value::u64(self.vnodes as u64));
        o.insert("retries", Value::u64(self.retries as u64));
        o.insert("timeout_ms", ms_of(self.timeout));
        o.insert("connect_timeout_ms", ms_of(self.connect_timeout));
        o.insert("drain_ms", ms_of(self.drain));
        o.insert("max_conns", Value::u64(self.max_conns as u64));
        o.insert("keepalive", Value::Bool(self.keepalive));
        o.insert("tag", Value::Bool(self.tag));
        o.insert("cpu_us", us_of(self.cpu));
        mem_into(&mut o, &self.mem);
        let h = &self.health;
        let kind = match h.kind {
            CheckKind::None => "none",
            CheckKind::Tcp => "tcp",
            CheckKind::Http => "http",
        };
        o.insert("health", Value::object([("type", word(kind)), ("path", word(&h.path)), ("interval_ms", ms_of(h.interval)), ("timeout_ms", ms_of(h.timeout)), ("rise", Value::u64(h.rise as u64)), ("fall", Value::u64(h.fall as u64))]));
        // SA6: the members of traffic management, as far as the form shows them
        // (a policy that is off has no member: its fields stay empty)
        let p = &self.pools[0];
        if let Some(q) = p.queue {
            o.insert("queue", Value::u64(q as u64));
        }
        if self.mode == LbMode::Http {
            o.insert("queue_timeout_ms", ms_of(p.queue_timeout));
            o.insert("slow_start_ms", ms_of(p.slow_start));
        }
        if let Some(s) = &p.sticky {
            o.insert("sticky", s.value());
        }
        if let Some(x) = &p.outlier {
            o.insert("outlier", x.value());
        }
        if let Some(x) = &p.breaker {
            o.insert("breaker", x.value());
        }
        if let Some(x) = &self.retry {
            o.insert("retry", x.value());
        }
        if let Some(x) = &self.limit {
            o.insert("limit", x.value());
        }
        if self.proxy {
            o.insert("handshake_us", us_of(self.tls_cost.handshake));
            o.insert("resume_us", us_of(self.tls_cost.resume));
            o.insert("tls_kb_us", us_of(self.tls_cost.per_kb));
            o.insert("session_reuse", Value::Bool(self.tls_cost.reuse));
            o.insert("session_ttl_ms", ms_of(self.tls_cost.session_ttl));
            o.insert("max_requests", Value::u64(self.serve.cfg.max_requests as u64));
            o.insert("keepalive_ms", ms_of(self.serve.cfg.keepalive));
            if let Some(c) = &self.cache {
                o.insert("cache", Value::object([("entries", Value::u64(c.entries as u64)), ("max_kb", Value::u64(c.max_kb)), ("ttl_ms", ms_of(c.ttl)), ("stale_ms", ms_of(c.stale)), ("hit_us", us_of(c.hit))]));
            }
        }
        o
    }
}

impl Service for Lb {
    fn kind(&self) -> &'static str {
        if self.proxy { "proxy" } else { "lb" }
    }
    fn start(&mut self, os: &mut OsApi) {
        let now = os.now();
        self.seen = now;
        self.serve.stop();
        self.agent.reset();
        self.checker.reset();
        self.wheel.clear();
        self.gate.clear();
        self.fwd.clear();
        self.by_agent.clear();
        self.hchecks.clear();
        self.tchecks.clear();
        self.pipes.clear();
        self.socks.clear();
        self.listener = None;
        self.agent.max_per_host = if self.pools[0].queue.is_some() { 0 } else { self.max_conns as usize };
        self.agent.idle_timeout = 60 * SEC;
        for b in self.backends.iter_mut() {
            b.active = 0;
            b.checking = false;
        }
        self.clear_ext();
        if self.retry_random() {
            self.rng = crate::httpsvc::seeded(os);
        }
        if let Some(spec) = self.tls.clone() {
            let id = spec.identity(os, &format!("proxy:{}", self.port));
            if let Some(c) = id.chain.first().and_then(|d| netpkt::x509::Cert::parse(d)) {
                os.note(Note::Tls(crate::tlssim::TlsNote::CertMade { subject: c.subject_cn, issuer: c.issuer_cn }));
            }
            self.serve.cfg.tls = Some(alloc::sync::Arc::new(id));
        }
        match self.mode {
            LbMode::Http => {
                self.serve.start(os, if self.proxy { "proxy" } else { "lb" });
            }
            LbMode::Tcp => match os.tcp_listen(self.port, 0, SockOpts::default()) {
                Ok(l) => self.listener = Some(l),
                Err(_) => os.note(Note::PortInUse { port: self.port, svc: String::from("lb") }),
            },
        }
        if self.health.kind != CheckKind::None {
            for i in 0..self.backends.len() {
                if !self.backends[i].gone() {
                    let t = self.wheel.add(now, LTimer::Check(i));
                    self.backends[i].check_t = Some(t);
                }
            }
        }
        self.wheel.arm(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        self.seen = os.now();
        if let SvcEvent::Timer(K_OWN) = ev {
            self.timers(os);
            return;
        }
        if self.mode == LbMode::Http && self.serve.on(&ev, os) {
            for r in self.serve.take() {
                self.arrive(os, r);
            }
        } else if self.mode == LbMode::Http && self.agent.on(&ev, os) {
            for d in self.agent.take_done() {
                self.answered(os, d);
            }
        } else if self.checker.on(&ev, os) {
            for d in self.checker.take_done() {
                if let Some(i) = self.hchecks.remove(&d.id) {
                    let ok = d.result.as_ref().is_ok_and(|r| (200..400).contains(&r.head.status));
                    self.check_done(os, i, ok);
                }
            }
        } else {
            self.tcp_event(os, &ev);
        }
        self.wheel.arm(os);
    }
    fn stop(&mut self, _os: &mut OsApi) {
        self.serve.stop();
        self.agent.reset();
        self.checker.reset();
        self.wheel.clear();
        self.gate.clear();
        self.fwd.clear();
        self.by_agent.clear();
        self.hchecks.clear();
        self.tchecks.clear();
        self.pipes.clear();
        self.socks.clear();
        self.listener = None;
        for b in self.backends.iter_mut() {
            b.active = 0;
            b.checking = false;
        }
        self.clear_ext();
    }
    fn reconfigure(&mut self, c: &Value, os: Option<&mut OsApi>) -> Result<(), Msg> {
        let fresh = if self.proxy { proxy_from_def_msg(c)? } else { from_def_msg(c)? };
        let Some(f) = fresh.as_any().downcast_ref::<Lb>() else {
            return Err(msg!("Der Dienst \u{201e}{}\u{201c} ist von anderer Art.", "lb"));
        };
        if self.mode != f.mode || self.port != f.port {
            return Err(msg!("Modus und Port eines Load Balancers ändern sich nur mit einem Neustart."));
        }
        if self.health.kind != f.health.kind {
            return Err(msg!("Die Art der Prüfungen ändert sich nur mit einem Neustart."));
        }
        if self.tls != f.tls {
            return Err(msg!("Das Zertifikat des Proxys ändert sich nur mit einem Neustart."));
        }
        if self.pools.len() != f.pools.len() || self.pools.iter().zip(f.pools.iter()).any(|(a, b)| a.name != b.name) || self.routes.len() != f.routes.len() || self.routes.iter().zip(f.routes.iter()).any(|(a, b)| !a.same_place(b)) {
            return Err(msg!("Pools und Routen ändern sich nur mit einem Neustart."));
        }
        if self.pools[0].queue.is_some() != f.pools[0].queue.is_some() && os.is_some() {
            return Err(msg!("Eine Warteschlange kommt oder geht nur mit einem Neustart."));
        }
        // what happens to the backends, seen before anything changes
        let mut ops = Vec::new();
        for b in &f.backends {
            match self.backends.iter().find(|x| !x.gone() && x.name.eq_ignore_ascii_case(&b.name)) {
                Some(x) if x.addr != b.addr || x.port != b.port => return Err(msg!("Das Backend {} hat eine andere Adresse: es braucht einen anderen Namen.", b.name.clone())),
                Some(x) if x.pool != b.pool => return Err(msg!("Das Backend {} gehört jetzt zu einem anderen Pool: es braucht einen anderen Namen.", b.name.clone())),
                Some(x) if x.weight != b.weight => ops.push((LbOp::Weight { name: b.name.clone(), weight: b.weight }, b.pool)),
                Some(_) => {}
                None => ops.push((LbOp::Add { name: b.name.clone(), addr: b.addr, port: b.port, weight: b.weight }, b.pool)),
            }
        }
        for x in self.backends.iter().filter(|x| !x.gone()) {
            if !f.backends.iter().any(|b| b.name.eq_ignore_ascii_case(&x.name)) {
                ops.push((LbOp::Remove { name: x.name.clone() }, x.pool));
            }
        }
        if self.pools[0].algo != f.pools[0].algo {
            self.set_algo(f.pools[0].algo);
        }
        for (p, q) in self.pools.iter_mut().zip(f.pools.iter()) {
            if !p.same_cfg(q) {
                p.algo = q.algo;
                p.hash_key = q.hash_key.clone();
                p.max_conns = q.max_conns;
                p.queue = q.queue;
                p.queue_timeout = q.queue_timeout;
                p.sticky = q.sticky.clone();
                p.slow_start = q.slow_start;
                p.outlier = q.outlier.clone();
                p.breaker = q.breaker.clone();
            }
        }
        for (r, q) in self.routes.iter_mut().zip(f.routes.iter()) {
            r.strip = q.strip.clone();
            r.add = q.add.clone();
            r.set_headers = q.set_headers.clone();
            r.remove_headers = q.remove_headers.clone();
            r.cache = q.cache;
            r.retry = q.retry.clone();
            r.limit = q.limit.clone();
        }
        let random_before = self.retry_random();
        self.retry = f.retry.clone();
        self.limit = f.limit.clone();
        self.tls_cost = f.tls_cost.clone();
        self.serve.cfg.max_requests = f.serve.cfg.max_requests;
        self.serve.cfg.keepalive = f.serve.cfg.keepalive;
        match (&self.cache, &f.cache) {
            (_, None) => {
                self.store.clear();
                self.cache = None;
            }
            (_, Some(c)) => {
                self.store.max_len = c.entries;
                self.store.max_weight = c.max_kb << 10;
                self.cache = Some(c.clone());
            }
        }
        let rering = self.vnodes != f.vnodes;
        self.vnodes = f.vnodes;
        self.health.path = f.health.path.clone();
        self.health.interval = f.health.interval;
        self.health.timeout = f.health.timeout;
        self.health.rise = f.health.rise;
        self.health.fall = f.health.fall;
        self.retries = f.retries;
        self.timeout = f.timeout;
        self.connect_timeout = f.connect_timeout;
        self.drain = f.drain;
        self.max_conns = f.max_conns;
        self.keepalive = f.keepalive;
        self.tag = f.tag;
        self.cpu = f.cpu;
        self.mem = f.mem;
        self.compute_ext();
        if rering {
            self.rering();
        }
        match os {
            Some(os) => {
                if !random_before && self.retry_random() {
                    self.rng = crate::httpsvc::seeded(os);
                }
                for (op, pool) in ops {
                    self.change_in(os, op, pool)?;
                }
            }
            None => {
                for (op, pool) in ops {
                    self.change_stopped_in(op, pool)?;
                }
            }
        }
        Ok(())
    }
    fn mem(&self) -> u64 {
        self.mem.bytes((self.serve.conns() + self.agent.conns() + 2 * self.pipes.len()) as u64, self.store.weight())
    }
    fn footprint(&self) -> usize {
        if !self.ext {
            return 1024;
        }
        1024 + self.store.weight() as usize + self.limiter.footprint() + self.routes.iter().map(|r| r.limiter.footprint()).sum::<usize>() + 64 * (self.held.len() + self.later.len() + self.sessions.len()) + self.serve.tls_bytes()
    }
    fn metrics(&self) -> Vec<Metric> {
        self.metrics_at(self.seen)
    }
    fn metrics_at(&self, now: Ns) -> Vec<Metric> {
        let v = |x: u64| x as i64;
        let live: Vec<&Backend> = self.backends.iter().filter(|b| !b.gone()).collect();
        let mut m = alloc::vec![
            Metric::new(if self.mode == LbMode::Http { "requests" } else { "connections" }, v(self.requests)),
            Metric::new("responses", v(self.responses)),
            Metric::new("active", v(self.active())),
            Metric::new("errors", v(self.errors)),
            Metric::new("no_backend", v(self.no_backend)),
            Metric::new("retries", v(self.retried)),
            Metric::new("status_2xx", v(self.by_class[1])),
            Metric::new("status_3xx", v(self.by_class[2])),
            Metric::new("status_4xx", v(self.by_class[3])),
            Metric::new("status_5xx", v(self.by_class[4])),
            Metric::new("backends", live.len() as i64),
            Metric::new("backends_up", live.iter().filter(|b| b.up && b.admin == Admin::Active).count() as i64),
            Metric::new("p50_us", us(self.lat.p50(now))),
            Metric::new("p95_us", us(self.lat.p95(now))),
            Metric::new("p99_us", us(self.lat.p99(now))),
        ];
        for (k, b) in self.backends.iter().take(8).enumerate() {
            m.push(Metric::new(M_REQ[k], v(b.total)));
            m.push(Metric::new(M_ACT[k], b.active as i64));
            m.push(Metric::new(M_UP[k], (b.up && !b.gone()) as i64));
            m.push(Metric::new(M_ERR[k], v(b.errors)));
        }
        if self.ext {
            let sum = |f: fn(&Pool) -> u64| self.pools.iter().map(f).sum::<u64>();
            let wait = self.pools.iter().filter_map(|p| p.wait.p99(now)).max();
            m.push(Metric::new("limited", v(self.limited + self.routes.iter().map(|r| r.limiter.limited).sum::<u64>())));
            m.push(Metric::new("queued", v(sum(|p| p.queued))));
            m.push(Metric::new("waiting", v(self.held.len() as u64)));
            m.push(Metric::new("shed", v(sum(|p| p.shed))));
            m.push(Metric::new("queue_timeouts", v(sum(|p| p.queue_timeouts))));
            m.push(Metric::new("budget_denied", v(self.budget_denied)));
            m.push(Metric::new("fail_fast", v(sum(|p| p.fail_fast))));
            m.push(Metric::new("breakers_open", live.iter().filter(|b| b.breaker.state != BState::Closed).count() as i64));
            m.push(Metric::new("ejected", live.iter().filter(|b| b.ejected_until.is_some()).count() as i64));
            m.push(Metric::new("ejections", v(self.ejections)));
            m.push(Metric::new("sticky_moved", v(sum(|p| p.sticky_moved))));
            m.push(Metric::new("wait_p99_us", us(wait)));
            for (k, b) in self.backends.iter().take(8).enumerate() {
                m.push(Metric::new(M_BRK[k], b.breaker.state.code()));
                m.push(Metric::new(M_EJ[k], b.ejected_until.is_some() as i64));
            }
        }
        if self.proxy {
            m.push(Metric::new("cache_hits", v(self.cache_hits)));
            m.push(Metric::new("cache_misses", v(self.cache_misses)));
            m.push(Metric::new("cache_stale", v(self.cache_stale)));
            m.push(Metric::new("hit_pm", v(self.hit_pm())));
            m.push(Metric::new("bytes_saved", v(self.bytes_saved)));
            m.push(Metric::new("tls_handshakes", v(self.handshakes)));
            m.push(Metric::new("tls_resumed", v(self.resumed)));
            m.push(Metric::new("tls_cpu_us", v(self.tls_cpu / US)));
        }
        m
    }
    fn metric_kind(&self, name: &str) -> crate::series::MetricKind {
        match name {
            "waiting" | "breakers_open" | "ejected" => crate::series::MetricKind::Gauge,
            n if n.starts_with('b') && (n.ends_with("_breaker") || n.ends_with("_ejected")) => crate::series::MetricKind::Gauge,
            n => crate::series::metric_kind_of(n),
        }
    }
    fn windows(&self) -> Vec<LatWindow<'_>> {
        alloc::vec![LatWindow::new(&self.lat, LAT)]
    }
    fn status(&self) -> String {
        self.status_msg().text()
    }
    fn status_msg(&self) -> Msg {
        let live: Vec<&Backend> = self.backends.iter().filter(|b| !b.gone()).collect();
        let up = live.iter().filter(|b| b.up && b.admin == Admin::Active).count();
        if self.proxy {
            let tls = if self.tls.is_some() { msg!("mit TLS") } else { msg!("ohne TLS") };
            msg!("Proxy Port {} {}, {} Routen: {} von {} Backends verfügbar, {}, {} Fehler", self.port, tls, self.routes.len(), up, live.len(), jfn_model::msg::count(self.requests, msg!("{} Anfrage", self.requests), msg!("{} Anfragen", self.requests)), self.errors)
        } else if self.mode == LbMode::Http {
            msg!("HTTP Port {}, {}: {} von {} Backends verfügbar, {}, {} Fehler", self.port, self.pools[0].algo.name(), up, live.len(), jfn_model::msg::count(self.requests, msg!("{} Anfrage", self.requests), msg!("{} Anfragen", self.requests)), self.errors)
        } else {
            msg!("TCP Port {}, {}: {} von {} Backends verfügbar, {}, {} Fehler", self.port, self.pools[0].algo.name(), up, live.len(), jfn_model::msg::count(self.requests, msg!("{} Verbindung", self.requests), msg!("{} Verbindungen", self.requests)), self.errors)
        }
    }
    fn clone_box(&self) -> Box<dyn Service> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
}

impl Lb {
    /// forgets the requests of the new paths (start, stop); the policies' state stays
    fn clear_ext(&mut self) {
        self.held.clear();
        self.later.clear();
        self.outbox.clear();
        self.revalidating.clear();
        self.tls_socks.clear();
        self.wake = None;
        for p in self.pools.iter_mut() {
            p.waiting.clear();
        }
        for b in self.backends.iter_mut() {
            b.breaker.probing = 0;
        }
    }
    /// a retry policy draws random numbers
    fn retry_random(&self) -> bool {
        self.retry.as_ref().is_some_and(|r| r.random()) || self.routes.iter().any(|r| r.retry.as_ref().is_some_and(|x| x.random()))
    }
}

// ---------------------------------------------------------------- project definitions

/// a load balancer from its definition in a project file (svcact.rs); the German reason when
/// the configuration does not fit
pub fn from_def(c: &Value) -> Result<Box<dyn Service>, String> {
    from_def_msg(c).map_err(|m| m.text())
}

/// from_def, the reason as a message for the app's catalogue
pub fn from_def_msg(c: &Value) -> Result<Box<dyn Service>, Msg> {
    Ok(Box::new(read(c, false)?))
}

/// a reverse proxy from its definition (kind "proxy")
pub fn proxy_from_def_msg(c: &Value) -> Result<Box<dyn Service>, Msg> {
    Ok(Box::new(read(c, true)?))
}

/// the members of SA6 that only a proxy takes
const PROXY_ONLY: [&str; 9] = ["tls", "handshake_us", "resume_us", "tls_kb_us", "session_reuse", "session_ttl_ms", "max_requests", "keepalive_ms", "cache"];
/// the members of SA6 that only HTTP takes
const HTTP_ONLY: [&str; 9] = ["queue", "queue_timeout_ms", "sticky", "slow_start_ms", "outlier", "breaker", "retry", "limit", "routes"];

/// a pool's algorithm, hash and policies from its config (the top level for "default")
fn pool_cfg(c: &Value, pl: &mut Pool, mode: LbMode, top: bool) -> Result<(), Msg> {
    let text = |k: &str| c.get(k).and_then(|v| v.as_str()).map(String::from);
    if let Some(a) = text("algorithm") {
        pl.algo = Algo::parse(&a).ok_or_else(|| msg!("\u{201e}algorithm\u{201c} kennt {} nicht (round-robin, least-conn, weighted, hash).", a))?;
    }
    if let Some(h) = text("hash") {
        pl.hash_key = match h.as_str() {
            "path" => HashKey::Path,
            "client" => HashKey::Client,
            x if x.starts_with("header:") => HashKey::Header(String::from(x[7..].trim())),
            x => return Err(msg!("\u{201e}hash\u{201c} kennt {} nicht (path, client, header:NAME).", x)),
        };
    }
    if mode == LbMode::Tcp {
        return Ok(());
    }
    let max_conns = c.get("max_conns").and_then(|v| v.as_u64()).unwrap_or(0).min(1_000_000) as u32;
    if !top {
        pl.max_conns = max_conns;
        if max_conns > 0 {
            pl.queue = Some(1000);
        }
    }
    if let Some(q) = c.get("queue") {
        let q = q.as_u64().ok_or_else(|| msg!("\u{201e}queue\u{201c} ist eine Zahl von Anfragen."))?;
        if max_conns == 0 {
            return Err(msg!("\u{201e}queue\u{201c} braucht \u{201e}max_conns\u{201c}: ohne Grenze wartet niemand."));
        }
        pl.max_conns = max_conns;
        pl.queue = Some(q.min(1_000_000) as u32);
    }
    pl.queue_timeout = c.get("queue_timeout_ms").and_then(|v| decimal(v, MS)).unwrap_or(SEC).max(MS);
    if let Some(s) = c.get("sticky") {
        pl.sticky = Some(StickyCfg::of(s)?);
    }
    pl.slow_start = c.get("slow_start_ms").and_then(|v| decimal(v, MS)).unwrap_or(0);
    if let Some(o) = c.get("outlier") {
        pl.outlier = Some(OutlierCfg::of(o)?);
    }
    if let Some(b) = c.get("breaker") {
        pl.breaker = Some(BreakerCfg::of(b)?);
    }
    Ok(())
}

/// the backends of a pool's config
fn backends_of(c: &Value, lb: &mut Lb, pool: usize, port: u16) -> Result<(), Msg> {
    let mode = lb.mode;
    for (k, b) in c.get("backends").and_then(|b| b.as_array()).unwrap_or(&[]).iter().enumerate() {
        let addr = b.get("addr").and_then(|v| v.as_str()).and_then(|s| IpAddr::parse(s.trim())).ok_or_else(|| msg!("Backend {} braucht \u{201e}addr\u{201c} mit einer IP-Adresse.", k + 1))?;
        let bport = b.get("port").and_then(|v| v.as_u64()).unwrap_or(if mode == LbMode::Http { 80 } else { port as u64 });
        if !(1..=65535).contains(&bport) {
            return Err(msg!("Port {} gibt es nicht.", bport));
        }
        let name = b.get("name").and_then(|v| v.as_str()).map(String::from).unwrap_or_else(|| if addr.is_v4() { format!("{}:{}", addr, bport) } else { format!("[{}]:{}", addr, bport) });
        if lb.backends.iter().any(|x| x.name.eq_ignore_ascii_case(&name)) {
            return Err(msg!("Das Backend {} steht zweimal da.", name));
        }
        let weight = b.get("weight").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 1000) as u32;
        lb.add_backend_to(&name, addr, bport as u16, weight, pool);
    }
    Ok(())
}

fn read(c: &Value, proxy: bool) -> Result<Lb, Msg> {
    let num = |k: &str, d: u64| c.get(k).and_then(|v| v.as_u64()).unwrap_or(d);
    let ms = |k: &str, d: u64| c.get(k).and_then(|v| decimal(v, MS)).unwrap_or(d * MS);
    let text = |k: &str| c.get(k).and_then(|v| v.as_str()).map(String::from);
    let mode = match text("mode").as_deref().unwrap_or("http") {
        "http" => LbMode::Http,
        "tcp" if !proxy => LbMode::Tcp,
        m if proxy => return Err(msg!("Ein Proxy spricht nur HTTP, nicht {}.", m)),
        m => return Err(msg!("\u{201e}mode\u{201c} kennt {} nicht (http, tcp).", m)),
    };
    if !proxy
        && let Some(k) = PROXY_ONLY.iter().find(|k| c.get(k).is_some())
    {
        return Err(msg!("\u{201e}{}\u{201c} gibt es nur beim Proxy (Dienstart proxy).", *k));
    }
    if mode == LbMode::Tcp
        && let Some(k) = HTTP_ONLY.iter().find(|k| c.get(k).is_some()).copied().or_else(|| if c.get("pools").is_some() { Some("pools") } else { None })
    {
        return Err(msg!("\u{201e}{}\u{201c} gibt es nur im HTTP-Modus.", k));
    }
    let default_port = if proxy && c.get("tls").is_some() { netpkt::tls::PORT as u64 } else { 80 };
    let port = match num("port", default_port) {
        p @ 1..=65535 => p as u16,
        p => return Err(msg!("Port {} gibt es nicht.", p)),
    };
    let mut lb = Lb::new(mode, port);
    lb.proxy = proxy;
    lb.vnodes = num("vnodes", 64).clamp(1, 1000) as u32;
    let mut p0 = Pool::new("default", mode);
    pool_cfg(c, &mut p0, mode, true)?;
    lb.pools[0] = p0;
    lb.retries = num("retries", 1).min(16) as u32;
    lb.timeout = ms("timeout_ms", 10_000);
    lb.connect_timeout = ms("connect_timeout_ms", 1000).max(MS);
    lb.drain = ms("drain_ms", 30_000);
    lb.max_conns = num("max_conns", 0).min(1_000_000) as u32;
    lb.cpu = c.get("cpu_us").and_then(|v| decimal(v, crate::kernel::US)).unwrap_or(Lb::CPU);
    lb.mem = crate::res::SvcMem::of(c, 16, 64);
    lb.keepalive = c.get("keepalive").and_then(|v| v.as_bool()).unwrap_or(true);
    lb.tag = c.get("tag").and_then(|v| v.as_bool()).unwrap_or(true);
    if let Some(h) = c.get("health") {
        let hs = |k: &str| h.get(k).and_then(|v| v.as_str()).map(String::from);
        let hm = |k: &str, d: Ns| h.get(k).and_then(|v| decimal(v, MS)).unwrap_or(d);
        if let Some(k) = hs("type") {
            lb.health.kind = match k.as_str() {
                "none" | "off" => CheckKind::None,
                "tcp" => CheckKind::Tcp,
                "http" => CheckKind::Http,
                x => return Err(msg!("\u{201e}health.type\u{201c} kennt {} nicht (tcp, http, none).", x)),
            };
        }
        if let Some(p) = hs("path") {
            lb.health.path = p;
        }
        lb.health.interval = hm("interval_ms", lb.health.interval).max(10 * MS);
        lb.health.timeout = hm("timeout_ms", lb.health.timeout).clamp(MS, lb.health.interval);
        lb.health.rise = h.get("rise").and_then(|v| v.as_u64()).unwrap_or(2).clamp(1, 100) as u32;
        lb.health.fall = h.get("fall").and_then(|v| v.as_u64()).unwrap_or(3).clamp(1, 100) as u32;
    }
    backends_of(c, &mut lb, 0, port)?;
    // SA6: more pools, routes, retries, limits
    for (k, pc) in c.get("pools").and_then(|p| p.as_array()).unwrap_or(&[]).iter().enumerate() {
        let name = pc.get("name").and_then(|v| v.as_str()).map(|s| String::from(s.trim())).filter(|s| !s.is_empty()).ok_or_else(|| msg!("Pool {} braucht \u{201e}name\u{201c}.", k + 1))?;
        if lb.pools.iter().any(|p| p.name.eq_ignore_ascii_case(&name)) {
            return Err(msg!("Den Pool {} gibt es zweimal.", name));
        }
        let mut pl = Pool::new(&name, mode);
        pool_cfg(pc, &mut pl, mode, false)?;
        lb.pools.push(pl);
        let i = lb.pools.len() - 1;
        backends_of(pc, &mut lb, i, port)?;
        if !lb.backends.iter().any(|b| b.pool == i) {
            return Err(msg!("Der Pool {} hat keine Backends.", name));
        }
    }
    if let Some(r) = c.get("retry") {
        lb.retry = Some(RetryCfg::of(r)?);
    }
    if let Some(l) = c.get("limit") {
        lb.limit = Some(LimitCfg::of(l)?);
    }
    for (k, rc) in c.get("routes").and_then(|r| r.as_array()).unwrap_or(&[]).iter().enumerate() {
        lb.routes.push(route_of(rc, k, &lb.pools, proxy)?);
    }
    if proxy {
        if let Some(t) = c.get("tls") {
            lb.tls = Some(crate::tlssim::TlsSpec::of(t)?);
        }
        lb.tls_cost = TlsCost::of(c);
        lb.serve.cfg.max_requests = num("max_requests", 0).min(u32::MAX as u64) as u32;
        lb.serve.cfg.keepalive = ms("keepalive_ms", 75_000);
        if let Some(cc) = c.get("cache") {
            if cc.as_object().is_none() {
                return Err(msg!("\u{201e}{}\u{201c} braucht ein Objekt.", "cache"));
            }
            let entries = cc.get("entries").and_then(|v| v.as_u64()).unwrap_or(1000).clamp(1, 1_000_000) as usize;
            let max_kb = cc.get("max_kb").and_then(|v| v.as_u64()).unwrap_or(65_536).clamp(1, 16 << 20);
            let ttl = cc.get("ttl_ms").and_then(|v| decimal(v, MS)).unwrap_or(30 * SEC);
            let stale = cc.get("stale_ms").and_then(|v| decimal(v, MS)).unwrap_or(0);
            let hit = cc.get("hit_us").and_then(|v| decimal(v, US)).unwrap_or(20 * US);
            lb.cache = Some(CacheCfg { entries, max_kb, ttl, stale, hit });
            lb.store = Lru::new(entries, max_kb << 10);
        }
        if lb.backends.is_empty() {
            return Err(msg!("\u{201e}backends\u{201c} oder \u{201e}pools\u{201c} fehlt."));
        }
    } else if lb.backends.is_empty() {
        return Err(msg!("\u{201e}backends\u{201c} fehlt."));
    }
    lb.compute_ext();
    Ok(lb)
}

/// a route from its config
fn route_of(rc: &Value, k: usize, pools: &[Pool], proxy: bool) -> Result<Route, Msg> {
    if rc.as_object().is_none() {
        return Err(msg!("Route {} braucht ein Objekt.", k + 1));
    }
    if !proxy
        && let Some(key) = ["strip", "add", "set_headers", "remove_headers", "cache"].iter().find(|x| rc.get(x).is_some())
    {
        return Err(msg!("Route {}: \u{201e}{}\u{201c} gibt es nur beim Proxy (Dienstart proxy).", k + 1, *key));
    }
    let s = |key: &str| rc.get(key).and_then(|v| v.as_str()).map(|x| String::from(x.trim())).unwrap_or_default();
    let prefix = {
        let p = s("path");
        if p.is_empty() { String::from("/") } else { p }
    };
    if !prefix.starts_with('/') {
        return Err(msg!("Route {}: der Pfad {} beginnt nicht mit /.", k + 1, prefix));
    }
    let pool_name = {
        let p = s("pool");
        if p.is_empty() { String::from("default") } else { p }
    };
    let pool = pools.iter().position(|p| p.name.eq_ignore_ascii_case(&pool_name)).ok_or_else(|| msg!("Route {}: den Pool {} gibt es nicht.", k + 1, pool_name.clone()))?;
    let add = s("add");
    if !add.is_empty() && !add.starts_with('/') {
        return Err(msg!("Route {}: \u{201e}add\u{201c} beginnt nicht mit /.", k + 1));
    }
    let mut set_headers = Vec::new();
    if let Some(o) = rc.get("set_headers").and_then(|v| v.as_object()) {
        for (name, v) in o {
            if hop_by_hop(name) || name.eq_ignore_ascii_case("host") {
                return Err(msg!("Route {}: den Header {} setzt der Proxy nicht.", k + 1, name.clone()));
            }
            set_headers.push((name.clone(), String::from(v.as_str().unwrap_or(""))));
        }
    }
    let remove_headers: Vec<String> = rc.get("remove_headers").and_then(|v| v.as_array()).unwrap_or(&[]).iter().filter_map(|v| v.as_str()).map(String::from).collect();
    Ok(Route {
        host: s("host"),
        prefix,
        pool,
        strip: s("strip"),
        add,
        set_headers,
        remove_headers,
        cache: rc.get("cache").and_then(|v| v.as_bool()).unwrap_or(true),
        retry: rc.get("retry").map(RetryCfg::of).transpose()?,
        limit: rc.get("limit").map(LimitCfg::of).transpose()?,
        limiter: Limiter::default(),
        budget: Budget::default(),
        requests: 0,
    })
}

impl Engine {
    /// a load balancer's (or proxy's) view for the lab
    pub fn lb_view(&self, dev: u32, svc: &str) -> Option<LbView> {
        Some(self.service::<Lb>(dev, svc)?.view(self.now))
    }
}

/// the bytes of an address for hashing (an IPv4 address: its four bytes, as before)
fn ip_bytes(a: IpAddr) -> Vec<u8> {
    match a {
        IpAddr::V4(x) => x.0.to_vec(),
        IpAddr::V6(x) => x.0.to_vec(),
    }
}
