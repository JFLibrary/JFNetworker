// dbpool -- the database pooler of the lab (SA6): clients send their statements to it over HTTP as to the
// database (db.rs: POST /query with the same bodies, the same answers); it holds at most `pool` connections to one
// db and lets the other statements wait in order.  It is no real product and speaks no real wire protocol
//
//   kind    config (JSON; every member but "db" may be left out)
//   dbpool  {"port": 6432, "db": {"addr": "10.0.2.30", "port": 7100}, "pool": 10, "mode": "transaction",
//            "queue": 1000, "queue_timeout_ms": 5000, "idle_txn_ms": 0, "timeout_ms": 30000,
//            "connect_timeout_ms": 3000, "cpu_us": 10, "mem_mb": 8, "conn_kb": 16}
//
// A slot is one of the `pool` connections.  "statement": every statement takes a slot for its round trip
// and gives it back with the answer; BEGIN and statements of a transaction are refused (400, code
// "pool_mode"), as such a pooler cannot keep a transaction on one connection.  "transaction": a statement
// without a transaction takes a slot for its round trip; BEGIN takes one for the whole transaction, which
// holds it until its COMMIT or ROLLBACK is answered (or the database says it does not know the
// transaction, 404), while it says nothing too: a client that leaves a transaction open holds a connection
// that nobody else can use.  "idle_txn_ms" > 0 ends such a transaction after that long without a statement
// (the pooler sends ROLLBACK, counted as idle_kills).  The transaction's id is what db's BEGIN answered;
// statements name it as for db ("txn" in the JSON, ?txn=, X-Txn).  Statements that find no free slot wait
// in order, at most `queue` of them (then 503, code "pool_full") and queue_timeout_ms long (503,
// "pool_timeout").  GET /_pool/stats answers the numbers as JSON; every other request goes to the database
// as it is, with a slot for its round trip.
//
// On a device with hardware every request costs cpu_us at a reference core (res.rs).  What the pooler
// holds: mem_mb, conn_kb per connection (clients' and the database's).  The connections to the database
// are kept alive, so db's max_conns and conn_kb see `pool` connections at most, whatever the clients do.
//
// Metrics: requests, statements, responses, errors, refused, queue_timeouts, mode_refused, waited, waiting,
// max_waiting, in_use, max_in_use, txns, txns_total, idle_kills, db_conns, wait_mean_us, wait_p99_us,
// p50_us, p95_us, p99_us (arrival to answer).  DbPool::view / Engine::dbpool_view for the lab and dbpoolctl;
// q_area (waiting requests integrated over time) and wait_sum for Little's law (tests).

use alloc::boxed::Box;
use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use agent_core::jmap::Value;
use base::metrics::Window;
use jfn_model::msg;
use jfn_model::msg::Msg;
use netpkt::http::{Headers, Version};
use netpkt::IpAddr;

use crate::http::{Agent, Done, Reply, Req, ReqIn, Serve, ServeCfg, Url, Wheel};
use crate::httpsvc::decimal;
use crate::kernel::{Engine, Ns, MS, SEC, US};
use crate::os::{Metric, OsApi, Service, SvcEvent};
use crate::series::{LatWindow, LAT};
use crate::trace::Note;
use crate::traffic::TrafficNote;

const K_OWN: u32 = 0;
const K_SERVE: u32 = 1;
const K_AGENT: u32 = 2;
/// the pooler's own port by default
pub const POOL_PORT: u16 = 6432;

/// how long a slot is held
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PoolMode {
    Statement,
    Transaction,
}

impl PoolMode {
    pub fn name(self) -> &'static str {
        match self {
            PoolMode::Statement => "statement",
            PoolMode::Transaction => "transaction",
        }
    }
}

/// what a request is to the slots
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// a round trip on a slot of its own
    Single,
    /// BEGIN: the slot stays with the transaction it opens
    Begin,
    /// a statement of a transaction that holds a slot (COMMIT / ROLLBACK end it)
    InTxn { txn: u64, ends: bool },
}

/// a request on its way to the database
#[derive(Clone, Debug)]
struct Out {
    req: ReqIn,
    kind: Kind,
}

