// repltest -- replication of the database (part of --selftest, group repl; jfn_sim's dbrepl.rs): the modes,
// the lag, failovers and fencing against closed forms, because learners take the figures for the truth; every
// refusal with its words; determinism; an old project that stays as it was
//
// Each comparison names its formula:
//
//   sync commit    latency = local flush + RTT + remote flush
//   semi commit    latency = local flush + RTT
//   async commit   latency = local flush
//   one client     throughput = 1 / latency (sync)
//   async lag      per commit = one-way delay + the replica's log write + apply (2 records: 2 apply_us + apply_row_us)
//   load           applied per second = min(lambda, 1 / a); the backlog grows by lambda - 1 / a per second
//   failover       lost = commits answered beyond what the replica received (async > 0, sync 0), counted alike
//   fencing        after the promotion the old primary takes no write; without fencing each side's count of
//                  commits since the fork is what it took
//   base copy      time = data / (bandwidth * 1460 / 1538): the modelled bulk at the rate the probe measured,
//                  TCP's payload of a frame on the wire
//
// Floating point only compares results here; nothing of it feeds the simulation.  "--selftest --only=repl
// --numbers" prints every comparison with its numbers.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use agent_core::jmap::Value;
use jfn_model::catalog::{DiskClass, Hw};
use jfn_model::project::{PortRef, Project, ServiceDef};
use jfn_sim::db::{Db, ReplCfg, ReplRole};
use jfn_sim::dbstore::Val;
use jfn_sim::http::{Agent, Req, Url};
use jfn_sim::keyframe::{digest, Budget};
use jfn_sim::os::{Metric, OsApi, Service, SvcEvent};
use jfn_sim::{Action, Cmd, Engine, Inline, Ns, Report, MS, SEC, US};
use netpkt::http::Headers;

use crate::selftest::T;

const BUDGET: usize = 400_000_000;
const GEN_IP: &str = "10.0.0.1";
const P_IP: &str = "10.0.0.2";
const R_IP: &str = "10.0.0.3";
const R2_IP: &str = "10.0.0.4";

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: String::from(port) }
}

fn cfg(text: &str) -> Value {
    agent_core::json::parse(text.as_bytes()).unwrap_or(Value::Null)
}

fn def(kind: &str, name: &str, config: &str) -> ServiceDef {
    ServiceDef { kind: String::from(kind), name: String::from(name), disabled: false, config: cfg(config) }
}

fn hw(cores: u32) -> Hw {
    Hw { cores, speed_pm: 1000, ram_mb: 8 << 10, swap_mb: 1 << 10, disk: DiskClass::Nvme, disk_gb: 400 }
}

fn check_near(t: &mut T, name: &str, got: f64, want: f64, tol: f64) {
    let line = format!("{}: {:.3} against {:.3} (within {:.1} %)", name, got, want, tol * 100.0);
    if plat::args().iter().any(|a| a == "--numbers") {
        plat::write_stdout(format!("{}, off by {:.2} %\n", line, (got - want).abs() / want.abs().max(1e-12) * 100.0).as_bytes());
    }
    t.check(&line, (got - want).abs() <= want.abs() * tol);
}

fn section(name: &str) {
    if plat::args().iter().any(|a| a == "--numbers") {
        plat::write_stdout(format!("-- {}\n", name).as_bytes());
    }
}

/// a client, a switch, a primary, a replica (and a second one): the replica's cable has `delay` one way
struct Net {
    p: Project,
    gen_: u32,
    pri: u32,
    rep: u32,
    rep2: u32,
    link_rep: u32,
}

/// a table for the tests: 1000 rows of 200 bytes, a number column
const TABLES: &str = r#"[{"name": "t", "rows": 1000, "row_bytes": 200, "columns": ["n"]}]"#;
/// a disk whose flush takes 1 ms and a little
const DISK: &str = r#"{"iops": 0, "mbps": 500, "latency_us": 1000}"#;

