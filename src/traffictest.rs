// traffictest -- traffic management (part of --selftest, group traffic; jfn_sim's traffic.rs, the balancer and
// proxy of lb.rs, dbpool.rs, the load generator's retries, DNS round robin): the models against closed forms,
// because learners take the figures for the truth; every refusal with its words; live or restart as predicted;
// old configurations unchanged
//
// Each comparison names its formula:
//
//   rate limit     admitted = burst + rate * span (exact to one request)
//   retries        load at a backend failing with p, n retries: lambda (1 + p + .. + p^n); budget b: <= lambda (1 + b)
//   slow start     weight(t) = t / slow_start of the full one: share = f / (n - 1 + f), f = t / slow_start
//   breaker        opens after `failures`, lets `probes` through after open_ms, closes after their success
//   outlier        ejected after k failures; ejected <= cap * backends at every moment
//   queue          X = max_conns / service time; in flight at a backend <= max_conns
//   TLS            new connections per second <= cores * speed / handshake; with keep-alive the CPU per request
//                  falls by the reuse factor
//   cache          hit rate = 1 - misses / requests = (lambda T / K) / (1 + lambda T / K), K keys, TTL T
//   pooler         concurrency at db <= pool; closed loop: W = (N - c) / X, mean queue = N - c (Little)
//   DNS            each address first equally often over a full cycle; the resolver keeps the first for the TTL
//   determinism    a clone of a warm engine equals the run that went on; 1 and N sweep threads, the same bytes
//
// Floating point only compares results.  "--selftest --only=traffic --numbers" prints each comparison.

use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::any::Any;

use agent_core::jmap::Value;
use jfn_model::catalog::{DiskClass, Hw};
use jfn_model::experiment::Experiment;
use jfn_model::project::{PortRef, Project, ServiceDef};
use jfn_run::run_sweep;
use jfn_sim::dbpool::DbPool;
use jfn_sim::experiment::Plan;
use jfn_sim::http::{Agent, Req, Url};
use jfn_sim::keyframe::digest;
use jfn_sim::lb::{Lb, LbOp};
use jfn_sim::node::port_mac;
use jfn_sim::os::{Metric, OsApi, Service, SvcEvent};
use jfn_sim::traffic::{BState, Budget, LimitCfg, Limiter, RetryCfg};
use jfn_sim::{Action, Engine, MS, SEC};
use netpkt::http::{Headers, Version};
use netpkt::{IpAddr, Ipv4, Mac, Prefix4};

use crate::selftest::T;
use crate::workspace::devwin::svcform::{fill, merged};

const BUDGET: usize = 400_000_000;
const SEED: u64 = 31;

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: String::from(port) }
}

fn mac(dev: u32) -> Mac {
    port_mac(dev, 0)
}

/// the address of host i (0-based): 10.0.0.(i + 1)
fn addr(i: usize) -> Ipv4 {
    Ipv4([10, 0, 0, i as u8 + 1])
}

fn cfg(text: &str) -> Value {
    agent_core::json::parse(text.as_bytes()).unwrap_or(Value::Null)
}

fn def(kind: &str, name: &str, config: &str) -> ServiceDef {
    ServiceDef { kind: String::from(kind), name: String::from(name), disabled: false, config: cfg(config) }
}

/// adds a service from a definition; true when the engine took it
fn add(e: &mut Engine, dev: u32, kind: &str, name: &str, config: &str) -> bool {
    e.apply(Action::Service { dev, def: def(kind, name, config) }).is_ok()
}

fn hw(cores: u32, speed_pm: u32) -> Hw {
    Hw { cores, speed_pm, ram_mb: 8 << 10, swap_mb: 1 << 10, disk: DiskClass::Nvme, disk_gb: 400 }
}

/// hosts 10.0.0.1..n ("server-1u") on one switch, every ARP entry set by hand
fn net(n: usize) -> (Engine, Vec<u32>) {
    let mut p = Project::new("traffic", SEED);
    let sw = p.add_device("jf-s48x", 0, 200).unwrap_or(0);
    let hosts: Vec<u32> = (0..n).map(|i| p.add_device("server-1u", i as i32 * 100, 0).unwrap_or(0)).collect();
    for (i, h) in hosts.iter().enumerate() {
        let _ = p.connect(&pr(*h, "eth0"), &pr(sw, &format!("te1/0/{}", i + 1)), None);
    }
    let mut e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    for (i, h) in hosts.iter().enumerate() {
        let _ = e.apply(Action::IfAddr { dev: *h, iface: String::from("eth0"), prefix: Prefix4 { addr: addr(i), len: 24 }, add: true });
    }
    for (i, a) in hosts.iter().enumerate() {
        for (j, b) in hosts.iter().enumerate() {
            if i != j {
                let _ = e.apply(Action::StaticArp { dev: *a, ip: addr(j), mac: Some(mac(*b)) });
            }
        }
    }
    (e, hosts)
}

fn run_for(e: &mut Engine, d: u64) {
    let t = e.now() + d;
    e.run_until(t, BUDGET);
    let _ = e.take_reports();
}

fn metric(e: &Engine, dev: u32, svc: &str, name: &str) -> i64 {
    e.metric(dev, svc, name).unwrap_or(-1)
}

fn numbers() -> bool {
    plat::args().iter().any(|a| a == "--numbers")
}

fn section(name: &str) {
    if numbers() {
        plat::write_stdout(format!("-- {}\n", name).as_bytes());
    }
}

fn check_near(t: &mut T, name: &str, got: f64, want: f64, tol: f64) {
    let line = format!("{}: {:.3} against {:.3} (within {:.1} %)", name, got, want, tol * 100.0);
    if numbers() {
        plat::write_stdout(format!("{}, off by {:.2} %\n", line, (got - want).abs() / want.abs().max(1e-12) * 100.0).as_bytes());
    }
    t.check(&line, (got - want).abs() <= want.abs() * tol);
}

/// the square root by Newton (no_std has no sqrt)
fn sqrt(x: f64) -> f64 {
    if x <= 0.0 {
        return 0.0;
    }
    (0..60).fold(x.max(1.0), |r, _| 0.5 * (r + x / r))
}

/// p to the power k (no_std has no powi)
fn pow(p: f64, k: i32) -> f64 {
    (0..k).fold(1.0, |a, _| a * p)
}

fn note(text: &str) {
    if numbers() {
        plat::write_stdout(format!("{}\n", text).as_bytes());
    }
}

/// backends web1.. on hosts 2.. (10.0.0.3..) as the JSON of a balancer
fn backends(n: usize) -> String {
    let b: Vec<String> = (0..n).map(|i| format!("{{\"name\": \"web{}\", \"addr\": \"10.0.0.{}\"}}", i + 1, i + 3)).collect();
    format!("[{}]", b.join(", "))
}

/// an open loop of `rps` against a URL, `requests` in all
fn open_load(url: &str, rps: u64, requests: u64, extra: &str) -> String {
    format!("{{\"targets\": [\"{}\"], \"mode\": \"open\", \"rps\": {}, \"requests\": {}, \"timeout_ms\": 30000{}}}", url, rps, requests, extra)
}

// ---------------------------------------------------------------- a probe

/// an answer a Probe got
#[derive(Clone, Debug, PartialEq, Eq)]
struct Got {
    i: usize,
    end: u64,
    status: u16,
    body: String,
    headers: Headers,
}

impl Got {
    fn header(&self, h: &str) -> String {
        String::from(self.headers.get(h).unwrap_or(""))
    }
}

/// sends the requests of a plan at their times and keeps the answers
#[derive(Clone, Debug)]
struct Probe {
    plan: Vec<(u64, Req)>,
    agent: Agent,
    ids: BTreeMap<u64, usize>,
    next: usize,
    got: Vec<Got>,
}

impl Probe {
    fn arm(&mut self, os: &mut OsApi) {
        if let Some(p) = self.plan.get(self.next) {
            let at = p.0.max(os.now());
            os.timer(0, at);
        }
    }
}