/// a transaction that holds a slot
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TxnSlot {
    pub since: Ns,
    pub last: Ns,
    pub inflight: u32,
    timer: Option<u64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PTimer {
    Gate(u64),
    /// a waiting request waited too long
    Wait(u64),
    /// a transaction may have idled too long
    Idle(u64),
}

/// the pooler
#[derive(Clone, Debug)]
pub struct DbPool {
    pub port: u16,
    pub db: (IpAddr, u16),
    pub size: u32,
    pub mode: PoolMode,
    pub queue: u32,
    pub queue_timeout: Ns,
    pub idle_txn: Ns,
    pub timeout: Ns,
    pub connect_timeout: Ns,
    pub cpu: Ns,
    pub mem: crate::res::SvcMem,
    serve: Serve,
    agent: Agent,
    wheel: Wheel<PTimer>,
    gate: crate::res::CpuGate<ReqIn>,
    /// agent id -> what it carries
    out: BTreeMap<u64, Out>,
    /// the requests waiting for a slot: id, kind, since, its timer
    waiting: VecDeque<(ReqIn, Kind, Ns, u64)>,
    pub txns: BTreeMap<u64, TxnSlot>,
    /// slots of single round trips and BEGINs in flight
    busy: u32,
    pub requests: u64,
    pub statements: u64,
    pub responses: u64,
    pub errors: u64,
    pub refused: u64,
    pub queue_timeouts: u64,
    pub mode_refused: u64,
    pub waited: u64,
    pub max_waiting: u32,
    pub max_in_use: u32,
    pub txns_total: u64,
    pub idle_kills: u64,
    /// waiting requests integrated over time (count x ns) and since when it counts
    pub q_area: u128,
    q_at: Ns,
    pub wait_sum: Ns,
    wait: Window,
    lat: Window,
    seen: Ns,
}

impl DbPool {
    pub fn new(db: (IpAddr, u16)) -> DbPool {
        let cfg = ServeCfg { port: POOL_PORT, server: String::new(), max_requests: 0, max_body: 1 << 20, ..ServeCfg::default() };
        DbPool {
            port: POOL_PORT,
            db,
            size: 10,
            mode: PoolMode::Transaction,
            queue: 1000,
            queue_timeout: 5 * SEC,
            idle_txn: 0,
            timeout: 30 * SEC,
            connect_timeout: 3 * SEC,
            cpu: 10 * US,
            mem: crate::res::SvcMem::default(),
            serve: Serve::new(cfg, K_SERVE),
            agent: Agent::new(K_AGENT),
            wheel: Wheel::new(K_OWN),
            gate: crate::res::CpuGate::default(),
            out: BTreeMap::new(),
            waiting: VecDeque::new(),
            txns: BTreeMap::new(),
            busy: 0,
            requests: 0,
            statements: 0,
            responses: 0,
            errors: 0,
            refused: 0,
            queue_timeouts: 0,
            mode_refused: 0,
            waited: 0,
            max_waiting: 0,
            max_in_use: 0,
            txns_total: 0,
            idle_kills: 0,
            q_area: 0,
            q_at: 0,
            wait_sum: 0,
            wait: Window::new(300, SEC),
            lat: Window::new(300, SEC),
            seen: 0,
        }
    }

    /// slots in use now
    pub fn in_use(&self) -> u32 {
        self.busy + self.txns.len() as u32
    }
    /// requests waiting for a slot now
    pub fn waiting(&self) -> u32 {
        self.waiting.len() as u32
    }
    /// connections to the database now (kept alive)
    pub fn db_conns(&self) -> usize {
        self.agent.conns()
    }
    /// the time integral of the queue up to `now`
    fn area_to(&mut self, now: Ns) {
        self.q_area += self.waiting.len() as u128 * now.saturating_sub(self.q_at) as u128;
        self.q_at = now;
    }
    /// the mean queue length since the start (Little: L = lambda W)
    pub fn mean_waiting(&self, now: Ns) -> f64 {
        let area = self.q_area + self.waiting.len() as u128 * now.saturating_sub(self.q_at) as u128;
        if now == 0 { 0.0 } else { area as f64 / now as f64 }
    }

    fn arrive(&mut self, os: &mut OsApi, r: ReqIn) {
        self.requests += 1;
        let (now, at) = (os.now(), os.cpu(self.cpu));
        if at > now {
            let id = self.gate.hold(r);
            self.wheel.add(at, PTimer::Gate(id));
            return;
        }
        self.take(os, r);
    }

    /// what a request is, and a slot for it (or the queue)
    fn take(&mut self, os: &mut OsApi, r: ReqIn) {
        let now = os.now();
        if r.head.method == "GET" && r.head.path() == "/_pool/stats" {
            let body = self.stats_json(now);
            let mut rep = Reply::text(200, &body);
            rep.headers.set("Content-Type", "application/json");
            self.answer(os, r.id, r.at, rep);
            return;
        }
        let (word, txn) = statement_of(&r);
        let begins = word == "BEGIN" || word == "START";
        let ends = word == "COMMIT" || word == "ROLLBACK" || word == "END" || word == "ABORT";
        if self.mode == PoolMode::Statement && (begins || txn.is_some()) {
            self.mode_refused += 1;
            self.refuse(os, &r, 400, "pool_mode", "statement mode: a transaction needs one connection for all its statements, use transaction mode");
            return;
        }
        let kind = match txn {
            Some(t) if self.txns.contains_key(&t) => Kind::InTxn { txn: t, ends },
            _ if begins => Kind::Begin,
            _ => Kind::Single,
        };
        if let Kind::InTxn { txn, .. } = kind {
            self.send(os, r, kind);
            if let Some(t) = self.txns.get_mut(&txn) {
                t.last = now;
            }
            return;
        }
        if self.in_use() < self.size && self.waiting.is_empty() {
            self.send(os, r, kind);
            return;
        }
        if self.waiting.len() as u32 >= self.queue {
            self.refused += 1;
            self.refuse(os, &r, 503, "pool_full", "every connection of the pool is busy and the queue is full");
            return;
        }
        self.area_to(now);
        let t = self.wheel.add(now + self.queue_timeout, PTimer::Wait(r.id));
        self.waiting.push_back((r, kind, now, t));
        self.waited += 1;
        self.max_waiting = self.max_waiting.max(self.waiting.len() as u32);
        if os.recording() {
            os.note(Note::Traffic(TrafficNote::PoolWait { waiting: self.waiting.len() as u32 }));
        }
    }

    /// forwards a request on its slot
    fn send(&mut self, os: &mut OsApi, r: ReqIn, kind: Kind) {
        match kind {
            Kind::Single | Kind::Begin => self.busy += 1,
            Kind::InTxn { txn, .. } => {
                if let Some(t) = self.txns.get_mut(&txn) {
                    t.inflight += 1;
                }
            }
        }
        self.max_in_use = self.max_in_use.max(self.in_use());
        self.statements += 1;
        let mut h = Headers::new();
        for (k, v) in &r.head.headers.0 {
            if !["connection", "keep-alive", "transfer-encoding", "content-length", "te", "trailer", "upgrade", "host"].iter().any(|x| k.eq_ignore_ascii_case(x)) {
                h.add(k, v);
            }
        }
        let url = Url { host: format!("{}", self.db.0), port: self.db.1, target: r.head.target.clone() };
        let req = Req { method: r.head.method.clone(), url, version: Version::Http11, headers: h, body: r.body.clone(), timeout: self.timeout, connect_timeout: self.connect_timeout, reuse: true, keep_body: true };
        let id = self.agent.submit(os, req);
        self.out.insert(id, Out { req: r, kind });
    }

    fn answered(&mut self, os: &mut OsApi, d: Done) {
        let now = os.now();
        let Some(o) = self.out.remove(&d.id) else {
            return;
        };
        let status = d.result.as_ref().map(|r| r.head.status).unwrap_or(0);
        match o.kind {
            Kind::Single => self.busy = self.busy.saturating_sub(1),
            Kind::Begin => {
                self.busy = self.busy.saturating_sub(1);
                let txn = d.result.as_ref().ok().filter(|r| r.head.status == 200).and_then(|r| agent_core::json::parse(&r.body).ok()).and_then(|v| v.get("txn").and_then(|x| x.as_u64()));
                if let Some(t) = txn {
                    let timer = if self.idle_txn > 0 { Some(self.wheel.add(now + self.idle_txn, PTimer::Idle(t))) } else { None };
                    self.txns.insert(t, TxnSlot { since: now, last: now, inflight: 0, timer });
                    self.txns_total += 1;
                    if os.recording() {
                        os.note(Note::Traffic(TrafficNote::PoolTxn { txn: t, held: true }));
                    }
                }
            }
            Kind::InTxn { txn, ends } => {
                if let Some(t) = self.txns.get_mut(&txn) {
                    t.inflight = t.inflight.saturating_sub(1);
                    t.last = now;
                }
                // the transaction is over: its end was answered, or the database does not know it
                if ends || status == 404 || d.result.is_err() {
                    self.release(os, txn);
                }
            }
        }
        match d.result {
            Ok(resp) => {
                let mut h = Headers::new();
                for (k, v) in &resp.head.headers.0 {
                    if !["connection", "keep-alive", "transfer-encoding", "content-length", "te", "trailer", "upgrade"].iter().any(|x| k.eq_ignore_ascii_case(x)) {
                        h.add(k, v);
                    }
                }
                if resp.head.status >= 500 {
                    self.errors += 1;
                }
                self.answer(os, o.req.id, o.req.at, Reply { status: resp.head.status, headers: h, body: resp.body, chunk: 0, close: false });
            }
            Err(f) => {
                self.errors += 1;
                let st = if f.is_timeout() { 504 } else { 502 };
                let mut rep = Reply::text(st, &f.gateway_body(st));
                rep.headers.add("X-Jfn-Pool", "db-failed");
                self.answer(os, o.req.id, o.req.at, rep);
            }
        }
        self.next(os);
    }

    /// a transaction gives its slot back
    fn release(&mut self, os: &mut OsApi, txn: u64) {
        if let Some(t) = self.txns.remove(&txn) {
            if let Some(id) = t.timer {
                self.wheel.cancel(id);
            }
            if os.recording() {
                os.note(Note::Traffic(TrafficNote::PoolTxn { txn, held: false }));
            }
        }
    }

    /// waiting requests take the free slots
    fn next(&mut self, os: &mut OsApi) {
        let now = os.now();
        while self.in_use() < self.size && !self.waiting.is_empty() {
            self.area_to(now);
            let Some((r, kind, since, t)) = self.waiting.pop_front() else {
                break;
            };
            self.wheel.cancel(t);
            self.wait_sum += now - since;
            self.wait.record(now, now - since);
            self.send(os, r, kind);
        }
    }

    fn answer(&mut self, os: &mut OsApi, sid: u64, at: Ns, rep: Reply) {
        let now = os.now();
        if self.serve.respond(os, sid, rep) {
            self.responses += 1;
            self.lat.record(now, now - at);
        }
    }

    fn refuse(&mut self, os: &mut OsApi, r: &ReqIn, status: u16, code: &str, text: &str) {
        self.errors += 1;
        let body = format!("{{\"ok\": false, \"code\": \"{}\", \"error\": \"{}\", \"retryable\": {}}}\n", code, text, status == 503);
        let mut rep = Reply::text(status, &body);
        rep.headers.set("Content-Type", "application/json");
        if status == 503 {
            rep.headers.add("Retry-After", "1");
        }
        self.answer(os, r.id, r.at, rep);
    }

    fn timers(&mut self, os: &mut OsApi) {
        self.wheel.fired();
        let now = os.now();
        for (_, t) in self.wheel.due(now) {
            match t {
                PTimer::Gate(id) => {
                    if let Some(r) = self.gate.take(id) {
                        self.take(os, r);
                    }
                }
                PTimer::Wait(sid) => {
                    if let Some(i) = self.waiting.iter().position(|w| w.0.id == sid) {
                        self.area_to(now);
                        if let Some((r, _, since, _)) = self.waiting.remove(i) {
                            self.queue_timeouts += 1;
                            self.wait_sum += now - since;
                            self.refuse(os, &r, 503, "pool_timeout", "no connection of the pool became free in time");
                        }
                    }
                }
                PTimer::Idle(txn) => {
                    let Some(t) = self.txns.get_mut(&txn) else {
                        continue;
                    };
                    t.timer = None;
                    if t.inflight > 0 || now < t.last + self.idle_txn {
                        let at = t.last.max(now) + self.idle_txn;
                        t.timer = Some(self.wheel.add(at, PTimer::Idle(txn)));
                        continue;
                    }
                    // ends it at the database: the slot goes when ROLLBACK is sent
                    self.idle_kills += 1;
                    os.log(4, "dbpool", &format!("Transaktion {} war {} ms still und wird zurückgerollt", txn, self.idle_txn / MS));
                    let mut h = Headers::new();
                    h.add("X-Txn", &format!("{}", txn));
                    let url = Url { host: format!("{}", self.db.0), port: self.db.1, target: String::from("/query") };
                    let req = Req { method: String::from("POST"), url, version: Version::Http11, headers: h, body: Vec::from(&b"ROLLBACK"[..]), timeout: self.timeout, connect_timeout: self.connect_timeout, reuse: true, keep_body: false };
                    self.agent.submit(os, req);
                    self.release(os, txn);
                    self.next(os);
                }
            }
        }
        self.wheel.arm(os);
    }

    fn stats_json(&self, now: Ns) -> String {
        format!(
            "{{\"mode\": \"{}\", \"pool\": {}, \"in_use\": {}, \"waiting\": {}, \"txns\": {}, \"db_conns\": {}, \"requests\": {}, \"refused\": {}, \"queue_timeouts\": {}, \"idle_kills\": {}, \"wait_p99_us\": {}}}\n",
            self.mode.name(),
            self.size,
            self.in_use(),
            self.waiting(),
            self.txns.len(),
            self.db_conns(),
            self.requests,
            self.refused,
            self.queue_timeouts,
            self.idle_kills,
            self.wait.p99(now).unwrap_or(0) / US
        )
    }

    /// what the lab shows
    pub fn view(&self, now: Ns) -> DbPoolView {
        DbPoolView {
            port: self.port,
            listening: self.serve.listening(),
            db: self.db,
            mode: self.mode,
            size: self.size,
            in_use: self.in_use(),
            waiting: self.waiting(),
            txns: self.txns.iter().map(|(k, t)| (*k, now.saturating_sub(t.since), now.saturating_sub(t.last), t.inflight)).collect(),
            db_conns: self.db_conns() as u32,
            requests: self.requests,
            refused: self.refused,
            queue_timeouts: self.queue_timeouts,
            mode_refused: self.mode_refused,
            idle_kills: self.idle_kills,
            waited: self.waited,
            max_waiting: self.max_waiting,
            max_in_use: self.max_in_use,
            wait_p50: self.wait.p50(now),
            wait_p99: self.wait.p99(now),
            p99: self.lat.p99(now),
        }
    }

    /// its settings in the units of its config (svcform.rs)
    pub(crate) fn settings(&self) -> Value {
        use crate::svcform::{mem_into, ms_of, us_of, word};
        let mut o = agent_core::jmap::empty_object();
        o.insert("port", Value::u64(self.port as u64));
        o.insert("pool", Value::u64(self.size as u64));
        o.insert("mode", word(self.mode.name()));
        o.insert("queue", Value::u64(self.queue as u64));
        o.insert("queue_timeout_ms", ms_of(self.queue_timeout));
        o.insert("idle_txn_ms", ms_of(self.idle_txn));
        o.insert("timeout_ms", ms_of(self.timeout));
        o.insert("connect_timeout_ms", ms_of(self.connect_timeout));
        o.insert("cpu_us", us_of(self.cpu));
        mem_into(&mut o, &self.mem);
        o
    }
}

/// the first word of a request's statement (upper case) and the transaction it names
fn statement_of(r: &ReqIn) -> (String, Option<u64>) {
    let body = &r.body;
    let first = body.iter().copied().find(|b| !b.is_ascii_whitespace());
    let (sql, mut txn) = if first == Some(b'{') {
        match agent_core::json::parse(body) {
            Ok(v) => (String::from(v.get("sql").and_then(|x| x.as_str()).unwrap_or("")), v.get("txn").and_then(|x| x.as_u64())),
            Err(_) => (String::new(), None),
        }
    } else {
        (String::from(core::str::from_utf8(body).unwrap_or("")), None)
    };
    if txn.is_none() {
        txn = r.head.header("x-txn").and_then(|v| v.trim().parse().ok()).or_else(|| crate::kv::query_value(&r.head.target, "txn").and_then(|v| v.parse().ok()));
    }
    let word: String = sql.trim_start().chars().take_while(|c| c.is_ascii_alphabetic()).collect::<String>().to_ascii_uppercase();
    (word, txn)
}

/// a pooler as the lab shows it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbPoolView {
    pub port: u16,
    pub listening: bool,
    pub db: (IpAddr, u16),
    pub mode: PoolMode,
    pub size: u32,
    pub in_use: u32,
    pub waiting: u32,
    /// open transactions: id, how long open, how long silent, statements in flight
    pub txns: Vec<(u64, Ns, Ns, u32)>,
    pub db_conns: u32,
    pub requests: u64,
    pub refused: u64,
    pub queue_timeouts: u64,
    pub mode_refused: u64,
    pub idle_kills: u64,
    pub waited: u64,
    pub max_waiting: u32,
    pub max_in_use: u32,
    pub wait_p50: Option<Ns>,
    pub wait_p99: Option<Ns>,
    pub p99: Option<Ns>,
}