fn db_cfg(repl: &str, extra: &str) -> String {
    format!(r#"{{"tables": {}, "disk": {}, "replication": {}{}}}"#, TABLES, DISK, repl, extra)
}

#[allow(clippy::too_many_arguments)]
fn net(pri: &str, rep: &str, rep2: Option<&str>, hw_p: Option<Hw>, hw_r: Option<Hw>, delay: Ns, gen_svc: Option<ServiceDef>) -> Net {
    let mut p = Project::new("repl", 23);
    let g = p.add_device("traffic-gen", 0, 0).unwrap_or(0);
    let s = p.add_device("jf-s48x", 100, 0).unwrap_or(0);
    let a = p.add_device("server-1u", 200, 0).unwrap_or(0);
    let b = p.add_device("server-1u", 300, 0).unwrap_or(0);
    let c = p.add_device("server-1u", 400, 0).unwrap_or(0);
    let _ = p.connect(&pr(g, "eth0"), &pr(s, "TenGigabitEthernet1/0/1"), None);
    let _ = p.connect(&pr(a, "eth1"), &pr(s, "TenGigabitEthernet1/0/2"), None);
    let lr = p.connect(&pr(b, "eth1"), &pr(s, "TenGigabitEthernet1/0/3"), None).unwrap_or(0);
    let _ = p.connect(&pr(c, "eth1"), &pr(s, "TenGigabitEthernet1/0/4"), None);
    for l in p.links.iter_mut() {
        l.delay_ns = if l.id == lr { delay } else { 50 };
    }
    if let Some(d) = p.device_mut(g) {
        d.startup_config = format!("ip addr add {}/24 dev eth0\n", GEN_IP);
        if let Some(x) = gen_svc {
            d.services.push(x);
        }
    }
    for (dev, ip, conf, h) in [(a, P_IP, Some(pri), hw_p), (b, R_IP, Some(rep), hw_r.clone()), (c, R2_IP, rep2, hw_r)] {
        if let Some(d) = p.device_mut(dev) {
            d.startup_config = format!("ip addr add {}/24 dev eth1\n", ip);
            if let Some(conf) = conf {
                d.services = alloc::vec![def("db", "db", conf)];
            }
            d.hw = h;
        }
    }
    Net { p, gen_: g, pri: a, rep: b, rep2: c, link_rep: lr }
}

fn engine(p: &Project) -> Engine {
    Engine::from_project(p).unwrap_or_else(|_| Engine::new(0))
}

fn run_for(e: &mut Engine, d: Ns) {
    let t = e.now() + d;
    e.run_until(t, BUDGET);
    let _ = e.take_reports();
}

fn metric(e: &Engine, dev: u32, name: &str) -> i64 {
    e.metric(dev, "db", name).unwrap_or(-1)
}

fn db(e: &Engine, dev: u32) -> Option<&Db> {
    e.service::<Db>(dev, "db")
}

fn n_of(e: &Engine, dev: u32, key: u64) -> Option<i64> {
    match db(e, dev)?.row_now("t", key)?.get(1)? {
        Val::Int(n) => Some(*n),
        _ => None,
    }
}

// ---------------------------------------------------------------- clients

/// statements at given moments to given hosts; the answers in the order of the plan
#[derive(Clone, Debug)]
struct Driver {
    steps: Vec<(Ns, String, String)>,
    next: usize,
    agent: Agent,
    by_agent: BTreeMap<u64, usize>,
    got: BTreeMap<usize, (u16, String, Ns)>,
}

impl Driver {
    fn new(steps: &[(u64, &str, &str)]) -> Driver {
        Driver { steps: steps.iter().map(|(ms, h, b)| (*ms * MS, String::from(*h), String::from(*b))).collect(), next: 0, agent: Agent::new(1), by_agent: BTreeMap::new(), got: BTreeMap::new() }
    }
    fn send_due(&mut self, os: &mut OsApi) {
        let now = os.now();
        while let Some((at, host, body)) = self.steps.get(self.next).cloned() {
            if at > now {
                os.timer(0, at);
                break;
            }
            let i = self.next;
            self.next += 1;
            let mut r = if let Some(path) = body.strip_prefix("GET ") {
                Req::get(Url { host, port: 7100, target: String::from(path) })
            } else {
                let mut r = Req::get(Url { host, port: 7100, target: String::from("/query") });
                r.method = String::from("POST");
                r.body = body.into_bytes();
                r.headers = Headers::new();
                r.headers.add("Content-Type", "application/json");
                r
            };
            r.timeout = 600 * SEC;
            let a = self.agent.submit(os, r);
            self.by_agent.insert(a, i);
        }
    }
}

impl Service for Driver {
    fn kind(&self) -> &'static str {
        "test-driver"
    }
    fn start(&mut self, os: &mut OsApi) {
        self.send_due(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        if let SvcEvent::Timer(0) = ev {
            self.send_due(os);
            return;
        }
        if self.agent.on(&ev, os) {
            for d in self.agent.take_done() {
                let Some(i) = self.by_agent.remove(&d.id) else { continue };
                let (status, body) = match d.result {
                    Ok(r) => (r.head.status, String::from_utf8_lossy(&r.body).into_owned()),
                    Err(f) => (0, f.text()),
                };
                self.got.insert(i, (status, body, os.now()));
            }
        }
    }
    fn metrics(&self) -> Vec<Metric> {
        Vec::new()
    }
    fn status(&self) -> String {
        String::new()
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

fn got(e: &Engine, dev: u32, i: usize) -> (u16, String) {
    e.service::<Driver>(dev, "drv").and_then(|d| d.got.get(&i)).map_or((0, String::new()), |g| (g.0, g.1.clone()))
}

fn sql(s: &str) -> String {
    let mut b = String::from("{\"sql\":");
    Val::Text(String::from(s)).json(&mut b);
    b.push('}');
    b
}

/// one client in a closed loop: INSERTs with keys from 2000 on to one host, the answers kept (its latency per
/// commit, the keys answered 200)
#[derive(Clone, Debug)]
struct Writer {
    host: String,
    agent: Agent,
    key: u64,
    until: Ns,
    sent_at: Ns,
    ok: Vec<u64>,
    lat: Vec<Ns>,
    statuses: BTreeMap<u16, u64>,
    pending: Option<(u64, u64)>,
}

impl Writer {
    fn new(host: &str, until: Ns) -> Writer {
        Writer { host: String::from(host), agent: Agent::new(1), key: 2000, until, sent_at: 0, ok: Vec::new(), lat: Vec::new(), statuses: BTreeMap::new(), pending: None }
    }
    fn send(&mut self, os: &mut OsApi) {
        if os.now() >= self.until {
            return;
        }
        self.key += 1;
        let mut r = Req::get(Url { host: self.host.clone(), port: 7100, target: String::from("/query") });
        r.method = String::from("POST");
        r.body = sql(&format!("INSERT INTO t (id, n) VALUES ({}, 1)", self.key)).into_bytes();
        r.timeout = 600 * SEC;
        self.sent_at = os.now();
        let id = self.agent.submit(os, r);
        self.pending = Some((id, self.key));
    }
}

impl Service for Writer {
    fn kind(&self) -> &'static str {
        "test-writer"
    }
    fn start(&mut self, os: &mut OsApi) {
        self.send(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        if self.agent.on(&ev, os) {
            for d in self.agent.take_done() {
                let Some((id, key)) = self.pending else { continue };
                if id != d.id {
                    continue;
                }
                self.pending = None;
                let status = d.result.as_ref().map_or(0, |r| r.head.status);
                *self.statuses.entry(status).or_insert(0) += 1;
                if status == 200 {
                    self.ok.push(key);
                    self.lat.push(os.now() - self.sent_at);
                }
                self.send(os);
            }
        }
    }
    fn metrics(&self) -> Vec<Metric> {
        Vec::new()
    }
    fn status(&self) -> String {
        String::new()
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

fn writer(e: &Engine, dev: u32, name: &str) -> Option<Writer> {
    e.service::<Writer>(dev, name).cloned()
}

fn median(v: &[Ns]) -> f64 {
    let mut s = v.to_vec();
    s.sort_unstable();
    s.get(s.len() / 2).copied().unwrap_or(0) as f64
}

/// an open load of single-row UPDATEs at `rps` to the primary (drawn keys)
fn open_updates(rps: u64) -> ServiceDef {
    let body = r#"{"sql":"UPDATE t SET n = n + 1 WHERE id = ?","params":[{"draw":true}]}"#;
    let mix = Value::object([("method", Value::string("POST")), ("path", Value::string("/query")), ("weight", Value::u64(1)), ("body", Value::string(body))]);
    let o = Value::object([("targets", Value::Array(alloc::vec![Value::string(format!("http://{}:7100/", P_IP))])), ("mode", Value::string("open")), ("rps", Value::u64(rps)), ("timeout_ms", Value::u64(600_000)), ("max_conns", Value::u64(64)), ("mix", Value::Array(alloc::vec![mix]))]);
    ServiceDef { kind: String::from("http-load"), name: String::from("load"), disabled: false, config: o }
}

pub(crate) fn run(t: &mut T) {
    let parts: [(&str, fn(&mut T)); 18] = [
        ("config", config),
        ("ships", ships),
        ("commit_latency", commit_latency),
        ("one_client", one_client),
        ("async_lag", async_lag),
        ("lag_under_load", lag_under_load),
        ("failover", failover),
        ("switchover", switchover),
        ("partition", partition),
        ("stale_reads", stale_reads),
        ("read_balancer", read_balancer),
        ("base_copy", base_copy),
        ("gone_replica", gone_replica),
        ("determinism", determinism),
        ("old_project", old_project),
        ("tools", tools),
        ("auto_failover", auto_failover),
        ("form", form),
    ];
    for (name, f) in parts {
        section(name);
        let from = plat::time_us();
        f(t);
        if plat::args().iter().any(|a| a == "--numbers") {
            plat::write_stdout(format!("-- {}: {} ms\n", name, (plat::time_us() - from) / 1000).as_bytes());
        }
    }
}

// ---------------------------------------------------------------- the config

fn refusal(text: &str) -> String {
    match jfn_sim::db::from_def_msg(&cfg(text)) {
        Ok(_) => String::from("taken"),
        Err(m) => m.text(),
    }
}

fn config(t: &mut T) {
    let tb = format!(r#""tables": {}"#, TABLES);
    t.eq("a replica without its primary", refusal(&format!(r#"{{{}, "replication": {{"role": "replica"}}}}"#, tb)), String::from("Ein Replikat braucht \u{201e}replication.primary\u{201c}: die Adresse seines Primary."));
    t.eq("an address that is none", refusal(&format!(r#"{{{}, "replication": {{"primary": "10.0.2"}}}}"#, tb)), String::from("\u{201e}replication.primary\u{201c}: \u{201e}10.0.2\u{201c} ist keine IPv4-Adresse."));
    t.eq("a mode it does not know", refusal(&format!(r#"{{{}, "replication": {{"mode": "quorum"}}}}"#, tb)), String::from("\u{201e}replication.mode\u{201c} kennt quorum nicht (async, semi, sync, apply)."));
    t.eq("a role it does not know", refusal(&format!(r#"{{{}, "replication": {{"role": "leader"}}}}"#, tb)), String::from("\u{201e}replication.role\u{201c} kennt leader nicht (primary, replica)."));
    t.eq("the log on the database's port", refusal(&format!(r#"{{{}, "replication": {{"port": 7100}}}}"#, tb)), String::from("Das Log braucht einen eigenen Port, nicht 7100."));
    t.eq("no object", refusal(&format!(r#"{{{}, "replication": "sync"}}"#, tb)), String::from("\u{201e}replication\u{201c} ist ein Objekt."));
    t.eq("a primary still needs tables", refusal(r#"{"replication": {"mode": "sync"}}"#), String::from("\u{201e}tables\u{201c} fehlt: eine Datenbank braucht mindestens eine Tabelle."));
    t.eq("a replica may leave them to its copy", refusal(r#"{"replication": {"primary": "10.0.0.2"}}"#), String::from("taken"));
    // the defaults, as the README names them
    let c = ReplCfg::of(&cfg(r#"{"primary": "10.0.0.2"}"#), 7100).ok();
    t.eq("defaults", c.as_ref().map(|c| (c.role, c.port, c.sync_count, c.sync_timeout, c.apply_cpu, c.apply_row_cpu, c.heartbeat, c.fencing, c.failover)), Some((ReplRole::Replica, 7101, 1, 10 * SEC, 2 * US, 10 * US, 500 * MS, true, 0)));
    // its settings written back read the same
    let full = r#"{"role": "primary", "port": 7201, "mode": "sync", "sync_count": 2, "sync_timeout_ms": 2500, "apply_us": 3, "apply_row_us": 12, "heartbeat_ms": 200, "timeout_ms": 1000, "retry_ms": 300, "fencing": false, "fence_wait_ms": 500, "failover_ms": 4000}"#;
    let a = ReplCfg::of(&cfg(full), 7100).ok();
    let b = a.as_ref().and_then(|x| ReplCfg::of(&x.value(), 7100).ok());
    t.eq("its settings read back the same", b, a);
    // a database without replication says nothing of it
    let plain = jfn_sim::db::from_def_msg(&cfg(&format!("{{{}}}", tb))).ok();
    t.check("without it: no replication at all", plain.as_ref().and_then(|s| s.as_any().downcast_ref::<Db>()).is_some_and(|d| d.repl.is_none()));
}

// ---------------------------------------------------------------- the log over the network

fn ships(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "async"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 50, None);
    let mut e = engine(&n.p);
    let steps = [
        (500, P_IP, sql("UPDATE t SET n = 4711 WHERE id = 7")),
        (600, R_IP, sql("SELECT n FROM t WHERE id = 7")),
        (650, R_IP, sql("UPDATE t SET n = 1 WHERE id = 7")),
        (700, R_IP, String::from("GET /_db/health")),
        (710, R_IP, String::from("GET /_db/health/replica?max_lag_ms=100")),
        (720, P_IP, String::from("GET /_db/health")),
        (730, P_IP, String::from("GET /_db/health/replica")),
        (740, R_IP, sql("BEGIN")),
    ];
    let list: Vec<(u64, &str, &str)> = steps.iter().map(|(a, b, c)| (*a, *b, c.as_str())).collect();
    let _ = e.add_service(n.gen_, "drv", Box::new(Driver::new(&list)), true);
    run_for(&mut e, SEC);
    let (s0, b0) = got(&e, n.gen_, 0);
    t.check(&format!("the write on the primary: {} {}", s0, b0), s0 == 200 && b0.contains("\"lsn\":"));
    let (s1, b1) = got(&e, n.gen_, 1);
    t.check(&format!("the replica has it: {}", b1), s1 == 200 && b1.contains("\"n\":4711"));
    let (s2, b2) = got(&e, n.gen_, 2);
    t.eq("a write on the replica", (s2, b2.contains("\"code\":\"read_only\""), b2.contains("Ein Replikat nimmt nur Lesezugriffe an. Schreibzugriffe gehen an den Primary 10.0.0.2.")), (503, true, true));
    let (_, b3) = got(&e, n.gen_, 3);
    t.check(&format!("its health: {}", b3), b3.contains("\"role\":\"replica\"") && b3.contains("\"epoch\":1") && b3.contains("\"connected\":true") && b3.contains("\"primary\":\"10.0.0.2\""));
    t.eq("fresh enough", got(&e, n.gen_, 4).0, 200);
    let (_, b5) = got(&e, n.gen_, 5);
    t.check(&format!("the primary's health names its replica: {}", b5), b5.contains("\"role\":\"primary\"") && b5.contains("\"addr\":\"10.0.0.3\"") && b5.contains("\"mode\":\"async\""));
    t.eq("a primary is no replica", got(&e, n.gen_, 6).0, 503);
    t.eq("a replica takes read transactions", got(&e, n.gen_, 7).0, 200);
    t.eq("the row on the replica", n_of(&e, n.rep, 7), Some(4711));
    // the bytes of a record on the wire follow the log: a row of 200 bytes and its commit
    let sent = db(&e, n.pri).and_then(|d| d.repl.as_ref()).map_or(0, |r| r.stats.bytes_out);
    t.check(&format!("bytes on the wire at least the log's: {}", sent), sent >= 264 + 64);
    t.eq("one replica", metric(&e, n.pri, "replicas"), 1);
}

// ---------------------------------------------------------------- the modes

/// the median commit latency of one client writing for 3 s with this mode, the replica's cable 5 ms one way
fn latency_of(mode: &str) -> f64 {
    let pri = db_cfg(&format!(r#"{{"mode": "{}"}}"#, mode), "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 5 * MS, None);
    let mut e = engine(&n.p);
    let _ = e.add_service(n.gen_, "w", Box::new(Writer::new(P_IP, 4 * SEC)), true);
    run_for(&mut e, 4 * SEC);
    let w = writer(&e, n.gen_, "w");
    // (the first second warms up: connections, ARP)
    let lat: Vec<Ns> = w.map_or(Vec::new(), |w| w.lat.iter().skip(w.lat.len() / 4).copied().collect());
    median(&lat) / MS as f64
}

fn commit_latency(t: &mut T) {
    // the disk: 1 ms latency + 328 bytes at 500 MB/s; the replica's cable: 5 ms each way; the client's and the
    // primary's cables 50 ns
    let flush = 1.0 + (264.0 + 64.0) / 500e6 * 1000.0;
    let rtt = 10.0;
    check_near(t, "async: local flush (ms)", latency_of("async"), flush, 0.03);
    check_near(t, "semi: local flush + RTT (ms)", latency_of("semi"), flush + rtt, 0.03);
    check_near(t, "sync: local flush + RTT + remote flush (ms)", latency_of("sync"), flush + rtt + flush, 0.03);
}

fn one_client(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "sync"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 5 * MS, None);
    let mut e = engine(&n.p);
    let _ = e.add_service(n.gen_, "w", Box::new(Writer::new(P_IP, 20 * SEC)), true);
    run_for(&mut e, 2 * SEC);
    let a = writer(&e, n.gen_, "w").map_or(0, |w| w.ok.len());
    run_for(&mut e, 5 * SEC);
    let b = writer(&e, n.gen_, "w").map_or(0, |w| w.ok.len());
    let x = (b - a) as f64 / 5.0;
    let lat = 2.0 * (1.0 + 328.0 / 500e6 * 1000.0) + 10.0;
    check_near(t, "one client, sync: 1 / latency (commits per second)", x, 1000.0 / lat, 0.03);
    t.eq("every commit is on the replica", metric(&e, n.rep, "applied_commits") >= b as i64 - 2, true);
}

fn async_lag(t: &mut T) {
    // the replica pays its CPU: 2 records of 2 us and a row of 10 us = 14 us; the write of its log 1 ms + 328 B
    let pri = db_cfg(r#"{"mode": "async"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}", "apply_us": 2, "apply_row_us": 10}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, Some(hw(1)), 5 * MS, None);
    let mut e = engine(&n.p);
    let steps: Vec<(u64, &str, String)> = (0..40).map(|i| (1000 + i * 50, P_IP, sql("UPDATE t SET n = n + 1 WHERE id = 3"))).collect();
    let list: Vec<(u64, &str, &str)> = steps.iter().map(|(a, b, c)| (*a, *b, c.as_str())).collect();
    let _ = e.add_service(n.gen_, "drv", Box::new(Driver::new(&list)), true);
    run_for(&mut e, 4 * SEC);
    let p50 = metric(&e, n.rep, "lag_p50_us") as f64 / 1000.0;
    let want = 5.0 + 1.0 + 328.0 / 500e6 * 1000.0 + 0.014;
    check_near(t, "async lag per commit: one-way delay + the replica's log write + apply (ms)", p50, want, 0.03);
    t.eq("all applied", n_of(&e, n.rep, 3), n_of(&e, n.pri, 3));
    t.eq("idle: no lag now", (metric(&e, n.rep, "lag_ms"), metric(&e, n.rep, "lag_bytes")), (0, 0));
}

/// commits per second the replica applied, and how its backlog grew (transactions per second), at `rps`
fn under_load(rps: u64, row_us: u64) -> (f64, f64, i64) {
    let pri = db_cfg(r#"{"mode": "async"}"#, r#", "durability": "relaxed", "flush_ms": 2"#);
    let rep = db_cfg(&format!(r#"{{"primary": "{}", "apply_us": 5, "apply_row_us": {}}}"#, P_IP, row_us), r#", "pool_mb": 64"#);
    let n = net(&pri, &rep, None, None, Some(hw(1)), 50, Some(open_updates(rps)));
    let mut e = engine(&n.p);
    run_for(&mut e, 3 * SEC);
    let (a, la) = (metric(&e, n.rep, "applied_commits"), metric(&e, n.rep, "lag_bytes"));
    run_for(&mut e, 4 * SEC);
    let (b, lb) = (metric(&e, n.rep, "applied_commits"), metric(&e, n.rep, "lag_bytes"));
    let per_txn = (200 + 64 + 64) as f64;
    ((b - a) as f64 / 4.0, (lb - la) as f64 / per_txn / 4.0, metric(&e, n.rep, "lag_p99_us"))
}

fn lag_under_load(t: &mut T) {
    // a = 2 * 5 + 40 = 50 us per transaction: the replica applies at most 20 000 per second
    let (x, grow, p99) = under_load(10_000, 40);
    check_near(t, "lambda a = 0.5: applied = lambda (per second)", x, 10_000.0, 0.03);
    t.check(&format!("lambda a = 0.5: the backlog does not grow ({:.1} per second), p99 {} us", grow, p99), grow.abs() < 50.0 && p99 < 20_000);
    // a = 2 * 5 + 190 = 200 us: at most 5000 per second, 8000 come
    let (x, grow, _) = under_load(8000, 190);
    check_near(t, "lambda a = 1.6: applied = 1 / a (per second)", x, 5000.0, 0.03);
    check_near(t, "lambda a = 1.6: the backlog grows by lambda - 1 / a (per second)", grow, 3000.0, 0.03);
}

// ---------------------------------------------------------------- failover

/// one client writes to an async or sync primary, which is switched off at 2 s; the replica is promoted
fn kill_and_promote(mode: &str) -> (usize, u64, i64, Engine, Net) {
    let pri = db_cfg(&format!(r#"{{"mode": "{}", "sync_timeout_ms": 0}}"#, mode), "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 20 * MS, None);
    let mut e = engine(&n.p);
    let _ = e.add_service(n.gen_, "w", Box::new(Writer::new(P_IP, 60 * SEC)), true);
    run_for(&mut e, 2 * SEC);
    let _ = e.apply(Action::Power { dev: n.pri, on: false });
    run_for(&mut e, 10 * MS);
    let promoted = e.apply(Action::DbPromote { dev: n.rep, svc: String::from("db") }).is_ok();
    run_for(&mut e, 500 * MS);
    let w = writer(&e, n.gen_, "w");
    let ok = w.as_ref().map_or(Vec::new(), |w| w.ok.clone());
    // the answered commits the new primary does not have
    let missing = ok.iter().filter(|k| db(&e, n.rep).and_then(|d| d.row_now("t", **k)).is_none()).count();
    if !promoted {
        return (ok.len(), u64::MAX, -1, e, n);
    }
    (ok.len(), missing as u64, metric(&e, n.rep, "failover_lost"), e, n)
}

fn failover(t: &mut T) {
    let (ok, missing, counted, e, n) = kill_and_promote("async");
    t.check(&format!("async: {} commits answered, {} of them not on the replica", ok, missing), ok > 100 && missing > 0);
    t.eq("async: the failover lost exactly those, and counted them", counted, missing as i64);
    t.eq("the replica is primary now", (metric(&e, n.rep, "role"), metric(&e, n.rep, "epoch")), (1, 2));
    t.check("it says so in its log", e.log_lines(n.rep).iter().any(|l| l.text.contains("ist jetzt Primary in Epoche 2")));
    t.check("and in the recording", e.trace.events().any(|x| matches!(x.note, jfn_sim::Note::DbPromoted { epoch: 2 })));
    let (ok, missing, counted, _, _) = kill_and_promote("sync");
    t.check(&format!("sync: {} commits answered", ok), ok > 20);
    t.eq("sync: none lost, none counted", (missing, counted), (0, 0));
    // the refusals of a promotion
    let mut e2 = engine(&net(&db_cfg(r#"{}"#, ""), &db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), ""), None, None, None, 50, None).p);
    run_for(&mut e2, 500 * MS);
    let n2 = net(&db_cfg(r#"{}"#, ""), &db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), ""), None, None, None, 50, None);
    let refused = |e: &mut Engine, dev: u32| match e.apply(Action::DbPromote { dev, svc: String::from("db") }) {
        Ok(()) => String::from("taken"),
        Err(x) => x.msg().text(),
    };
    t.eq("a primary is one already", refused(&mut e2, n2.pri), String::from("Diese Datenbank ist schon Primary."));
    let _ = e2.apply(Action::DbPromote { dev: n2.rep, svc: String::from("db") });
    t.eq("twice (it had nothing left to apply)", refused(&mut e2, n2.rep), String::from("Diese Datenbank ist schon Primary."));
    let mut e3 = engine(&net(&db_cfg(r#"{}"#, ""), r#"{"replication": {"primary": "10.0.0.2"}}"#, None, None, None, 50, None).p);
    let _ = e3.apply(Action::Power { dev: n2.pri, on: false });
    run_for(&mut e3, 200 * MS);
    t.eq("without a copy", refused(&mut e3, n2.rep), String::from("Das Replikat hat noch keine vollständige Kopie der Daten: es kann nicht Primary werden."));
    t.eq("no replication", refused(&mut e3, n2.gen_), String::from("Den Dienst \u{201e}db\u{201c} gibt es nicht."));
}

/// a promotion while the old primary runs: it hears of it and steps down
fn switchover(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "async"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let rep2 = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, Some(&rep2), None, None, 50, None);
    let mut e = engine(&n.p);
    let steps = [(500, P_IP, sql("UPDATE t SET n = 5 WHERE id = 9")), (2000, P_IP, sql("UPDATE t SET n = 6 WHERE id = 9")), (2010, R_IP, sql("UPDATE t SET n = 7 WHERE id = 9")), (2500, R2_IP, sql("SELECT n FROM t WHERE id = 9"))];
    let list: Vec<(u64, &str, &str)> = steps.iter().map(|(a, b, c)| (*a, *b, c.as_str())).collect();
    let _ = e.add_service(n.gen_, "drv", Box::new(Driver::new(&list)), true);
    run_for(&mut e, 1500 * MS);
    let _ = e.apply(Action::DbPromote { dev: n.rep, svc: String::from("db") });
    run_for(&mut e, 400 * MS);
    t.eq("the new primary", (metric(&e, n.rep, "role"), metric(&e, n.rep, "epoch")), (1, 2));
    t.eq("the old one stepped down (fencing)", (metric(&e, n.pri, "role"), metric(&e, n.pri, "fenced"), metric(&e, n.pri, "epoch")), (2, 1, 2));
    t.eq("the other replica follows the new one", db(&e, n.rep2).and_then(|d| d.repl.as_ref()).and_then(|r| r.up_addr).map(|a| format!("{}", a)), Some(String::from(R_IP)));
    run_for(&mut e, 1100 * MS);
    let (s1, b1) = got(&e, n.gen_, 1);
    t.check(&format!("after the promotion the old primary takes no write: {} {}", s1, b1), s1 == 503 && b1.contains("\"code\":\"fenced\""));
    t.eq("the new one does", got(&e, n.gen_, 2).0, 200);
    t.check(&format!("and its replicas have it: {}", got(&e, n.gen_, 3).1), got(&e, n.gen_, 3).1.contains("\"n\":7"));
    t.eq("the old primary follows and has it too", n_of(&e, n.pri, 9), Some(7));
    t.check("its log says why", e.log_lines(n.pri).iter().any(|l| l.text.contains("Abgegrenzt")));
}

/// a partition between the primary and its replica: the replica is promoted, both sides take writes, the
/// partition heals
fn split(fencing: bool) -> (Engine, Net, u64, u64) {
    let pri = db_cfg(&format!(r#"{{"mode": "async", "fencing": {}}}"#, fencing), "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}", "fencing": {}}}"#, P_IP, fencing), "");
    let n = net(&pri, &rep, None, None, None, 50, None);
    let mut e = engine(&n.p);
    run_for(&mut e, SEC);
    let _ = e.apply(Action::Fault { fault: jfn_sim::Fault::Partition { a: alloc::vec![n.pri], b: alloc::vec![n.rep] }, dur: None });
    run_for(&mut e, 100 * MS);
    let _ = e.apply(Action::DbPromote { dev: n.rep, svc: String::from("db") });
    run_for(&mut e, 100 * MS);
    // writes on both sides for 2 s
    let _ = e.add_service(n.gen_, "wp", Box::new(Writer::new(P_IP, e.now() + 2 * SEC)), true);
    let mut wr = Writer::new(R_IP, e.now() + 2 * SEC);
    wr.key = 50_000;
    let _ = e.add_service(n.gen_, "wr", Box::new(wr), true);
    run_for(&mut e, 2100 * MS);
    let a = writer(&e, n.gen_, "wp").map_or(0, |w| w.ok.len() as u64);
    let b = writer(&e, n.gen_, "wr").map_or(0, |w| w.ok.len() as u64);
    let _ = e.apply(Action::HealAll);
    run_for(&mut e, 4 * SEC);
    (e, n, a, b)
}

fn partition(t: &mut T) {
    let (e, n, a, b) = split(false);
    t.check(&format!("without fencing both sides took writes: {} and {}", a, b), a > 50 && b > 50);
    t.eq("the old primary counts what it took since the fork", metric(&e, n.pri, "split_commits"), a as i64);
    t.eq("the new one what it took", metric(&e, n.rep, "split_commits"), b as i64);
    t.eq("and what the other side took", db(&e, n.rep).and_then(|d| d.repl.as_ref()).map(|r| r.stats.rival_commits), Some(a));
    t.check("both say split brain", db(&e, n.pri).is_some_and(|d| d.status().contains("Split Brain")) && db(&e, n.rep).is_some_and(|d| d.status().contains("Split Brain")));
    let (e, n, a, b) = split(true);
    t.check(&format!("with fencing too, while the partition lasts: {} and {}", a, b), a > 50 && b > 50);
    t.eq("when they meet, the old one stops: its commits since the fork are lost", (metric(&e, n.pri, "fenced"), metric(&e, n.pri, "fenced_lost")), (1, a as i64));
    t.eq("it follows the new one", metric(&e, n.pri, "role"), 2);
    let k = 50_001 + b / 2;
    t.check("and has the new one's writes, not its own", n_of(&e, n.pri, k).is_some() && db(&e, n.pri).and_then(|d| d.row_now("t", 2001)).is_none());
}

// ---------------------------------------------------------------- reads

/// a web server writes and then reads through its read address (the replica, 5 ms away)
fn web_reads(read_wait: u64, mode: &str, after: u64) -> (i64, i64, i64, i64) {
    let pri = db_cfg(&format!(r#"{{"mode": "{}"}}"#, mode), "");
    // (the replica's disk takes 10 ms: its log write is what a read right after a write overtakes)
    let rep = format!(r#"{{"tables": {}, "disk": {{"iops": 0, "mbps": 500, "latency_us": 10000}}, "replication": {{"primary": "{}"}}}}"#, TABLES, P_IP);
    let mut n = net(&pri, &rep, None, None, None, 5 * MS, None);
    let web = format!(r#"{{"port": 80, "db": {{"addr": "{}", "read": "{}", "read_wait_ms": {}}}, "routes": [
        {{"path": "/w/*", "method": "POST", "backend": "db", "sql": "UPDATE t SET n = n + 1 WHERE id = ?"}},
        {{"path": "/r/*", "method": "GET", "backend": "db", "sql": "SELECT n FROM t WHERE id = ?"}}]}}"#, P_IP, R_IP, read_wait);
    if let Some(d) = n.p.device_mut(n.rep2) {
        d.services = alloc::vec![def("http-server", "web", &web)];
    }
    let mut e = engine(&n.p);
    let mut steps = Vec::new();
    for i in 0..20u64 {
        steps.push((500 + i * 100, R2_IP, format!("POST /w/{}", i + 1)));
        steps.push((500 + after + i * 100, R2_IP, format!("GET /r/{}", i + 1)));
    }
    let _ = e.add_service(n.gen_, "web_drv", Box::new(WebDriver::new(&steps)), true);
    run_for(&mut e, 3 * SEC);
    let reads = e.metric(n.rep2, "web", "db_reads").unwrap_or(-1);
    let stale = e.metric(n.rep2, "web", "db_stale").unwrap_or(-1);
    (reads, stale, metric(&e, n.rep, "stale_reads"), metric(&e, n.rep, "read_waits"))
}

fn stale_reads(t: &mut T) {
    // the write is answered after 1 ms; its record reaches the replica after 5 ms and is applied after its 10 ms
    // log write; a read 4 ms after the write reaches the replica at 9 ms: before its write
    let (reads, stale, counted, waits) = web_reads(0, "async", 4);
    t.eq("async: every read went to the replica", reads, 20);
    // (the first write opens its connection first: its read comes before its answer)
    t.check(&format!("async, a read 4 ms after the write: stale {} of {}, counted {}", stale, reads, counted), stale >= 19 && counted == stale && waits == 0);
    let (reads, stale, counted, waits) = web_reads(50, "async", 4);
    t.check(&format!("with read_wait_ms 50 the replica waits and nothing is stale: {} {} {} {}", reads, stale, counted, waits), reads == 20 && stale == 0 && counted == 0 && waits >= 19);
    // apply: the write is answered after 1 + 5 + 10 + 5 ms
    let (reads, stale, counted, _) = web_reads(0, "apply", 30);
    t.eq("apply: the write's answer waits for the replica's apply, nothing is stale", (reads, stale, counted), (20, 0, 0));
}

/// a client of web servers: "POST path" and "GET path" at given moments
#[derive(Clone, Debug)]
struct WebDriver {
    steps: Vec<(Ns, String, String)>,
    next: usize,
    agent: Agent,
}

impl WebDriver {
    fn new(steps: &[(u64, &str, String)]) -> WebDriver {
        WebDriver { steps: steps.iter().map(|(ms, h, b)| (*ms * MS, String::from(*h), b.clone())).collect(), next: 0, agent: Agent::new(1) }
    }
    fn send_due(&mut self, os: &mut OsApi) {
        let now = os.now();
        while let Some((at, host, what)) = self.steps.get(self.next).cloned() {
            if at > now {
                os.timer(0, at);
                break;
            }
            self.next += 1;
            let (m, path) = what.split_once(' ').unwrap_or(("GET", what.as_str()));
            let mut r = Req::get(Url { host, port: 80, target: String::from(path) });
            r.method = String::from(m);
            r.timeout = 60 * SEC;
            let _ = self.agent.submit(os, r);
        }
    }
}

impl Service for WebDriver {
    fn kind(&self) -> &'static str {
        "test-web"
    }
    fn start(&mut self, os: &mut OsApi) {
        self.send_due(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        if let SvcEvent::Timer(0) = ev {
            self.send_due(os);
            return;
        }
        if self.agent.on(&ev, os) {
            let _ = self.agent.take_done();
        }
    }
    fn metrics(&self) -> Vec<Metric> {
        Vec::new()
    }
    fn status(&self) -> String {
        String::new()
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

/// a load balancer in front of two replicas checks /_db/health/replica?max_lag_ms=200: the one cut off from
/// its primary goes down, reads go to the other
fn read_balancer(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "async", "heartbeat_ms": 100, "timeout_ms": 300}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}", "heartbeat_ms": 100, "timeout_ms": 300}}"#, P_IP), "");
    let mut n = net(&pri, &rep, Some(&rep), None, None, 50, None);
    let lb = format!(r#"{{"mode": "http", "port": 7100, "backends": [{{"name": "r1", "addr": "{}", "port": 7100}}, {{"name": "r2", "addr": "{}", "port": 7100}}], "health": {{"type": "http", "path": "/_db/health/replica?max_lag_ms=200", "interval_ms": 200, "timeout_ms": 100, "rise": 1, "fall": 2}}}}"#, R_IP, R2_IP);
    if let Some(d) = n.p.device_mut(n.gen_) {
        d.services.push(def("lb", "lb", &lb));
    }
    let mut e = engine(&n.p);
    run_for(&mut e, 2 * SEC);
    t.eq("both replicas are up", e.metric(n.gen_, "lb", "backends_up"), Some(2));
    let _ = e.apply(Action::Fault { fault: jfn_sim::Fault::Partition { a: alloc::vec![n.pri], b: alloc::vec![n.rep2] }, dur: None });
    run_for(&mut e, 2 * SEC);
    t.eq("the replica cut off from its primary is down", e.metric(n.gen_, "lb", "backends_up"), Some(1));
    t.eq("which one", e.metric(n.gen_, "lb", "b2_up"), Some(0));
    let _ = e.apply(Action::HealAll);
    run_for(&mut e, 3 * SEC);
    t.eq("back when it caught up", e.metric(n.gen_, "lb", "backends_up"), Some(2));
}

// ---------------------------------------------------------------- the copy

fn base_copy(t: &mut T) {
    // a primary with 10 million rows (about 2 GB of data and indexes) and a replica without tables on a
    // cable of 1 Gbit/s: the copy takes data / bandwidth
    let big = r#"[{"name": "t", "rows": 10000000, "row_bytes": 200, "columns": ["n"]}]"#;
    let pri = format!(r#"{{"tables": {}, "disk": {{"iops": 0, "mbps": 5000, "latency_us": 100, "capacity_mb": 100000}}, "replication": {{}}}}"#, big);
    let rep = format!(r#"{{"disk": {{"iops": 0, "mbps": 5000, "latency_us": 100, "capacity_mb": 100000}}, "replication": {{"primary": "{}"}}}}"#, P_IP);
    let mut n = net(&pri, &rep, None, None, None, 50, None);
    for l in n.p.links.iter_mut() {
        if l.id == n.link_rep {
            l.kbps = 1_000_000;
        }
    }
    // the replica starts later: the primary has a touched row by then
    if let Some(d) = n.p.device_mut(n.rep) {
        d.services[0].disabled = true;
    }
    let mut e = engine(&n.p);
    let steps = [(300, P_IP, sql("UPDATE t SET n = 99 WHERE id = 1234567"))];
    let list: Vec<(u64, &str, &str)> = steps.iter().map(|(a, b, c)| (*a, *b, c.as_str())).collect();
    let _ = e.add_service(n.gen_, "drv", Box::new(Driver::new(&list)), true);
    run_for(&mut e, SEC);
    let _ = e.apply(Action::ServiceRun { dev: n.rep, name: String::from("db"), on: true });
    let start = e.now();
    let mut done = None;
    for _ in 0..400 {
        run_for(&mut e, 100 * MS);
        if db(&e, n.rep).and_then(|d| d.repl_view(e.now())).is_some_and(|v| v.copying.is_none() && v.connected) {
            done = Some(e.now() - start);
            break;
        }
    }
    let bytes = db(&e, n.rep).and_then(|d| d.repl.as_ref()).map_or(0, |r| r.stats.copies);
    t.eq("one copy", bytes, 1);
    let data = db(&e, n.pri).map_or(0, |d| {
        let l = d.cfg.layout;
        d.tables().iter().map(|x| (x.heap_pages + 2 * jfn_sim::dbstore::Layout::new(l.page, l.fanout).tree_pages(x.rows) / 2) * l.page).sum::<u64>()
    });
    // (TCP carries 1460 bytes of data in a frame of 1538 bytes on the wire with preamble and gap)
    let want = data as f64 * 8.0 / (1e9 * 1460.0 / 1538.0);
    check_near(t, "the copy: data / (bandwidth * 1460 / 1538) (s)", done.unwrap_or(0) as f64 / SEC as f64, want, 0.03);
    t.eq("the touched row came with it", n_of(&e, n.rep, 1_234_567), Some(99));
    t.eq("an untouched row is the same as on the primary", db(&e, n.rep).and_then(|d| d.row_now("t", 77)), db(&e, n.pri).and_then(|d| d.row_now("t", 77)));
}

/// a replica that is gone: sync waits sync_timeout_ms, then falls back to async and says so; it comes back
fn gone_replica(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "sync", "sync_timeout_ms": 1000}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 50, None);
    let mut e = engine(&n.p);
    let _ = e.add_service(n.gen_, "w", Box::new(Writer::new(P_IP, 60 * SEC)), true);
    run_for(&mut e, SEC);
    let _ = e.apply(Action::Power { dev: n.rep, on: false });
    run_for(&mut e, 500 * MS);
    t.eq("a commit waits for the replica that is gone", metric(&e, n.pri, "sync_waiting"), 1);
    run_for(&mut e, 700 * MS);
    t.eq("after sync_timeout_ms: async", (metric(&e, n.pri, "sync_waiting"), metric(&e, n.pri, "sync_fallbacks")), (0, 1));
    t.check("the status says so", db(&e, n.pri).is_some_and(|d| d.status().contains("async statt sync")));
    t.check("the log too", e.log_lines(n.pri).iter().any(|l| l.text.contains("Commits gehen jetzt async statt sync")));
    let a = writer(&e, n.gen_, "w").map_or(0, |w| w.ok.len());
    run_for(&mut e, 500 * MS);
    t.check("and commits go on", writer(&e, n.gen_, "w").map_or(0, |w| w.ok.len()) > a + 100);
    let _ = e.apply(Action::Power { dev: n.rep, on: true });
    run_for(&mut e, 3 * SEC);
    t.check("back to sync once it caught up", db(&e, n.pri).is_some_and(|d| d.repl.as_ref().is_some_and(|r| !r.fallback)));
    t.check("the problem shows while it is gone", {
        let mut e2 = engine(&n.p);
        let _ = e2.apply(Action::Power { dev: n.pri, on: false });
        run_for(&mut e2, 6 * SEC);
        e2.problems().iter().any(|p| p.kind == jfn_sim::problems::ProblemKind::ReplicaNoPrimary && p.text.contains("erreicht seinen Primary 10.0.0.2 nicht"))
    });
}

// ---------------------------------------------------------------- determinism

fn determinism(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "semi"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, Some(hw(2)), Some(hw(1)), 2 * MS, Some(open_updates(500)));
    let start = || engine(&n.p);
    let (mut a, mut b) = (start(), start());
    run_for(&mut a, 2 * SEC);
    run_for(&mut b, 2 * SEC);
    t.eq("two runs: equal", digest(&a), digest(&b));
    let mut c1 = start();
    run_for(&mut c1, SEC);
    t.check("records are on their way now", metric(&c1, n.rep, "applied_commits") > 100);
    let mut c2 = c1.clone();
    run_for(&mut c1, SEC);
    run_for(&mut c2, SEC);
    t.eq("a clone of a warm engine with the log in flight: equal", digest(&c1), digest(&c2));
    t.eq("and it equals the plain run", digest(&c1), digest(&a));
    // a keyframe with the log in flight replays to the same result, a promotion in the recording too
    let mut drv = Inline::new(start());
    drv.rewind_on(Budget { gap_events: 2000, ..Budget::default() });
    for i in 0..20 {
        if i == 10 {
            let _ = drv.send(Cmd::Apply(Action::DbPromote { dev: n.rep, svc: String::from("db") }), 0);
        }
        let _ = drv.send(Cmd::RunFor(100 * MS), 0);
    }
    let _ = drv.reports();
    let live = digest(&drv.eng);
    t.eq("the promotion ran", drv.eng.metric(n.rep, "db", "role"), Some(1));
    let pos = drv.pos();
    let past = drv.rewind().and_then(|rw| {
        let mut r = drv.restore_pos(pos, None)?;
        if r.run(rw, usize::MAX) { Some(r.finish()) } else { None }
    });
    t.eq("the replay equals the run, the promotion with it", past.map(|p| digest(&p.eng)), Some(live));
    let fp = db(&a, n.rep).map_or(0, |d| d.footprint());
    t.check(&format!("its footprint counts what it holds: {}", fp), fp > 8 << 10);
}

/// a database without replication runs as it did: no new port, no new figures, its health as before
fn old_project(t: &mut T) {
    let Ok(p) = Project::from_text(include_str!("../tests/fixtures/db_basic.jfnet")) else {
        t.check("db_basic reads", false);
        return;
    };
    let mut e = engine(&p);
    run_for(&mut e, 2 * SEC);
    let dev = p.devices.iter().find(|d| d.name == "DB").map_or(0, |d| d.id);
    let d = db(&e, dev);
    t.check("no replication", d.is_some_and(|d| d.repl.is_none()));
    t.check("no figure of it", e.metric(dev, "db", "lag_ms").is_none() && e.metric(dev, "db", "role").is_none());
    t.check("its settings say nothing of it", d.is_some_and(|d| d.repl_view(0).is_none()));
}

/// dbctl replication and promote, in German and English
fn tools(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "sync"}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}"}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 50, None);
    let mut e = engine(&n.p);
    run_for(&mut e, SEC);
    let out = |e: &mut Engine, dev: u32, line: &str| -> String {
        let _ = e.take_reports();
        let _ = e.cli_line(dev, line);
        let to = e.now() + 100 * MS;
        e.run_until(to, BUDGET);
        let mut s = String::new();
        for r in e.take_reports() {
            if let Report::CliOut { dev: d, bytes, .. } = r
                && d == dev
            {
                s.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
        s
    };
    let s = out(&mut e, n.pri, "dbctl replication");
    t.check(&format!("the primary: {}", s), s.contains("Primary in Epoche 1, Modus sync") && s.contains("10.0.0.3") && s.contains("folgt"));
    let s = out(&mut e, n.rep, "dbctl replication");
    t.check(&format!("the replica: {}", s), s.contains("Replikat von 10.0.0.2 in Epoche 1"));
    let s = out(&mut e, n.pri, "dbctl promote");
    t.check(&format!("a primary refuses: {}", s), s.contains("dbctl: Diese Datenbank ist schon Primary."));
    let s = out(&mut e, n.rep, "dbctl promote");
    t.check(&format!("the replica takes it: {}", s), s.contains("wird Primary"));
    run_for(&mut e, 500 * MS);
    let _ = e.apply(Action::ConsoleLang { lang: jfn_model::msg::Lang::En });
    let s = out(&mut e, n.rep, "dbctl replication");
    // (its own config has no mode: async)
    t.check(&format!("in English: {}", s), s.contains("Primary in epoch 2, mode async"));
    let s = out(&mut e, n.rep, "man dbctl");
    t.check("its man page", s.contains("promote"));
}

/// failover_ms: a replica without a word from its primary for so long promotes itself (after its timeout found the
/// connection dead)
fn auto_failover(t: &mut T) {
    let pri = db_cfg(r#"{"mode": "async", "heartbeat_ms": 100, "timeout_ms": 300}"#, "");
    let rep = db_cfg(&format!(r#"{{"primary": "{}", "heartbeat_ms": 100, "timeout_ms": 300, "failover_ms": 1000}}"#, P_IP), "");
    let n = net(&pri, &rep, None, None, None, 50, None);
    let mut e = engine(&n.p);
    run_for(&mut e, SEC);
    let _ = e.apply(Action::Power { dev: n.pri, on: false });
    run_for(&mut e, 700 * MS);
    t.eq("not yet after 0.7 s", metric(&e, n.rep, "role"), 2);
    run_for(&mut e, 1500 * MS);
    t.eq("after timeout_ms + failover_ms it is primary itself", (metric(&e, n.rep, "role"), metric(&e, n.rep, "epoch")), (1, 2));
    t.check("its log says why", e.log_lines(n.rep).iter().any(|l| l.text.contains("wird selbst Primary")));
}

/// the form against the JSON it writes (workspace/devwin/svcform.rs)
fn form(t: &mut T) {
    use crate::workspace::devwin::svcform::{fields, fill, merged};
    let base = cfg(&db_cfg(r#"{"role": "replica", "primary": "10.0.0.2", "mode": "sync"}"#, ""));
    let at = |key: &str| fields("db").position(|f| f.key == key).unwrap_or(usize::MAX);
    let v = fill("db", &base);
    let text = |v: &crate::workspace::devwin::svcform::Vals, k: &str| v.texts.get(at(k)).cloned().unwrap_or_default();
    t.eq("the form shows the role", text(&v, "replication.role"), String::from("replica"));
    t.eq("its primary", text(&v, "replication.primary"), String::from("10.0.0.2"));
    t.eq("its mode", text(&v, "replication.mode"), String::from("sync"));
    let mut v2 = v.clone();
    if let Some(x) = v2.texts.get_mut(at("replication.sync_count")) {
        *x = String::from("2");
    }
    let m = merged("db", &base, &v2).ok();
    let repl = m.as_ref().and_then(|c| c.get("replication"));
    t.eq("what the form writes", repl.and_then(|r| r.get("sync_count")).and_then(|x| x.as_u64()), Some(2));
    t.eq("and the rest stays", repl.and_then(|r| r.get("mode")).and_then(|x| x.as_str()), Some("sync"));
    t.check("a config the database takes", m.as_ref().is_some_and(|c| jfn_sim::db::from_def_msg(c).is_ok()));
    // every field of replication empty: the member goes, and with it replication
    let mut v3 = v.clone();
    for k in ["replication.role", "replication.primary", "replication.mode"] {
        if let Some(x) = v3.texts.get_mut(at(k)) {
            x.clear();
        }
    }
    let m3 = merged("db", &base, &v3).ok();
    t.check("emptied: no replication", m3.as_ref().is_some_and(|c| c.get("replication").is_none()));
}