impl Service for Probe {
    fn kind(&self) -> &'static str {
        "probe"
    }
    fn start(&mut self, os: &mut OsApi) {
        self.arm(os);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        if let SvcEvent::Timer(0) = ev {
            let now = os.now();
            while let Some(p) = self.plan.get(self.next) {
                if p.0 > now {
                    break;
                }
                let r = p.1.clone();
                let id = self.agent.submit(os, r);
                self.ids.insert(id, self.next);
                self.next += 1;
            }
            self.arm(os);
            return;
        }
        if self.agent.on(&ev, os) {
            for d in self.agent.take_done() {
                let i = self.ids.remove(&d.id).unwrap_or(usize::MAX);
                let g = match d.result {
                    Ok(r) => Got { i, end: d.end, status: r.head.status, body: String::from_utf8_lossy(&r.body).into_owned(), headers: r.head.headers },
                    Err(f) => Got { i, end: d.end, status: 0, body: f.text(), headers: Headers::new() },
                };
                self.got.push(g);
            }
        }
    }
    fn metrics(&self) -> Vec<Metric> {
        vec![Metric::new("got", self.got.len() as i64)]
    }
    fn status(&self) -> String {
        format!("{} Antworten", self.got.len())
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

fn req(method: &str, url: &str, body: &str, headers: &[(&str, &str)]) -> Req {
    let u = Url::parse(url).unwrap_or(Url { host: String::new(), port: 80, target: String::from("/") });
    let mut h = Headers::new();
    for (k, v) in headers {
        h.add(k, v);
    }
    Req { method: String::from(method), url: u, version: Version::Http11, headers: h, body: Vec::from(body.as_bytes()), timeout: 0, connect_timeout: 0, reuse: true, keep_body: true }
}

fn probe(e: &mut Engine, dev: u32, name: &str, plan: Vec<(u64, Req)>) -> bool {
    e.add_service(dev, name, Box::new(Probe { plan, agent: Agent::new(1), ids: BTreeMap::new(), next: 0, got: Vec::new() }), true).is_ok()
}

fn got(e: &Engine, dev: u32, name: &str) -> Vec<Got> {
    let mut g = e.service::<Probe>(dev, name).map(|p| p.got.clone()).unwrap_or_default();
    g.sort_by_key(|x| x.i);
    g
}

fn lb<'a>(e: &'a Engine, dev: u32, svc: &str) -> Option<&'a Lb> {
    e.service::<Lb>(dev, svc)
}

// ---------------------------------------------------------------- the group

pub fn run(t: &mut T) {
    refusals(t);
    compat(t);
    parts(t);
    limit_t(t);
    retry_t(t);
    client_retry_t(t);
    breaker_t(t);
    outlier_t(t);
    slow_start_t(t);
    queue_t(t);
    sticky_t(t);
    routes_t(t);
    tls_t(t);
    cache_t(t);
    dbpool_t(t);
    dns_t(t);
    reconfig_t(t);
    forms_t(t);
    problems_t(t);
    determinism_t(t);
}

// ---------------------------------------------------------------- refusals, old configurations

fn refused(kind: &str, text: &str) -> String {
    let c = cfg(text);
    let r = match kind {
        "lb" => jfn_sim::lb::from_def_msg(&c).err(),
        "proxy" => jfn_sim::lb::proxy_from_def_msg(&c).err(),
        "dbpool" => jfn_sim::dbpool::from_def_msg(&c).err(),
        k => jfn_sim::httpsvc::from_def_msg(k, &c).err(),
    };
    r.map(|m| m.text()).unwrap_or_default()
}