fn us(v: Option<u64>) -> i64 {
    v.map(|x| (x / 1000) as i64).unwrap_or(0)
}

impl Service for DbPool {
    fn kind(&self) -> &'static str {
        "dbpool"
    }
    fn start(&mut self, os: &mut OsApi) {
        let now = os.now();
        self.seen = now;
        self.serve.stop();
        self.agent.reset();
        self.wheel.clear();
        self.gate.clear();
        self.out.clear();
        self.waiting.clear();
        self.txns.clear();
        self.busy = 0;
        self.q_at = now;
        self.agent.max_per_host = self.size as usize;
        self.agent.idle_timeout = 600 * SEC;
        self.serve.start(os, "dbpool");
        self.wheel.arm(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        self.seen = os.now();
        if let SvcEvent::Timer(K_OWN) = ev {
            self.timers(os);
            return;
        }
        if self.serve.on(&ev, os) {
            for r in self.serve.take() {
                self.arrive(os, r);
            }
        } else if self.agent.on(&ev, os) {
            for d in self.agent.take_done() {
                self.answered(os, d);
            }
        }
        self.wheel.arm(os);
    }
    fn stop(&mut self, _os: &mut OsApi) {
        self.serve.stop();
        self.agent.reset();
        self.wheel.clear();
        self.gate.clear();
        self.out.clear();
        self.waiting.clear();
        self.txns.clear();
        self.busy = 0;
    }
    fn reconfigure(&mut self, c: &Value, os: Option<&mut OsApi>) -> Result<(), Msg> {
        let fresh = from_def_msg(c)?;
        let Some(f) = fresh.as_any().downcast_ref::<DbPool>() else {
            return Err(msg!("Der Dienst \u{201e}{}\u{201c} ist von anderer Art.", "dbpool"));
        };
        if self.port != f.port || self.db != f.db {
            return Err(msg!("Port und Datenbank des Poolers ändern sich nur mit einem Neustart."));
        }
        if self.mode != f.mode && !self.txns.is_empty() && os.is_some() {
            return Err(msg!("Der Modus des Poolers ändert sich nur ohne offene Transaktionen."));
        }
        self.size = f.size;
        self.mode = f.mode;
        self.queue = f.queue;
        self.queue_timeout = f.queue_timeout;
        self.idle_txn = f.idle_txn;
        self.timeout = f.timeout;
        self.connect_timeout = f.connect_timeout;
        self.cpu = f.cpu;
        self.mem = f.mem;
        self.agent.max_per_host = self.size as usize;
        if let Some(os) = os {
            // a new limit on silence counts for the open transactions too, from their last statement
            let now = os.now();
            let ids: Vec<u64> = self.txns.keys().copied().collect();
            for t in ids {
                if let Some(x) = self.txns.get_mut(&t) {
                    if let Some(id) = x.timer.take() {
                        self.wheel.cancel(id);
                    }
                    if self.idle_txn > 0 {
                        x.timer = Some(self.wheel.add((x.last + self.idle_txn).max(now), PTimer::Idle(t)));
                    }
                }
            }
            self.next(os);
            self.wheel.arm(os);
        }
        Ok(())
    }
    fn mem(&self) -> u64 {
        self.mem.bytes((self.serve.conns() + self.agent.conns()) as u64, 0)
    }
    fn metrics(&self) -> Vec<Metric> {
        self.metrics_at(self.seen)
    }
    fn metrics_at(&self, now: Ns) -> Vec<Metric> {
        let v = |x: u64| x as i64;
        let mean = self.wait_sum.checked_div(self.waited).unwrap_or(0);
        alloc::vec![
            Metric::new("requests", v(self.requests)),
            Metric::new("statements", v(self.statements)),
            Metric::new("responses", v(self.responses)),
            Metric::new("errors", v(self.errors)),
            Metric::new("refused", v(self.refused)),
            Metric::new("queue_timeouts", v(self.queue_timeouts)),
            Metric::new("mode_refused", v(self.mode_refused)),
            Metric::new("waited", v(self.waited)),
            Metric::new("waiting", self.waiting() as i64),
            Metric::new("max_waiting", self.max_waiting as i64),
            Metric::new("in_use", self.in_use() as i64),
            Metric::new("max_in_use", self.max_in_use as i64),
            Metric::new("txns", self.txns.len() as i64),
            Metric::new("txns_total", v(self.txns_total)),
            Metric::new("idle_kills", v(self.idle_kills)),
            Metric::new("db_conns", self.db_conns() as i64),
            Metric::new("wait_mean_us", (mean / US) as i64),
            Metric::new("wait_p99_us", us(self.wait.p99(now))),
            Metric::new("p50_us", us(self.lat.p50(now))),
            Metric::new("p95_us", us(self.lat.p95(now))),
            Metric::new("p99_us", us(self.lat.p99(now))),
        ]
    }
    fn metric_kind(&self, name: &str) -> crate::series::MetricKind {
        match name {
            "waiting" | "max_waiting" | "in_use" | "max_in_use" | "txns" | "db_conns" | "wait_mean_us" | "wait_p99_us" => crate::series::MetricKind::Gauge,
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
        msg!("DB-Pooler Port {} ({}): {} von {} Verbindungen belegt, {} warten, {} Transaktionen offen", self.port, self.mode.name(), self.in_use(), self.size, self.waiting(), self.txns.len())
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

/// a pooler from its definition (svcact.rs); the reason as a message for the app's catalogue
pub fn from_def_msg(c: &Value) -> Result<Box<dyn Service>, Msg> {
    let d = c.get("db").ok_or_else(|| msg!("\u{201e}db\u{201c} fehlt: der Pooler braucht die Adresse der Datenbank."))?;
    let addr = d.get("addr").and_then(|v| v.as_str()).and_then(|s| IpAddr::parse(s.trim())).ok_or_else(|| msg!("\u{201e}db.addr\u{201c} braucht eine IP-Adresse."))?;
    let dport = match d.get("port").and_then(|v| v.as_u64()).unwrap_or(crate::db::DB_PORT as u64) {
        p @ 1..=65535 => p as u16,
        p => return Err(msg!("Port {} gibt es nicht.", p)),
    };
    let mut p = DbPool::new((addr, dport));
    p.port = match c.get("port").and_then(|v| v.as_u64()).unwrap_or(POOL_PORT as u64) {
        x @ 1..=65535 => x as u16,
        x => return Err(msg!("Port {} gibt es nicht.", x)),
    };
    p.serve.cfg.port = p.port;
    let size = c.get("pool").and_then(|v| v.as_u64()).unwrap_or(10);
    if size == 0 {
        return Err(msg!("\u{201e}pool\u{201c} braucht mindestens eine Verbindung."));
    }
    p.size = size.min(10_000) as u32;
    p.mode = match c.get("mode").and_then(|v| v.as_str()).unwrap_or("transaction") {
        "statement" => PoolMode::Statement,
        "transaction" => PoolMode::Transaction,
        m => return Err(msg!("\u{201e}mode\u{201c} kennt {} nicht (statement, transaction).", m)),
    };
    let ms = |k: &str, d: Ns| c.get(k).and_then(|v| decimal(v, MS)).unwrap_or(d);
    p.queue = c.get("queue").and_then(|v| v.as_u64()).unwrap_or(1000).min(1_000_000) as u32;
    p.queue_timeout = ms("queue_timeout_ms", 5 * SEC).max(MS);
    p.idle_txn = ms("idle_txn_ms", 0);
    p.timeout = ms("timeout_ms", 30 * SEC);
    p.connect_timeout = ms("connect_timeout_ms", 3 * SEC).max(MS);
    p.cpu = c.get("cpu_us").and_then(|v| decimal(v, US)).unwrap_or(10 * US);
    p.mem = crate::res::SvcMem::of(c, 8, 16);
    Ok(Box::new(p))
}

impl Engine {
    /// a database pooler's view for the lab
    pub fn dbpool_view(&self, dev: u32, svc: &str) -> Option<DbPoolView> {
        Some(self.service::<DbPool>(dev, svc)?.view(self.now))
    }
}