fn refusals(t: &mut T) {
    section("refusals");
    let b = backends(1);
    let cases: Vec<(&str, String, &str)> = vec![
        ("lb", format!(r#"{{"backends": {}, "tls": {{}}}}"#, b), "„tls“ gibt es nur beim Proxy (Dienstart proxy)."),
        ("lb", format!(r#"{{"backends": {}, "cache": {{}}}}"#, b), "„cache“ gibt es nur beim Proxy (Dienstart proxy)."),
        ("lb", format!(r#"{{"mode": "tcp", "backends": {}, "retry": {{}}}}"#, b), "„retry“ gibt es nur im HTTP-Modus."),
        ("lb", format!(r#"{{"mode": "tcp", "backends": {}, "pools": []}}"#, b), "„pools“ gibt es nur im HTTP-Modus."),
        ("lb", format!(r#"{{"backends": {}, "queue": 5}}"#, b), "„queue“ braucht „max_conns“: ohne Grenze wartet niemand."),
        ("lb", format!(r#"{{"backends": {}, "retry": {{"jitter": "wild"}}}}"#, b), "„retry.jitter“ kennt wild nicht (none, full, equal)."),
        ("lb", format!(r#"{{"backends": {}, "retry": {{"on": ["4xx"]}}}}"#, b), "„retry.on“ kennt 4xx nicht (connect, timeout, reset, 5xx)."),
        ("lb", format!(r#"{{"backends": {}, "retry": {{"budget": "viel"}}}}"#, b), "„retry.budget“ ist ein Anteil zwischen 0 und 1."),
        ("lb", format!(r#"{{"backends": {}, "retry": 3}}"#, b), "„retry“ braucht ein Objekt."),
        ("lb", format!(r#"{{"backends": {}, "limit": {{"burst": 3}}}}"#, b), "„limit“ braucht „rate“ (Anfragen je Sekunde)."),
        ("lb", format!(r#"{{"backends": {}, "limit": {{"rate": 5, "key": "user"}}}}"#, b), "„limit.key“ kennt user nicht (all, client, route, header:NAME)."),
        ("lb", format!(r#"{{"backends": {}, "limit": {{"rate": 5, "status": 200}}}}"#, b), "„limit.status“: 200 ist kein Fehlerstatus (400 bis 599)."),
        ("lb", format!(r#"{{"backends": {}, "sticky": {{"type": "ip"}}}}"#, b), "„sticky.type“ kennt ip nicht (cookie, client)."),
        ("lb", format!(r#"{{"backends": {}, "sticky": {{"name": "a b"}}}}"#, b), "„sticky.name“: a b taugt nicht als Name eines Cookies."),
        ("lb", format!(r#"{{"backends": {}, "outlier": {{"max_ejected": "halb"}}}}"#, b), "„outlier.max_ejected“ ist ein Anteil zwischen 0 und 1."),
        ("lb", format!(r#"{{"backends": {}, "routes": [{{"path": "/a", "pool": "x"}}]}}"#, b), "Route 1: den Pool x gibt es nicht."),
        ("lb", format!(r#"{{"backends": {}, "routes": [{{"path": "a"}}]}}"#, b), "Route 1: der Pfad a beginnt nicht mit /."),
        ("lb", format!(r#"{{"backends": {}, "routes": [{{"path": "/a", "strip": "/a"}}]}}"#, b), "Route 1: „strip“ gibt es nur beim Proxy (Dienstart proxy)."),
        ("lb", format!(r#"{{"backends": {}, "pools": [{{"backends": []}}]}}"#, b), "Pool 1 braucht „name“."),
        ("lb", format!(r#"{{"backends": {}, "pools": [{{"name": "default", "backends": []}}]}}"#, b), "Den Pool default gibt es zweimal."),
        ("lb", format!(r#"{{"backends": {}, "pools": [{{"name": "api", "backends": []}}]}}"#, b), "Der Pool api hat keine Backends."),
        ("proxy", format!(r#"{{"mode": "tcp", "backends": {}}}"#, b), "Ein Proxy spricht nur HTTP, nicht tcp."),
        ("proxy", String::from("{}"), "„backends“ oder „pools“ fehlt."),
        ("proxy", format!(r#"{{"backends": {}, "routes": [{{"path": "/", "add": "v1"}}]}}"#, b), "Route 1: „add“ beginnt nicht mit /."),
        ("proxy", format!(r#"{{"backends": {}, "routes": [{{"path": "/", "set_headers": {{"Host": "x"}}}}]}}"#, b), "Route 1: den Header Host setzt der Proxy nicht."),
        ("proxy", format!(r#"{{"backends": {}, "cache": 5}}"#, b), "„cache“ braucht ein Objekt."),
        ("dbpool", String::from("{}"), "„db“ fehlt: der Pooler braucht die Adresse der Datenbank."),
        ("dbpool", String::from(r#"{"db": {"addr": "x"}}"#), "„db.addr“ braucht eine IP-Adresse."),
        ("dbpool", String::from(r#"{"db": {"addr": "10.0.0.3"}, "pool": 0}"#), "„pool“ braucht mindestens eine Verbindung."),
        ("dbpool", String::from(r#"{"db": {"addr": "10.0.0.3"}, "mode": "session"}"#), "„mode“ kennt session nicht (statement, transaction)."),
        ("http-load", String::from(r#"{"targets": ["http://10.0.0.2/"], "retry": {"jitter": "x"}}"#), "„retry.jitter“ kennt x nicht (none, full, equal)."),
    ];
    for (kind, text, want) in cases {
        let got = refused(kind, &text);
        t.eq(&format!("{} refuses {}", kind, text), got, String::from(want));
    }
    // and what works
    t.eq("an https target", refused("http-load", r#"{"targets": ["https://10.0.0.2/"]}"#), String::new());
    t.eq("a proxy with pools only", refused("proxy", r#"{"pools": [{"name": "a", "backends": [{"addr": "10.0.0.3"}]}], "routes": [{"path": "/", "pool": "a"}]}"#), String::new());
}

fn compat(t: &mut T) {
    section("old configurations");
    let old = format!(r#"{{"backends": {}, "algorithm": "least-conn", "max_conns": 4, "retries": 2}}"#, backends(2));
    let l = jfn_sim::lb::from_def_msg(&cfg(&old)).ok();
    let ext = |b: &Option<Box<dyn Service>>| b.as_ref().and_then(|s| s.as_any().downcast_ref::<Lb>()).map(|l| l.ext);
    t.eq("an old balancer runs the old paths", ext(&l), Some(false));
    for key in [r#""queue": 3"#, r#""sticky": {}"#, r#""slow_start_ms": 1000"#, r#""outlier": {}"#, r#""breaker": {}"#, r#""retry": {}"#, r#""limit": {"rate": 1}"#, r#""routes": [{"path": "/"}]"#] {
        let c = format!(r#"{{"backends": {}, "max_conns": 4, {}}}"#, backends(2), key);
        let l = jfn_sim::lb::from_def_msg(&cfg(&c)).ok();
        t.eq(&format!("{} takes the new paths", key), ext(&l), Some(true));
    }
    // its settings have no member of a policy that is off
    let s = jfn_sim::svcform::effective("lb", &cfg(&old)).unwrap_or(Value::Null);
    t.check("settings: no policy members", ["retry", "limit", "breaker", "outlier", "sticky", "queue"].iter().all(|k| s.get(k).is_none()));
    // an old load generator: no retries, no new metrics
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[2], "http-server", "web", "{}") && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.3/", 100, 50, ""));
    run_for(&mut e, SEC);
    t.check("an old generator runs", ok && metric(&e, h[0], "load", "responses") == 50);
    t.check("and has no retry metrics", e.metric(h[0], "load", "retries").is_none());
    // an old zone keeps its order (rotation is opt-in)
    let mut p = Project::new("x", 1);
    let d = p.add_device("server-1u", 0, 0).unwrap_or(0);
    if let Some(dev) = p.device_mut(d) {
        dev.services.push(def("dns-server", "dns", r#"{"zones": [{"origin": "lab", "records": ["web A 10.0.0.3", "web A 10.0.0.4"]}]}"#));
    }
    let e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    t.check("a zone without rotate does not rotate", e.dns_zones(d).first().is_some_and(|z| !z.rotate));
    // old project files: their balancers run the old paths, their configs stay as written
    for (name, text) in [("lab_shards", include_str!("../tests/fixtures/lab_shards.jfnet")), ("scale_up_vs_out", include_str!("../tests/experiments/scale_up_vs_out.jfnet")), ("02-lastverteilung", include_str!("../assets/labs/02-lastverteilung.jfnet"))] {
        let Ok(p) = Project::from_text(text) else {
            t.check(&format!("{} reads", name), false);
            continue;
        };
        let e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
        let lbs: Vec<bool> = p.devices.iter().flat_map(|d| d.services.iter().filter(|s| s.kind == "lb").map(|s| e.service::<Lb>(d.id, &s.name).map(|l| l.ext))).flatten().collect();
        t.check(&format!("{}: {} balancers, all on the old paths", name, lbs.len()), !lbs.is_empty() && lbs.iter().all(|x| !x));
        t.eq(&format!("{}: written again, the same", name), Project::from_text(&p.to_text()).map(|q| q.to_text()).ok(), Some(p.to_text()));
    }
}

/// the parts alone: the budget's arithmetic, the token bucket, the backoff
fn parts(t: &mut T) {
    section("parts");
    // token bucket: from full, burst + rate * span
    let c = LimitCfg::of(&cfg(r#"{"rate": 10, "burst": 5, "key": "all"}"#)).ok();
    let mut l = Limiter::default();
    if let Some(c) = &c {
        let mut ok = 0;
        for k in 0..1000u64 {
            if l.take(k * 5 * MS, c, "").is_ok() {
                ok += 1;
            }
        }
        // the span is 4.995 s: 5 + floor(49.95) = 54
        t.eq("token bucket: burst + rate * span", ok, 54);
        t.eq("Retry-After in whole seconds", l.take(4995 * MS + MS, c, ""), Err(1));
    }
    // the budget: retries <= budget * requests
    let r = RetryCfg::of(&cfg(r#"{"budget": 0.2}"#)).unwrap_or_default();
    let mut b = Budget::default();
    let mut retried = 0;
    for k in 0..1000u64 {
        let now = k * 2 * MS;
        b.request(now);
        if b.take(now, &r) {
            retried += 1;
        }
    }
    t.eq("retry budget: one retry per five requests", retried, 200);
    // the backoff doubles up to its cap, the jitter stays below it
    let r = RetryCfg::of(&cfg(r#"{"backoff_ms": 10, "max_backoff_ms": 50, "jitter": "none"}"#)).unwrap_or_default();
    let mut rng = base::rng::Pcg32::new(1, 1);
    t.eq("backoff: 10, 20, 40, 50 ms", [1, 2, 3, 4].map(|a| r.delay(a, &mut rng) / MS), [10, 20, 40, 50]);
    let r = RetryCfg::of(&cfg(r#"{"backoff_ms": 10, "jitter": "full"}"#)).unwrap_or_default();
    t.check("full jitter stays within its backoff", (0..200).all(|_| r.delay(2, &mut rng) <= 20 * MS));
    t.check("only what is safe to repeat", r.repeats("GET", jfn_sim::traffic::Outcome::Status(503)) && !r.repeats("POST", jfn_sim::traffic::Outcome::Status(503)) && r.repeats("POST", jfn_sim::traffic::Outcome::Connect));
}

// ---------------------------------------------------------------- rate limits

fn limit_t(t: &mut T) {
    section("rate limit");
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": [{{"name": "web1", "addr": "10.0.0.3"}}], "health": {{"type": "none"}}, "limit": {{"rate": 50, "burst": 10, "key": "all"}}}}"#)) && add(&mut e, h[2], "http-server", "web", "{}") && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 200, 800, ""));
    run_for(&mut e, 6 * SEC);
    let passed = metric(&e, h[1], "lb", "status_2xx");
    let limited = metric(&e, h[1], "lb", "limited");
    // 800 arrivals 5 ms apart: a span of 3.995 s
    let want = 10.0 + 50.0 * 3.995;
    t.check("the limit runs", ok);
    check_near(t, "admitted = burst + rate * span", passed as f64, (want as i64) as f64, 1.0 / want);
    t.eq("the others got 429", (passed + limited, metric(&e, h[1], "lb", "status_4xx")), (800, limited));
    // per client: each its own bucket
    let (mut e, h) = net(4);
    let ok = add(&mut e, h[2], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.4"}], "health": {"type": "none"}, "limit": {"rate": 20, "burst": 5}}"#) && add(&mut e, h[3], "http-server", "web", "{}") && add(&mut e, h[0], "http-load", "a", &open_load("http://10.0.0.3/", 100, 200, "")) && add(&mut e, h[1], "http-load", "b", &open_load("http://10.0.0.3/", 100, 200, ""));
    run_for(&mut e, 4 * SEC);
    let (a, b) = (metric(&e, h[0], "a", "status_2xx"), metric(&e, h[1], "b", "status_2xx"));
    t.check(&format!("per client: {} and {} pass, 5 + 20 * 1.99 each", a, b), ok && a == 44 && b == 44);
    // the answer says when to come back
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "limit": {"rate": 0.5, "burst": 1, "key": "all"}}"#) && add(&mut e, h[2], "http-server", "web", "{}");
    let plan = vec![(100 * MS, req("GET", "http://10.0.0.2/", "", &[])), (200 * MS, req("GET", "http://10.0.0.2/", "", &[]))];
    let ok = ok && probe(&mut e, h[0], "probe", plan);
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "probe");
    t.eq("429 with Retry-After", g.iter().map(|x| (x.status, x.header("retry-after"))).collect::<Vec<_>>(), vec![(200, String::new()), (429, String::from("2"))]);
    t.check("limit probe ran", ok);
}

// ---------------------------------------------------------------- retries

/// a balancer with a retry policy before one web server that fails with probability p
fn retry_net(p: &str, retry: &str) -> (Engine, Vec<u32>, bool) {
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": [{{"name": "web1", "addr": "10.0.0.3"}}], "health": {{"type": "none"}}, "retry": {}}}"#, retry)) && add(&mut e, h[2], "http-server", "web", &format!(r#"{{"error_rate": {}, "error_status": 503}}"#, p)) && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 400, 4000, ""));
    (e, h, ok)
}

fn retry_t(t: &mut T) {
    section("retries at the balancer");
    // the amplification of n retries: 1 + p + p^2 + .. + p^n
    for (p, n) in [(0.5f64, 3u32), (0.2, 2), (0.8, 4)] {
        let (mut e, h, ok) = retry_net(&format!("{}", p), &format!(r#"{{"max": {}, "backoff_ms": 0, "jitter": "none"}}"#, n));
        run_for(&mut e, 15 * SEC);
        let at = metric(&e, h[2], "web", "requests") as f64;
        let f: f64 = (0..=n).map(|k| pow(p, k as i32)).sum();
        t.check("retry net runs", ok);
        check_near(t, &format!("p {} n {}: load = lambda (1 + p + .. + p^n)", p, n), at, 4000.0 * f, 0.04);
        // what comes back failed after all: p^(n+1) of the requests
        // (a binomial count: three standard deviations)
        let want = 4000.0 * pow(p, n as i32 + 1);
        check_near(t, &format!("p {} n {}: failed after all = p^(n+1)", p, n), metric(&e, h[0], "load", "status_5xx") as f64, want, 3.0 / sqrt(want));
    }
    // a budget caps it: at most lambda (1 + b)
    let (mut e, h, ok) = retry_net("0.8", r#"{"max": 4, "backoff_ms": 0, "jitter": "none", "budget": 0.2}"#);
    run_for(&mut e, 15 * SEC);
    let at = metric(&e, h[2], "web", "requests") as f64;
    t.check(&format!("budget 0.2: {} <= 4000 * 1.2", at), ok && at <= 4800.0 && at > 4000.0);
    note(&format!("budget 0.2 at p 0.8: {} at the backend, {} retries denied", at, metric(&e, h[1], "lb", "budget_denied")));
    t.check("and it says how often it said no", metric(&e, h[1], "lb", "budget_denied") > 0);
    // a POST is not repeated after a 503 (it may have changed something)
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "retry": {"max": 3, "backoff_ms": 0}}"#) && add(&mut e, h[2], "http-server", "web", r#"{"error_rate": 1, "error_status": 503}"#) && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 100, 100, r#", "mix": [{"method": "POST", "path": "/", "body": "x"}]"#));
    run_for(&mut e, 3 * SEC);
    t.eq("a POST is not repeated", (ok, metric(&e, h[2], "web", "requests")), (true, 100));
    // the backoff with jitter spreads the retries: they arrive later than the failure
    let (mut e, h, ok) = retry_net("1", r#"{"max": 2, "backoff_ms": 100, "jitter": "full"}"#);
    run_for(&mut e, 2 * SEC);
    let p50 = metric(&e, h[0], "load", "p50_us");
    t.check(&format!("backoff: the failures took longer ({} us)", p50), ok && p50 > 50_000);
}

fn client_retry_t(t: &mut T) {
    section("retries at the client (a retry storm)");
    for (p, n) in [(0.3f64, 3u32), (0.6, 2)] {
        let (mut e, h) = net(2);
        let ok = add(&mut e, h[1], "http-server", "web", &format!(r#"{{"error_rate": {}, "error_status": 503}}"#, p)) && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 400, 4000, &format!(r#", "retry": {{"max": {}, "backoff_ms": 5, "jitter": "none"}}"#, n)));
        run_for(&mut e, 15 * SEC);
        let f: f64 = (0..=n).map(|k| pow(p, k as i32)).sum();
        t.check("client retry runs", ok);
        check_near(t, &format!("client p {} n {}: load = lambda (1 + p + .. + p^n)", p, n), metric(&e, h[1], "web", "requests") as f64, 4000.0 * f, 0.04);
        t.eq("and it counts them", metric(&e, h[0], "load", "retries"), metric(&e, h[1], "web", "requests") - 4000);
    }
    // with a budget the client holds back
    let (mut e, h) = net(2);
    let ok = add(&mut e, h[1], "http-server", "web", r#"{"error_rate": 1, "error_status": 503}"#) && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 400, 4000, r#", "retry": {"max": 3, "backoff_ms": 5, "jitter": "none", "budget": 0.1}"#));
    run_for(&mut e, 15 * SEC);
    let at = metric(&e, h[1], "web", "requests");
    t.check(&format!("client budget 0.1: {} <= 4400", at), ok && (4000..=4400).contains(&at));
}

// ---------------------------------------------------------------- breaker and ejection

fn breaker_t(t: &mut T) {
    section("circuit breaker");
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "breaker": {"failures": 5, "open_ms": 1000, "probes": 2}}"#)
        && add(&mut e, h[2], "http-server", "web", r#"{"error_rate": 1, "time_ms": 1}"#)
        && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 1, "think_ms": 10}"#);
    let state = |e: &Engine| e.lb_view(h[1], "lb").and_then(|v| v.backends.first().map(|b| b.breaker));
    run_for(&mut e, 500 * MS);
    t.check("breaker runs", ok);
    t.eq("open after exactly 5 failures", (metric(&e, h[2], "web", "requests"), state(&e)), (5, Some(BState::Open)));
    t.check("meanwhile 503 at once (fail fast)", metric(&e, h[1], "lb", "fail_fast") > 20);
    run_for(&mut e, 1000 * MS);
    // half-open after open_ms: the first probe fails and it opens again
    t.eq("one probe after open_ms, it failed", (metric(&e, h[2], "web", "requests"), state(&e)), (6, Some(BState::Open)));
    let _ = e.apply(Action::HttpServerSet { dev: h[2], svc: String::from("web"), error_ppm: Some(0), time_ns: None, workers: None });
    run_for(&mut e, 1100 * MS);
    t.eq("closed after 2 successful probes", state(&e), Some(BState::Closed));
    t.check(&format!("and traffic flows again ({})", metric(&e, h[2], "web", "requests")), metric(&e, h[2], "web", "requests") > 20);
}

fn outlier_t(t: &mut T) {
    section("outlier ejection");
    // one of three backends fails every request: ejected after k = 3, for 2 s, then 4 s
    let (mut e, h) = net(5);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": {}, "health": {{"type": "none"}}, "outlier": {{"consecutive": 3, "base_ms": 2000, "max_ejected": 0.34}}}}"#, backends(3)))
        && add(&mut e, h[2], "http-server", "web", r#"{"error_rate": 1}"#)
        && add(&mut e, h[3], "http-server", "web", "{}")
        && add(&mut e, h[4], "http-server", "web", "{}")
        && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 1, "think_ms": 20}"#);
    run_for(&mut e, SEC);
    t.check("outlier runs", ok);
    t.eq("ejected after exactly 3 failures", (metric(&e, h[2], "web", "requests"), metric(&e, h[1], "lb", "ejected")), (3, 1));
    run_for(&mut e, 2500 * MS);
    t.eq("back after 2 s, out again after 3 more, now for 4 s", (metric(&e, h[2], "web", "requests"), metric(&e, h[1], "lb", "ejections")), (6, 2));
    // two of three fail: the cap keeps one in
    let (mut e, h) = net(5);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": {}, "health": {{"type": "none"}}, "outlier": {{"consecutive": 2, "base_ms": 500, "max_ejected": 0.34}}}}"#, backends(3)))
        && add(&mut e, h[2], "http-server", "web", r#"{"error_rate": 1}"#)
        && add(&mut e, h[3], "http-server", "web", r#"{"error_rate": 1}"#)
        && add(&mut e, h[4], "http-server", "web", "{}")
        && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 4, "think_ms": 5}"#);
    let mut most = 0;
    for _ in 0..100 {
        run_for(&mut e, 50 * MS);
        most = most.max(metric(&e, h[1], "lb", "ejected"));
    }
    t.check(&format!("the cap: at most {} of 3 ejected, {} ejections", most, metric(&e, h[1], "lb", "ejections")), ok && most == 1 && metric(&e, h[1], "lb", "ejections") > 3);
}

// ---------------------------------------------------------------- slow start

fn slow_start_t(t: &mut T) {
    section("slow start");
    let (mut e, h) = net(4);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "slow_start_ms": 10000}"#)
        && add(&mut e, h[2], "http-server", "web", "{}")
        && add(&mut e, h[3], "http-server", "web", "{}")
        && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 1000, 0, ""));
    run_for(&mut e, SEC);
    let t0 = e.now();
    let added = e.apply(Action::LbBackend { dev: h[1], svc: String::from("lb"), op: LbOp::Add { name: String::from("web2"), addr: IpAddr::V4(addr(3)), port: 80, weight: 1 } }).is_ok();
    t.check("slow start runs", ok && added);
    let count = |e: &Engine| (metric(e, h[2], "web", "requests"), metric(e, h[3], "web", "requests"));
    let ss = 10.0f64;
    let mut prev = count(&e);
    for k in 0..6 {
        let (a, b) = (k as f64 * 2.0, k as f64 * 2.0 + 2.0);
        let left = (t0 + (b * SEC as f64) as u64).saturating_sub(e.now());
        run_for(&mut e, left);
        let now = count(&e);
        let share = (now.1 - prev.1) as f64 / ((now.0 - prev.0) + (now.1 - prev.1)).max(1) as f64;
        // the mean of f / (1 + f) over [a, b], f = min(1, t / ss)
        let steps = 2000;
        let want: f64 = (0..steps).map(|i| a + (b - a) * (i as f64 + 0.5) / steps as f64).map(|t| (t / ss).min(1.0)).map(|f| f / (1.0 + f)).sum::<f64>() / steps as f64;
        check_near(t, &format!("share of the new backend in [{}, {}] s = f / (1 + f)", a, b), share, want, 0.03);
        prev = now;
    }
}

// ---------------------------------------------------------------- queue and shedding

fn queue_t(t: &mut T) {
    section("max_conns, queue, shedding");
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "max_conns": 2, "queue": 3, "queue_timeout_ms": 1000}"#)
        && add(&mut e, h[2], "http-server", "web", r#"{"time_ms": 100}"#)
        && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 10, "think_ms": 20, "timeout_ms": 30000}"#);
    run_for(&mut e, SEC);
    let a = metric(&e, h[2], "web", "requests");
    let mut most = 0;
    for _ in 0..80 {
        run_for(&mut e, 50 * MS);
        most = most.max(metric(&e, h[2], "web", "inflight"));
    }
    let b = metric(&e, h[2], "web", "requests");
    t.check("queue runs", ok);
    check_near(t, "X = max_conns / service time", (b - a) as f64 / 4.0, 2.0 / 0.1, 0.03);
    t.check(&format!("in flight at the backend <= max_conns: {}", most), most == 2);
    t.check(&format!("the rest shed: {}", metric(&e, h[1], "lb", "shed")), metric(&e, h[1], "lb", "shed") > 50);
    let pv = e.lb_view(h[1], "lb").and_then(|v| v.pools.first().cloned());
    t.check("the queue was full but never more", pv.as_ref().is_some_and(|p| p.max_waiting == 3));
    // a short queue timeout: 503 after it
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "max_conns": 1, "queue": 100, "queue_timeout_ms": 50}"#) && add(&mut e, h[2], "http-server", "web", r#"{"time_ms": 100}"#) && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 5, "think_ms": 10}"#);
    run_for(&mut e, 2 * SEC);
    t.check(&format!("queue timeouts: {}", metric(&e, h[1], "lb", "queue_timeouts")), ok && metric(&e, h[1], "lb", "queue_timeouts") > 10);
}

// ---------------------------------------------------------------- sticky sessions

fn sticky_t(t: &mut T) {
    section("sticky sessions");
    let (mut e, h) = net(5);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": {}, "health": {{"type": "none"}}, "sticky": {{"type": "cookie", "name": "JFNLB"}}}}"#, backends(3)));
    let ok = ok && (2..5).all(|i| add(&mut e, h[i], "http-server", "web", "{}"));
    let mut plan = vec![(100 * MS, req("GET", "http://10.0.0.2/", "", &[]))];
    for k in 0..6 {
        plan.push((200 * MS + k * 10 * MS, req("GET", "http://10.0.0.2/", "", &[("Cookie", "a=1; JFNLB=web2")])));
    }
    // web2 drains: the same cookie moves on
    plan.push((1500 * MS, req("GET", "http://10.0.0.2/", "", &[("Cookie", "JFNLB=web2")])));
    let ok = ok && probe(&mut e, h[0], "probe", plan);
    run_for(&mut e, SEC);
    let _ = e.apply(Action::LbBackend { dev: h[1], svc: String::from("lb"), op: LbOp::Drain { name: String::from("web2") } });
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "probe");
    t.check("sticky runs", ok && g.len() == 8);
    t.eq("a first request gets its cookie", g.first().map(|x| x.header("set-cookie")), Some(String::from("JFNLB=web1; Path=/")));
    t.check("with the cookie always the same backend, no new cookie", g[1..7].iter().all(|x| x.header("x-served-by") == "web2" && x.header("set-cookie").is_empty()));
    let last = g.last().cloned();
    t.check("its backend gone: another one, and a new cookie", last.as_ref().is_some_and(|x| x.header("x-served-by") != "web2" && x.header("set-cookie") == format!("JFNLB={}; Path=/", x.header("x-served-by"))));
    t.eq("the move is counted", e.lb_view(h[1], "lb").map(|v| v.pools[0].sticky_moved), Some(1));
    // by address: every request of one client to one backend; it goes down, the client moves
    let (mut e, h) = net(5);
    let ok = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": {}, "health": {{"type": "http", "interval_ms": 100, "fall": 1}}, "sticky": {{"type": "client"}}}}"#, backends(3))) && (2..5).all(|i| add(&mut e, h[i], "http-server", "web", "{}"));
    let ok = ok && add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "users": 4, "think_ms": 10}"#);
    run_for(&mut e, SEC);
    let per = |e: &Engine| (2..5).map(|i| metric(e, h[i], "web", "requests")).collect::<Vec<_>>();
    let first = per(&e);
    let busy: Vec<usize> = (0..3).filter(|&i| first[i] > 10).collect();
    t.check(&format!("one client, one backend: {:?}", first), ok && busy.len() == 1);
    if let Some(&b) = busy.first() {
        let _ = e.apply(Action::ServiceRun { dev: h[b + 2], name: String::from("web"), on: false });
        run_for(&mut e, SEC);
        let then = per(&e);
        let moved: Vec<usize> = (0..3).filter(|&i| i != b && then[i] > first[i] + 10).collect();
        t.check(&format!("it moved to one other: {:?}", then), moved.len() == 1);
    }
}

// ---------------------------------------------------------------- routes and the proxy

fn routes_t(t: &mut T) {
    section("routes and pools");
    let (mut e, h) = net(5);
    let lbc = r#"{"backends": [{"name": "web", "addr": "10.0.0.3"}], "pools": [{"name": "api", "algorithm": "round-robin", "backends": [{"name": "api1", "addr": "10.0.0.4"}, {"name": "api2", "addr": "10.0.0.5"}]}], "routes": [{"path": "/api/", "pool": "api"}], "health": {"type": "none"}}"#;
    let ok = add(&mut e, h[1], "lb", "lb", lbc) && (2..5).all(|i| add(&mut e, h[i], "http-server", "web", r#"{"routes": [{"path": "*", "body": "{path}"}]}"#));
    let plan: Vec<(u64, Req)> = ["/api/a", "/api/b", "/x", "/api/c"].iter().enumerate().map(|(k, p)| (100 * MS + k as u64 * 10 * MS, req("GET", &format!("http://10.0.0.2{}", p), "", &[]))).collect();
    let ok = ok && probe(&mut e, h[0], "probe", plan);
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "probe");
    t.eq("a path goes to its pool", g.iter().map(|x| x.header("x-served-by")).collect::<Vec<_>>(), vec![String::from("api1"), String::from("api2"), String::from("web"), String::from("api1")]);
    t.check("routes run", ok);
    // the proxy rewrites: by Host, strip and add a prefix, set and remove headers
    let (mut e, h) = net(5);
    let pc = r#"{"pools": [{"name": "shop", "backends": [{"name": "shop1", "addr": "10.0.0.3"}]}, {"name": "blog", "backends": [{"name": "blog1", "addr": "10.0.0.4"}]}], "routes": [{"host": "shop.lab", "path": "/api/", "pool": "shop", "strip": "/api", "add": "/v1", "set_headers": {"X-Env": "lab"}, "remove_headers": ["Cookie"]}, {"host": "blog.lab", "path": "/", "pool": "blog"}], "health": {"type": "none"}}"#;
    let ok = add(&mut e, h[1], "proxy", "proxy", pc) && (2..4).all(|i| add(&mut e, h[i], "http-server", "web", r#"{"routes": [{"path": "*", "body": "{path}"}]}"#));
    let plan = vec![
        (100 * MS, req("GET", "http://10.0.0.2/api/cart?x=1", "", &[("Host", "shop.lab")])),
        (110 * MS, req("GET", "http://10.0.0.2/post/7", "", &[("Host", "blog.lab:80")])),
        (120 * MS, req("GET", "http://10.0.0.2/api/cart", "", &[("Host", "other.lab")])),
    ];
    let ok = ok && probe(&mut e, h[0], "probe", plan);
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "probe");
    t.eq("by Host and prefix, rewritten", g.iter().map(|x| (x.status, x.body.clone())).collect::<Vec<_>>(), vec![(200, String::from("/v1/cart")), (200, String::from("/post/7")), (404, String::from("404 Not Found: no route of the proxy takes this request\n"))]);
    t.check("proxy runs", ok && lb(&e, h[1], "proxy").is_some_and(|p| p.kind() == "proxy"));
}

fn tls_t(t: &mut T) {
    section("TLS termination");
    // without keep-alive every request is a full handshake: X <= cores * speed / handshake
    let tls_proxy = |extra: &str| format!(r#"{{"backends": [{{"name": "web1", "addr": "10.0.0.3"}}], "health": {{"type": "none"}}, "tls": {{"names": ["10.0.0.2"]}}, "handshake_us": 2000, "tls_kb_us": 0, "cpu_us": 0{}}}"#, extra);
    let run = |extra: &str, keepalive: bool, rps: u64, hw_on: bool| {
        let (mut e, h) = net(3);
        if hw_on {
            let _ = e.apply(Action::Resize { dev: h[1], hw: Some(hw(1, 1000)), downtime: 0 });
        }
        let ok = add(&mut e, h[1], "proxy", "proxy", &tls_proxy(extra)) && add(&mut e, h[2], "http-server", "web", "{}") && add(&mut e, h[0], "http-load", "load", &format!(r#"{{"targets": ["https://10.0.0.2/"], "mode": "open", "rps": {}, "duration_ms": 6000, "timeout_ms": 60000, "keepalive": {}}}"#, rps, keepalive));
        run_for(&mut e, 2 * SEC);
        // (the CPU counts when a request comes, so per request is per arrival)
        let a = (metric(&e, h[1], "proxy", "responses"), metric(&e, h[1], "proxy", "tls_cpu_us"), metric(&e, h[1], "proxy", "tls_handshakes"), metric(&e, h[1], "proxy", "requests"));
        run_for(&mut e, 3 * SEC);
        let b = (metric(&e, h[1], "proxy", "responses"), metric(&e, h[1], "proxy", "tls_cpu_us"), metric(&e, h[1], "proxy", "tls_handshakes"), metric(&e, h[1], "proxy", "requests"));
        (ok, (b.0 - a.0) as f64 / 3.0, (b.1 - a.1) as f64 / (b.3 - a.3).max(1) as f64, b.2 - a.2, e, h)
    };
    let (ok, x, per_req, _, _, _) = run("", false, 800, true);
    t.check("TLS proxy runs", ok);
    check_near(t, "no keep-alive: X = cores * speed / handshake", x, 1000.0 / 2.0, 0.03);
    check_near(t, "no keep-alive: CPU per request = handshake", per_req, 2000.0, 0.01);
    let (ok, x, per_req2, _, _, _) = run(r#", "max_requests": 4"#, true, 1500, true);
    t.check("keep-alive runs", ok);
    check_near(t, "keep-alive, 4 per connection: CPU per request = handshake / 4", per_req2, per_req / 4.0, 0.03);
    check_near(t, "and X = cores * speed / (handshake / 4)", x, 1500.0, 0.03);
    // session reuse (modelled): one client address, all but the first resumed
    let (ok, _, per_req3, _, e, h) = run(r#", "session_reuse": true, "resume_us": 100"#, false, 800, true);
    check_near(t, "session reuse: CPU per request = resume_us", per_req3, 100.0, 0.02);
    t.check("one full handshake, the rest resumed", ok && metric(&e, h[1], "proxy", "tls_handshakes") == 1 && metric(&e, h[1], "proxy", "tls_resumed") > 1000);
    // without hardware it costs nothing, but counts
    let (ok, _, per_req4, hs, _, _) = run("", false, 200, false);
    t.check(&format!("no hardware: no CPU ({}), {} handshakes", per_req4, hs), ok && per_req4 == 0.0 && hs > 500);
}

fn cache_t(t: &mut T) {
    section("the proxy's cache");
    // K keys uniform, Poisson at lambda, TTL T: hit rate = (lambda T / K) / (1 + lambda T / K)
    for (rate, keys, ttl) in [(200u64, 50u64, 1000u64), (200, 400, 1000), (100, 20, 500)] {
        let (mut e, h) = net(3);
        let ok = add(&mut e, h[1], "proxy", "proxy", &format!(r#"{{"backends": [{{"name": "web1", "addr": "10.0.0.3"}}], "health": {{"type": "none"}}, "cache": {{"ttl_ms": {}, "entries": 10000}}}}"#, ttl))
            && add(&mut e, h[2], "http-server", "web", r#"{"routes": [{"path": "*", "body": "0123456789"}]}"#)
            && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", rate, rate * 30, &format!(r#", "arrival": "poisson", "mix": [{{"path": "/k{{rand:{}}}"}}]"#, keys)));
        run_for(&mut e, 31 * SEC);
        let (hits, misses) = (metric(&e, h[1], "proxy", "cache_hits") as f64, metric(&e, h[1], "proxy", "cache_misses") as f64);
        let x = rate as f64 * ttl as f64 / 1000.0 / keys as f64;
        t.check("cache runs", ok);
        check_near(t, &format!("rate {} keys {} ttl {} ms: hit rate = x / (1 + x)", rate, keys, ttl), hits / (hits + misses), x / (1.0 + x), 0.03);
        t.eq("the backend saw the misses", metric(&e, h[2], "web", "requests") as f64, misses);
        t.eq("bytes saved", metric(&e, h[1], "proxy", "bytes_saved") as f64, hits * 10.0);
    }
    // stale-while-revalidate: expired entries answer at once, one request renews them
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "proxy", "proxy", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "cache": {"ttl_ms": 200, "stale_ms": 5000}}"#)
        && add(&mut e, h[2], "http-server", "web", r#"{"time_ms": 50}"#)
        && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 100, 1000, r#", "mix": [{"path": "/one"}]"#));
    run_for(&mut e, 11 * SEC);
    let stale = metric(&e, h[1], "proxy", "cache_stale");
    let at = metric(&e, h[2], "web", "requests");
    let misses = metric(&e, h[1], "proxy", "cache_misses");
    // misses only while the first answer is on its way (50 ms at 100/s), renewals at most one per TTL
    t.check(&format!("stale answers {}, {} misses and only {} renewals", stale, misses, at), ok && stale > 30 && at <= 10_000 / 200 + 7 && misses <= 7);
    t.check("no answer waited for the backend but the first", metric(&e, h[0], "load", "max_us") >= 50_000 && metric(&e, h[0], "load", "p99_us") < 50_000);
    // Cache-Control of the backend wins: no-store keeps nothing
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "proxy", "proxy", r#"{"backends": [{"name": "web1", "addr": "10.0.0.3"}], "health": {"type": "none"}, "cache": {}}"#) && add(&mut e, h[2], "http-server", "web", r#"{"routes": [{"path": "*", "headers": {"Cache-Control": "no-store"}}]}"#) && add(&mut e, h[0], "http-load", "load", &open_load("http://10.0.0.2/", 100, 100, ""));
    run_for(&mut e, 2 * SEC);
    t.eq("no-store: every request a miss", (ok, metric(&e, h[1], "proxy", "cache_hits"), metric(&e, h[2], "web", "requests")), (true, 0, 100));
}

// ---------------------------------------------------------------- the pooler

fn dbpool_t(t: &mut T) {
    section("database pooler");
    let db_cfg = r#"{"tables": [{"name": "t", "rows": 10000, "row_bytes": 100, "columns": ["n"]}], "stmt_us": 2000, "pool_mb": 64}"#;
    let body = r#"{"sql":"SELECT * FROM t WHERE id = ?","params":[{"draw":true}]}"#;
    let load = |users: u32| format!(r#"{{"targets": ["http://10.0.0.2:6432/"], "users": {}, "timeout_ms": 60000, "mix": [{{"method": "POST", "path": "/query", "body": "{}"}}]}}"#, users, body.replace('"', "\\\""));
    let (mut e, h) = net(3);
    let _ = e.apply(Action::Resize { dev: h[2], hw: Some(hw(16, 1000)), downtime: 0 });
    let ok = add(&mut e, h[2], "db", "db", db_cfg) && add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "pool": 4}"#) && add(&mut e, h[0], "http-load", "load", &load(16));
    run_for(&mut e, 2 * SEC);
    let mut most = 0;
    let (r0, w0, n0, t0) = (metric(&e, h[1], "pool", "responses"), e.service::<DbPool>(h[1], "pool").map_or(0, |p| p.wait_sum), metric(&e, h[1], "pool", "waited"), e.now());
    let a0 = e.service::<DbPool>(h[1], "pool").map_or(0, |p| p.q_area);
    for _ in 0..40 {
        run_for(&mut e, 50 * MS);
        most = most.max(metric(&e, h[2], "db", "inflight"));
    }
    let span = (e.now() - t0) as f64 / SEC as f64;
    let x = (metric(&e, h[1], "pool", "responses") - r0) as f64 / span;
    let p = e.service::<DbPool>(h[1], "pool");
    let w = p.map_or(0.0, |p| (p.wait_sum - w0) as f64 / (metric(&e, h[1], "pool", "waited") - n0).max(1) as f64 / SEC as f64);
    let l = p.map_or(0.0, |p| (p.q_area - a0) as f64 / (e.now() - t0) as f64);
    t.check("pooler runs", ok);
    t.check(&format!("at the database at most the pool: {}, {} connections", most, metric(&e, h[1], "pool", "db_conns")), most <= 4 && most >= 3 && metric(&e, h[1], "pool", "db_conns") <= 4);
    check_near(t, "closed loop: W = (N - c) / X", w, 12.0 / x, 0.04);
    check_near(t, "Little: mean queue = lambda W = N - c", l, x * w, 0.04);
    check_near(t, "and that is N - c", l, 12.0, 0.04);
    // transaction mode: a silent transaction holds its connection
    let (mut e, h) = net(4);
    let _ = e.apply(Action::Resize { dev: h[2], hw: Some(hw(16, 1000)), downtime: 0 });
    let ok = add(&mut e, h[2], "db", "db", db_cfg) && add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "pool": 2}"#);
    let ok = ok && probe(&mut e, h[3], "silent", vec![(50 * MS, req("POST", "http://10.0.0.2:6432/query", "BEGIN", &[]))]);
    let ok = ok && add(&mut e, h[0], "http-load", "load", &load(8));
    run_for(&mut e, 2 * SEC);
    let a = metric(&e, h[1], "pool", "responses");
    run_for(&mut e, 2 * SEC);
    let held = (metric(&e, h[1], "pool", "responses") - a) as f64 / 2.0;
    t.check("the transaction holds a slot", ok && metric(&e, h[1], "pool", "txns") == 1);
    note(&format!("with one of two slots held: {} statements/s", held));
    let (mut e2, h2) = net(4);
    let _ = e2.apply(Action::Resize { dev: h2[2], hw: Some(hw(16, 1000)), downtime: 0 });
    let ok2 = add(&mut e2, h2[2], "db", "db", db_cfg) && add(&mut e2, h2[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "pool": 2}"#) && add(&mut e2, h2[0], "http-load", "load", &load(8));
    run_for(&mut e2, 2 * SEC);
    let a = metric(&e2, h2[1], "pool", "responses");
    run_for(&mut e2, 2 * SEC);
    let free = (metric(&e2, h2[1], "pool", "responses") - a) as f64 / 2.0;
    check_near(t, "a silent transaction halves a pool of 2", held, free / 2.0, 0.03);
    t.check("second pooler runs", ok2);
    // idle_txn_ms takes it back
    let (mut e, h) = net(4);
    let ok = add(&mut e, h[2], "db", "db", db_cfg) && add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "pool": 2, "idle_txn_ms": 500}"#) && probe(&mut e, h[3], "silent", vec![(50 * MS, req("POST", "http://10.0.0.2:6432/query", "BEGIN", &[]))]);
    run_for(&mut e, 300 * MS);
    let before = metric(&e, h[1], "pool", "txns");
    run_for(&mut e, 700 * MS);
    t.eq("idle_txn_ms ends it", (ok, before, metric(&e, h[1], "pool", "txns"), metric(&e, h[1], "pool", "idle_kills")), (true, 1, 0, 1));
    // statement mode refuses transactions
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[2], "db", "db", db_cfg) && add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "mode": "statement"}"#) && probe(&mut e, h[0], "probe", vec![(50 * MS, req("POST", "http://10.0.0.2:6432/query", "BEGIN", &[])), (60 * MS, req("POST", "http://10.0.0.2:6432/query", "SELECT * FROM t WHERE id = 1", &[]))]);
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "probe");
    t.eq("statement mode: BEGIN refused, a statement goes", (ok, g.iter().map(|x| x.status).collect::<Vec<_>>()), (true, vec![400, 200]));
    t.check("with its words", g.first().is_some_and(|x| x.body.contains("\"code\": \"pool_mode\"") && x.body.contains("use transaction mode")));
    // a whole transaction through the pooler
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[2], "db", "db", db_cfg) && add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}, "pool": 1}"#) && probe(&mut e, h[0], "probe", vec![(50 * MS, req("POST", "http://10.0.0.2:6432/query", "BEGIN", &[]))]);
    run_for(&mut e, 200 * MS);
    let txn = got(&e, h[0], "probe").first().and_then(|x| agent_core::json::parse(x.body.as_bytes()).ok()).and_then(|v| v.get("txn").and_then(|x| x.as_u64())).unwrap_or(0);
    let tx = format!("{}", txn);
    let ok = ok && probe(&mut e, h[0], "p2", vec![(250 * MS, req("POST", "http://10.0.0.2:6432/query", "UPDATE t SET n = 5 WHERE id = 1", &[("X-Txn", &tx)])), (260 * MS, req("POST", "http://10.0.0.2:6432/query", "SELECT * FROM t WHERE id = 2", &[])), (300 * MS, req("POST", "http://10.0.0.2:6432/query", "COMMIT", &[("X-Txn", &tx)]))]);
    run_for(&mut e, SEC);
    let g = got(&e, h[0], "p2");
    t.check(&format!("a transaction of pool 1: its statements go, others wait for COMMIT ({:?})", g.iter().map(|x| (x.i, x.status, x.end / MS)).collect::<Vec<_>>()), ok && txn > 0 && g.len() == 3 && g.iter().all(|x| x.status == 200) && g[1].end > g[2].end);
}

// ---------------------------------------------------------------- DNS round robin

fn dns_t(t: &mut T) {
    section("DNS round robin");
    let n = 7;
    let (mut e, h) = net(n);
    let ok = add(&mut e, h[0], "dns-server", "dns", r#"{"zones": [{"origin": "lab", "ttl": 60, "rotate": true, "records": ["web A 10.0.0.11", "web A 10.0.0.12", "web A 10.0.0.13", "db A 10.0.0.30"]}]}"#);
    for &c in &h[1..] {
        let _ = e.apply(Action::Resolver { dev: c, servers: Some(vec![addr(0)]), search: None });
    }
    // six clients one after the other: each address first twice
    let mut firsts: Vec<Ipv4> = Vec::new();
    for &c in &h[1..] {
        if let Ok(id) = e.lookup(c, "web.lab", netpkt::dns::TYPE_A) {
            run_for(&mut e, 100 * MS);
            if let Some(a) = e.lookup_state(c, id).and_then(|l| l.result.clone()).and_then(|r| r.ok()) {
                firsts.push(a.addrs.first().copied().unwrap_or(Ipv4::UNSPECIFIED));
            }
        }
    }
    let count = |a: Ipv4| firsts.iter().filter(|x| **x == a).count();
    t.check("rotation runs", ok && firsts.len() == 6);
    t.eq("over full cycles each address is first equally often", [11u8, 12, 13].map(|k| count(Ipv4([10, 0, 0, k]))), [2, 2, 2]);
    t.eq("in turn", firsts.iter().map(|a| a.0[3]).collect::<Vec<_>>(), vec![11, 12, 13, 11, 12, 13]);
    // the resolver keeps its first for the TTL: the same client asks again, the same answer
    if let Ok(id) = e.lookup(h[1], "web.lab", netpkt::dns::TYPE_A) {
        run_for(&mut e, 100 * MS);
        let again = e.lookup_state(h[1], id).and_then(|l| l.result.clone()).and_then(|r| r.ok()).and_then(|a| a.addrs.first().copied());
        t.eq("the resolver's cache keeps its order for the TTL", again, firsts.first().copied());
    }
    // four clients: 2, 1, 1 (why DNS round robin is not load balancing)
    t.eq("four clients spread unevenly", [11u8, 12, 13].map(|k| firsts[..4].iter().filter(|a| a.0[3] == k).count()), [2, 1, 1]);
}

// ---------------------------------------------------------------- live or restart

fn reconfig_t(t: &mut T) {
    section("live or restart");
    let base = format!(r#"{{"backends": {}, "health": {{"type": "none"}}, "retry": {{"max": 2}}, "pools": [{{"name": "api", "backends": [{{"name": "api1", "addr": "10.0.0.9"}}]}}], "routes": [{{"path": "/api/", "pool": "api"}}]}}"#, backends(2));
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "lb", "lb", &base);
    run_for(&mut e, 100 * MS);
    let live = |e: &Engine, c: &str| e.can_reconfigure(h[1], "lb", &cfg(c));
    let change = base.replace(r#""retry": {"max": 2}"#, r#""retry": {"max": 4, "budget": 0.1}, "limit": {"rate": 100}, "breaker": {}"#);
    t.check("policies change live", ok && live(&e, &change));
    let pools = base.replace(r#""name": "api","#, r#""name": "app","#).replace(r#""pool": "api""#, r#""pool": "app""#);
    t.check("pools do not", !live(&e, &pools));
    let r = e.apply(Action::ServiceConfig { dev: h[1], svc: String::from("lb"), config: cfg(&pools) });
    t.eq("and say so", r.err().map(|x| x.msg().text()), Some(String::from("Pools und Routen ändern sich nur mit einem Neustart.")));
    let r = e.apply(Action::ServiceConfig { dev: h[1], svc: String::from("lb"), config: cfg(&change) });
    t.check("a live change takes", r.is_ok() && lb(&e, h[1], "lb").is_some_and(|l| l.retry.as_ref().is_some_and(|r| r.max == 4) && l.limit.is_some()));
    // a proxy's certificate needs a restart
    let p1 = format!(r#"{{"backends": {}, "tls": {{"names": ["a.lab"]}}}}"#, backends(1));
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "proxy", "proxy", &p1);
    run_for(&mut e, 100 * MS);
    t.check("a new certificate needs a restart", ok && !e.can_reconfigure(h[1], "proxy", &cfg(&p1.replace("a.lab", "b.lab"))) && e.can_reconfigure(h[1], "proxy", &cfg(&p1.replace("}}", "}, \"handshake_us\": 900}"))));
    // the pooler: size live, the database not
    let (mut e, h) = net(3);
    let ok = add(&mut e, h[1], "dbpool", "pool", r#"{"db": {"addr": "10.0.0.3"}}"#);
    run_for(&mut e, 100 * MS);
    t.check("pool size live, database address not", ok && e.can_reconfigure(h[1], "pool", &cfg(r#"{"db": {"addr": "10.0.0.3"}, "pool": 20}"#)) && !e.can_reconfigure(h[1], "pool", &cfg(r#"{"db": {"addr": "10.0.0.4"}}"#)));
}

// ---------------------------------------------------------------- forms and problems

/// what a form writes when these fields are typed into it
fn after(kind: &str, base: &str, sets: &[(&str, &str)]) -> Result<String, String> {
    let b = cfg(base);
    let mut v = fill(kind, &b);
    for (k, x) in sets {
        if !v.set(kind, k, x) {
            return Err(format!("no field {}", k));
        }
    }
    merged(kind, &b, &v).map(|c| agent_core::json::stringify(&c).unwrap_or_default())
}

fn forms_t(t: &mut T) {
    section("forms against JSON");
    t.eq("lb: a retry policy", after("lb", "{}", &[("retry.max", "3"), ("retry.budget", "10")]), Ok(String::from(r#"{"retry":{"max":3,"budget":"10%"}}"#)));
    t.eq("lb: a breaker and a queue", after("lb", r#"{"max_conns": 8}"#, &[("queue", "20"), ("breaker.failures", "5")]), Ok(String::from(r#"{"max_conns":8,"queue":20,"breaker":{"failures":5}}"#)));
    t.eq("lb: emptied, the policy goes", after("lb", r#"{"breaker": {"failures": 5}, "port": 80}"#, &[("breaker.failures", "")]), Ok(String::from(r#"{"port":80}"#)));
    t.eq("lb: sticky by cookie, a rate limit", after("lb", "{}", &[("sticky.type", "cookie"), ("limit.rate", "2,5")]), Ok(String::from(r#"{"sticky":{"type":"cookie"},"limit":{"rate":2.5}}"#)));
    t.eq("lb: a budget reads as percent", fill("lb", &cfg(r#"{"retry": {"budget": 0.25}}"#)).texts.get(26).cloned(), Some(String::from("25")));
    t.eq("proxy: TLS and a cache", after("proxy", r#"{"backends": [{"addr": "10.0.0.3"}]}"#, &[("tls.issuer", "lab"), ("cache.ttl_ms", "2000")]), Ok(String::from(r#"{"backends":[{"addr":"10.0.0.3"}],"tls":{"issuer":"lab"},"cache":{"ttl_ms":2000}}"#)));
    t.eq("dbpool: its database and mode", after("dbpool", "{}", &[("db.addr", "10.0.0.30"), ("pool", "4"), ("mode", "statement")]), Ok(String::from(r#"{"db":{"addr":"10.0.0.30"},"pool":4,"mode":"statement"}"#)));
    t.eq("http-load: retries and cookies", after("http-load", r#"{"targets": ["http://10.0.0.2/"]}"#, &[("retry.max", "2"), ("cookies", "true")]), Ok(String::from(r#"{"targets":["http://10.0.0.2/"],"retry":{"max":2},"cookies":true}"#)));
    // what the forms write, the services take
    for (kind, text) in [("lb", r#"{"backends": [{"addr": "10.0.0.3"}], "retry": {"max": 3, "budget": "10%"}, "max_conns": 8, "queue": 20, "breaker": {"failures": 5}, "sticky": {"type": "cookie"}, "limit": {"rate": 2.5}}"#), ("proxy", r#"{"backends": [{"addr": "10.0.0.3"}], "tls": {"issuer": "lab"}, "cache": {"ttl_ms": 2000}}"#), ("dbpool", r#"{"db": {"addr": "10.0.0.30"}, "pool": 4, "mode": "statement"}"#)] {
        t.eq(&format!("{}: the form's JSON starts", kind), refused(kind, text), String::new());
    }
    // the defaults a form shows are the service's
    let d = jfn_sim::svcform::defaults("proxy").unwrap_or(Value::Null);
    t.eq("proxy: a handshake's default", d.get("handshake_us").map(|v| agent_core::json::stringify(v).unwrap_or_default()), Some(String::from("400")));
    t.eq("dbpool: its defaults", jfn_sim::svcform::defaults("dbpool").map(|v| (v.get("port").and_then(|x| x.as_u64()), v.get("pool").and_then(|x| x.as_u64()), v.get("mode").and_then(|x| x.as_str().map(String::from)))), Some((Some(6432), Some(10), Some(String::from("transaction")))));
}

fn problems_t(t: &mut T) {
    section("problems");
    let (mut e, h) = net(4);
    let _ = add(&mut e, h[1], "lb", "lb", &format!(r#"{{"backends": {}, "health": {{"type": "none"}}, "algorithm": "least-conn", "slow_start_ms": 5000, "outlier": {{"max_ejected": 0.3}}, "retry": {{"max": 2, "budget": 0}}}}"#, backends(2)));
    let _ = add(&mut e, h[2], "db", "db", r#"{"tables": [{"name": "t", "rows": 10}], "max_conns": 5}"#);
    let _ = add(&mut e, h[3], "dbpool", "a", r#"{"db": {"addr": "10.0.0.3"}, "pool": 8}"#);
    let _ = add(&mut e, h[3], "dbpool", "b", r#"{"db": {"addr": "10.0.0.9"}, "port": 6433}"#);
    let texts: Vec<String> = e.problems().into_iter().filter(|p| p.kind == jfn_sim::ProblemKind::TrafficSetup).map(|p| p.text).collect();
    for want in [
        "Dienst lb, Pool default: der langsame Start wirkt nur bei round-robin und weighted, nicht bei least-conn.",
        "Dienst lb, Pool default: bei 2 Backends darf mit max_ejected 300 ‰ keines ausgeschlossen werden.",
        "Dienst lb: ein Retry-Budget von 0 lässt keine Wiederholung zu.",
        "Dienst a: der Pooler öffnet bis zu 8 Verbindungen, die Datenbank auf 10.0.0.3 nimmt nur 5.",
        "Dienst b: auf 10.0.0.9:7100 läuft keine Datenbank.",
    ] {
        t.check(&format!("problem: {}", want), texts.iter().any(|x| x == want));
    }
    t.eq("and nothing else", texts.len(), 5);
}

// ---------------------------------------------------------------- determinism

fn determinism_t(t: &mut T) {
    section("determinism");
    let start = || {
        let (mut e, h) = net(5);
        let pc = format!(r#"{{"backends": {}, "health": {{"type": "http", "interval_ms": 200}}, "retry": {{"max": 2, "backoff_ms": 20, "jitter": "full", "budget": 0.3}}, "limit": {{"rate": 300, "burst": 30}}, "breaker": {{"failures": 4, "open_ms": 300}}, "outlier": {{"consecutive": 3, "base_ms": 400}}, "slow_start_ms": 500, "max_conns": 4, "queue": 20, "cache": {{"ttl_ms": 300}}}}"#, backends(3));
        let _ = add(&mut e, h[1], "proxy", "proxy", &pc);
        let _ = add(&mut e, h[2], "http-server", "web", r#"{"error_rate": 0.3, "time_ms": 5, "jitter_ms": 5}"#);
        let _ = add(&mut e, h[3], "http-server", "web", r#"{"time_ms": 3}"#);
        let _ = add(&mut e, h[4], "http-server", "web", r#"{"time_ms": 8}"#);
        let _ = add(&mut e, h[0], "http-load", "load", r#"{"targets": ["http://10.0.0.2/"], "mode": "open", "rps": 400, "arrival": "poisson", "mix": [{"path": "/k{rand:40}"}], "retry": {"max": 1, "backoff_ms": 30}}"#);
        (e, h[1])
    };
    let ((mut a, px), (mut b, _)) = (start(), start());
    run_for(&mut a, 3 * SEC);
    run_for(&mut b, 3 * SEC);
    t.eq("two runs: equal", digest(&a), digest(&b));
    let (mut c1, _) = start();
    run_for(&mut c1, 1500 * MS);
    let mut c2 = c1.clone();
    run_for(&mut c1, 1500 * MS);
    run_for(&mut c2, 1500 * MS);
    t.eq("a clone of a warm engine: equal", digest(&c1), digest(&c2));
    t.eq("and it equals the plain run", digest(&c1), digest(&a));
    let h1 = a.lb_view(px, "proxy").map(|v| (v.requests, v.retried, v.limited, v.cache_hits));
    note(&format!("the mixed run: requests, retries, limited, hits = {:?}", h1));
    // the study: 1 and 3 threads give the same bytes
    let Ok(exp) = Experiment::from_text(include_str!("../tests/experiments/overload.jfnex")) else {
        t.check("the overload study reads", false);
        return;
    };
    let Ok(p) = Project::from_text(include_str!("../tests/experiments/overload.jfnet")) else {
        t.check("its project reads", false);
        return;
    };
    match Plan::new(&exp, &p, "overload") {
        Ok(plan) => {
            let one = plan.run_all().table();
            let three = run_sweep(&plan, 3).table();
            t.eq("1 and 3 threads give the same bytes", one, three);
        }
        Err(m) => t.check(&format!("its plan: {:?}", m), false),
    }
}
