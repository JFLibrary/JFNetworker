// lab -- the console commands that show the lab's services (our own tools, our own words):
//
//   lbctl [status|routes] [-s NAME]          a load balancer: settings, numbers, every backend; with traffic
//                                            management (SA6) also its pools, policies and routes
//   proxyctl [status|routes|cache] [-s NAME] a reverse proxy: the same, its TLS and its cache
//   dbpoolctl [status] [-s NAME]             a database pooler: its slots, what waits, the open transactions
//   kvctl [status|ring|where KEY|get KEY|keys] [-s NAME]
//                                            a KV node: members as it sees them, the ring, the
//                                            replicas of a key and what each one holds, its
//                                            own entries
//   dbctl [status|tables|locks|explain SQL|replication|promote] [-s NAME]
//                                            a database: state and figures, its tables, the row locks
//                                            held and waited for, how a statement would go; its
//                                            replication (role, epoch, positions, lag, replicas); a
//                                            replica becomes primary (Engine::db_promote, SA5)
//   cachectl [status|keys] [-s NAME]         a cache: hits, misses, its entries
//   mqctl [status] [-s NAME]                 a queue: its topics
//   workerctl [status] [-s NAME]             a worker: its slots
//
// Without -s a command takes the first service of its kind on the device.  The texts are
// messages (jfn_model::msg) in the console's language (Engine::console_lang): German like the
// rest of the lab by default ("seit 6,512 s"), English with the rows of engine.tsv ("6.512 s");
// a table's head is one message with its cells split at '|' (head).  The database's own
// answers stay German in both: the SQL errors of dbctl explain, as its clients get them.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use base::hashring::hash64;
use jfn_model::msg;
use jfn_model::msg::{dur, phrase, Msg};

use crate::cache::Cache;
use crate::db::Db;
use crate::dbpool::DbPool;
use crate::disk::size_text;
use crate::kernel::{Engine, Ns};
use crate::kv::KvNode;
use crate::lb::{Admin, CheckKind, Lb, LbMode};
use crate::mq::{Queue, SlotState, Worker};
use crate::os::Service;

/// columns: every one as wide as its widest cell (+ 2), the last one not padded
pub(crate) fn table(rows: &[Vec<String>]) -> String {
    let n = rows.iter().map(|r| r.len()).max().unwrap_or(0);
    let mut w = alloc::vec![0usize; n];
    for r in rows {
        for (i, c) in r.iter().enumerate() {
            w[i] = w[i].max(c.chars().count());
        }
    }
    let mut s = String::new();
    for r in rows {
        let mut line = String::new();
        for (i, c) in r.iter().enumerate() {
            line.push_str(c);
            if i + 1 < r.len() {
                for _ in c.chars().count()..w[i] + 2 {
                    line.push(' ');
                }
            }
        }
        s.push_str(line.trim_end());
        s.push('\n');
    }
    s
}

pub(crate) fn row(cells: &[&str]) -> Vec<String> {
    cells.iter().map(|c| String::from(*c)).collect()
}

fn opt_ns(v: Option<Ns>) -> Msg {
    match v {
        Some(x) => dur(x as i64),
        None => Msg::plain("-"),
    }
}

/// "80,0 %" from per mille
fn pm(v: u64) -> Msg {
    msg!("{},{} %", v / 10, v % 10)
}

/// "33,4 %" from parts per million
fn ppm(v: u32) -> Msg {
    msg!("{},{} %", v / 10_000, v % 10_000 / 1000)
}

/// a port, "(nicht offen)" when nothing listens on it
fn port_msg(port: u16, listening: bool) -> Msg {
    if listening { Msg::plain(format!("{}", port)) } else { msg!("{} (nicht offen)", port) }
}

fn lb_help() -> Vec<(Msg, Msg)> {
    // (lbctl routes is in the man page: this usage stays as old reports have it)
    alloc::vec![(phrase("lbctl [status] [-s DIENST]"), phrase("zeigt den Loadbalancer: Einstellungen, Zahlen und jedes Backend"))]
}

fn proxy_help() -> Vec<(Msg, Msg)> {
    alloc::vec![
        (phrase("proxyctl [status] [-s DIENST]"), phrase("zeigt den Reverse Proxy: TLS, Cache, Regeln, Zahlen und jedes Backend")),
        (Msg::plain("proxyctl routes"), phrase("seine Pools mit ihren Regeln und seine Routen")),
        (Msg::plain("proxyctl cache"), phrase("was sein Cache hält")),
    ]
}

fn dbpool_help() -> Vec<(Msg, Msg)> {
    alloc::vec![(phrase("dbpoolctl [status] [-s DIENST]"), phrase("zeigt den DB-Pooler: belegte Verbindungen, Wartende, offene Transaktionen"))]
}

fn kv_help() -> Vec<(Msg, Msg)> {
    alloc::vec![
        (phrase("kvctl [status] [-s DIENST]"), phrase("zeigt den KV-Knoten: Mitglieder, wie er sie sieht, und seine Zahlen")),
        (Msg::plain("kvctl ring"), phrase("die Tokens des Rings und ihre Knoten")),
        (phrase("kvctl where SCHLÜSSEL"), phrase("die Replikate eines Schlüssels und was jedes davon hält")),
        (phrase("kvctl get SCHLÜSSEL"), phrase("der Eintrag dieses Knotens")),
        (Msg::plain("kvctl keys"), phrase("die Schlüssel dieses Knotens")),
    ]
}

fn db_help() -> Vec<(Msg, Msg)> {
    alloc::vec![
        (phrase("dbctl [status] [-s DIENST]"), phrase("zeigt die Datenbank: Zustand, Puffer, Log und Zahlen")),
        (Msg::plain("dbctl tables"), phrase("ihre Tabellen: Zeilen, geänderte Zeilen, Seiten, Indizes")),
        (Msg::plain("dbctl locks"), phrase("die Zeilensperren, wer sie hält und wie viele warten")),
        (phrase("dbctl explain ANWEISUNG"), phrase("wie eine Anweisung liefe: Index oder alle Seiten, Zeilen, Seiten, warum")),
        (Msg::plain("dbctl replication"), phrase("die Replikation: Rolle, Epoche, Positionen, Rückstand, die Replikate")),
        (Msg::plain("dbctl promote"), phrase("ein Replikat wird Primary, nachdem es angewendet hat, was es schon hat")),
    ]
}

fn cache_help() -> Vec<(Msg, Msg)> {
    alloc::vec![(phrase("cachectl [status|keys] [-s DIENST]"), phrase("zeigt den Cache: Treffer, Fehlschläge, Einträge"))]
}

fn mq_help() -> Vec<(Msg, Msg)> {
    alloc::vec![(phrase("mqctl [status] [-s DIENST]"), phrase("zeigt die Queue mit ihren Topics"))]
}

fn worker_help() -> Vec<(Msg, Msg)> {
    alloc::vec![(phrase("workerctl [status] [-s DIENST]"), phrase("zeigt den Worker und seine Plätze"))]
}

impl Engine {
    fn lab_err(&mut self, ni: usize, text: &str) -> Result<(), String> {
        self.err_out(ni, text);
        Err(String::from(text.lines().next().unwrap_or("")))
    }

    /// a refusal of a lab command as a line in the console's language
    fn lab_no(&mut self, ni: usize, m: Msg) -> Result<(), String> {
        let t = format!("{}\n", self.tx(&m));
        self.lab_err(ni, &t)
    }

    /// words a lab command does not know, then its usage
    fn lab_unknown(&mut self, ni: usize, tool: &str, rest: &[String], help: &[(Msg, Msg)]) -> Result<(), String> {
        let t = format!("{}\n{}", self.tx(&msg!("{}: {} kenne ich nicht", tool, rest.join(" "))), self.help_text(help));
        self.lab_err(ni, &t)
    }

    /// a command's usage: a line per form and what it does, the second column three spaces
    /// after the widest form
    pub(crate) fn help_text(&self, rows: &[(Msg, Msg)]) -> String {
        let rows: Vec<(String, String)> = rows.iter().map(|(a, b)| (self.tx(a), self.tx(b))).collect();
        let w = rows.iter().map(|r| r.0.chars().count()).max().unwrap_or(0) + 3;
        let mut s = String::new();
        for (a, b) in rows {
            s.push_str(&a);
            for _ in a.chars().count()..w {
                s.push(' ');
            }
            s.push_str(&b);
            s.push('\n');
        }
        s
    }

    /// a table's head in the console's language: one message, its cells split at '|'
    pub(crate) fn head(&self, m: Msg) -> Vec<String> {
        self.tx(&m).split('|').map(String::from).collect()
    }

    /// "-s NAME" and the other words of a command; Err: the words were not understood
    fn lab_args(args: &[&str]) -> Result<(Option<String>, Vec<String>), ()> {
        let mut svc = None;
        let mut rest = Vec::new();
        let mut i = 0;
        while i < args.len() {
            match args[i] {
                "-s" | "--service" => {
                    i += 1;
                    svc = Some(String::from(*args.get(i).ok_or(())?));
                }
                "-h" | "--help" | "help" => return Err(()),
                a => rest.push(String::from(a)),
            }
            i += 1;
        }
        Ok((svc, rest))
    }

    /// the name of the service of type T a command shows
    fn lab_pick<T: Service + 'static>(&self, ni: usize, name: Option<&str>) -> Option<String> {
        let dev = self.dev_of(ni);
        match name {
            Some(n) => self.service::<T>(dev, n).map(|_| String::from(n)),
            None => self.service_ids(dev).into_iter().map(|s| s.1).find(|n| self.service::<T>(dev, n).is_some()),
        }
    }

    // ---------------------------------------------------------------- lbctl

    /// a balancer or proxy of the device by its kind: its name
    fn lab_pick_lb(&self, ni: usize, name: Option<&str>, kind: &str) -> Option<String> {
        let dev = self.dev_of(ni);
        let of = |n: &str| self.service::<Lb>(dev, n).is_some_and(|l| l.kind() == kind);
        match name {
            Some(n) => self.service::<Lb>(dev, n).map(|_| String::from(n)),
            None => self.service_ids(dev).into_iter().map(|s| s.1).find(|n| of(n)),
        }
    }

    pub(crate) fn sh_lbctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&lb_help());
            self.out(ni, &h);
            return Ok(());
        };
        if rest.iter().any(|w| w != "status" && w != "routes") {
            return self.lab_unknown(ni, "lbctl", &rest, &lb_help());
        }
        let Some(name) = self.lab_pick_lb(ni, svc.as_deref(), "lb").or_else(|| self.lab_pick::<Lb>(ni, svc.as_deref())) else {
            return self.lab_no(ni, msg!("lbctl: auf diesem Gerät läuft kein Loadbalancer"));
        };
        let dev = self.dev_of(ni);
        if rest.iter().any(|w| w == "routes") {
            let t = self.lb_routes_text(dev, &name);
            self.out(ni, &t);
            return Ok(());
        }
        let t = self.lb_text(dev, &name);
        self.out(ni, &t);
        Ok(())
    }

    /// what lbctl and proxyctl show of a balancer or proxy
    fn lb_text(&self, dev: u32, name: &str) -> String {
        let Some(v) = self.lb_view(dev, name) else {
            return String::new();
        };
        let lb = self.service::<Lb>(dev, name);
        let health = match lb.map(|l| l.health.clone()) {
            Some(h) if h.kind != CheckKind::None => {
                let how = if h.kind == CheckKind::Http { format!("http GET {}", h.path) } else { String::from("tcp") };
                msg!("Health Checks {} alle {} (Timeout {}, verfügbar nach {}, ausgefallen nach {} Prüfungen)", how, dur(h.interval as i64), dur(h.timeout as i64), h.rise, h.fall)
            }
            _ => phrase("ohne Health Checks"),
        };
        let http = v.mode == LbMode::Http;
        let head = if v.proxy {
            msg!("Reverse Proxy {}: {} auf Port {}, Verfahren {}, {}", name, if v.tls { "HTTPS" } else { "HTTP" }, port_msg(v.port, v.listening), v.algo.name(), health)
        } else {
            msg!("Loadbalancer {}: {} auf Port {}, Verfahren {}, {}", name, if http { "HTTP" } else { "TCP" }, port_msg(v.port, v.listening), v.algo.name(), health)
        };
        let counts = if http {
            msg!("Anfragen {}, beendet {}, offen {}, Fehler {} (ohne Backend {}), Wiederholungen {}", v.requests, v.responses, v.active, v.errors, v.no_backend, v.retried)
        } else {
            msg!("Verbindungen {}, beendet {}, offen {}, Fehler {} (ohne Backend {}), Wiederholungen {}", v.requests, v.responses, v.active, v.errors, v.no_backend, v.retried)
        };
        let lat = if http {
            msg!("Antwortzeit p50 {}, p95 {}, p99 {}", opt_ns(v.p50), opt_ns(v.p95), opt_ns(v.p99))
        } else {
            msg!("Verbindungsaufbau p50 {}, p95 {}, p99 {}", opt_ns(v.p50), opt_ns(v.p95), opt_ns(v.p99))
        };
        let mut s = format!("{}\n{}\n{}\n", self.tx(&head), self.tx(&counts), self.tx(&lat));
        if v.ext {
            // traffic management (SA6): what it refused and why, the proxy's TLS and cache
            let refused = msg!("Abgewiesen: Rate-Limit {}, Warteschlange voll {}, zu lange gewartet {}, Breaker offen {}; Retry-Budget verbraucht {}, Backends ausgeschlossen {}", v.limited, v.pools.iter().map(|p| p.shed).sum::<u64>(), v.pools.iter().map(|p| p.queue_timeouts).sum::<u64>(), v.pools.iter().map(|p| p.fail_fast).sum::<u64>(), v.budget_denied, v.ejections);
            s.push_str(&self.tx(&refused));
            s.push('\n');
            if v.proxy {
                let tls = if v.tls { msg!("TLS: {} volle Handshakes, {} wieder aufgenommen, {} CPU", v.handshakes, v.resumed, dur(v.tls_cpu as i64)) } else { msg!("TLS: aus") };
                s.push_str(&self.tx(&tls));
                s.push('\n');
                let hits = v.cache_hits + v.cache_stale;
                let all = hits + v.cache_misses;
                let cache = msg!("Cache: {} Einträge, Treffer {} (davon abgelaufen {}), Fehlschläge {} ({} Treffer), gespart {}", v.cache_entries, hits, v.cache_stale, v.cache_misses, pm(if all == 0 { 0 } else { hits * 1000 / all }), size_text(v.bytes_saved));
                s.push_str(&self.tx(&cache));
                s.push('\n');
            }
        }
        s.push('\n');
        let mut rows = alloc::vec![if v.ext { self.head(phrase("Backend|Pool|Adresse|Gewicht|Zustand|Breaker|seit|offen|gesamt|Fehler|p99")) } else { self.head(phrase("Backend|Adresse|Gewicht|Zustand|seit|offen|gesamt|Fehler|p99")) }];
        for b in &v.backends {
            let state = match b.admin {
                Admin::Active if b.ejected_until.is_some() => phrase("ausgeschlossen"),
                Admin::Active if b.up => phrase("verfügbar"),
                Admin::Active => phrase("ausgefallen"),
                Admin::Draining { .. } => phrase("leert sich"),
                Admin::Disabled => phrase("abgeschaltet"),
                Admin::Removed => phrase("entfernt"),
            };
            let at = if b.addr.is_v4() { format!("{}:{}", b.addr, b.port) } else { format!("[{}]:{}", b.addr, b.port) };
            if v.ext {
                // the weight now: slow start shows as a fraction
                let w = if b.weight_pm % 1000 == 0 { format!("{}", b.weight_pm / 1000) } else { format!("{}.{:03}", b.weight_pm / 1000, b.weight_pm % 1000) };
                rows.push(alloc::vec![b.name.clone(), b.pool.clone(), at, w, self.tx(&state), self.tx(&b.breaker.word()), self.tx(&dur(b.since as i64)), format!("{}", b.active), format!("{}", b.total), format!("{}", b.errors), self.tx(&opt_ns(b.p99))]);
            } else {
                rows.push(alloc::vec![b.name.clone(), at, format!("{}", b.weight), self.tx(&state), self.tx(&dur(b.since as i64)), format!("{}", b.active), format!("{}", b.total), format!("{}", b.errors), self.tx(&opt_ns(b.p99))]);
            }
        }
        s.push_str(&table(&rows));
        s
    }

    /// the pools with their policies and the routes of a balancer or proxy
    fn lb_routes_text(&self, dev: u32, name: &str) -> String {
        let Some(v) = self.lb_view(dev, name) else {
            return String::new();
        };
        let mut rows = alloc::vec![self.head(phrase("Pool|Verfahren|Regeln|wartend|höchstens|abgewiesen|zu lange|p99 Warten"))];
        for p in &v.pools {
            let pol = if p.policies.is_empty() { String::from("-") } else { p.policies.clone() };
            rows.push(alloc::vec![p.name.clone(), String::from(p.algo.name()), pol, format!("{}", p.waiting), format!("{}", p.max_waiting), format!("{}", p.shed), format!("{}", p.queue_timeouts), self.tx(&opt_ns(p.wait_p99))]);
        }
        let mut s = table(&rows);
        if v.routes.is_empty() {
            s.push_str(&self.tx(&msg!("Keine Routen: alles geht an den Pool default.")));
            s.push('\n');
            return s;
        }
        s.push('\n');
        let mut rows = alloc::vec![self.head(phrase("Route|Pool|Anfragen|begrenzt|Umschreiben|Cache"))];
        for r in &v.routes {
            let rw = if r.rewrite.is_empty() { String::from("-") } else { r.rewrite.clone() };
            rows.push(alloc::vec![r.label.clone(), r.pool.clone(), format!("{}", r.requests), format!("{}", r.limited), rw, self.tx(&if r.cache { phrase("ja") } else { phrase("nein") })]);
        }
        s.push_str(&table(&rows));
        s
    }

    // ---------------------------------------------------------------- proxyctl

    pub(crate) fn sh_proxyctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&proxy_help());
            self.out(ni, &h);
            return Ok(());
        };
        let Some(name) = self.lab_pick_lb(ni, svc.as_deref(), "proxy") else {
            return self.lab_no(ni, msg!("proxyctl: auf diesem Gerät läuft kein Reverse Proxy"));
        };
        let dev = self.dev_of(ni);
        let now = self.now;
        let words: Vec<&str> = rest.iter().map(String::as_str).collect();
        let text = match words.as_slice() {
            [] | ["status"] => self.lb_text(dev, &name),
            ["routes"] => self.lb_routes_text(dev, &name),
            ["cache"] => {
                let mut rows = alloc::vec![self.head(phrase("Schlüssel|Status|Bytes|Alter|frisch bis"))];
                if let Some(l) = self.service::<Lb>(dev, &name) {
                    for (k, e) in l.cached().into_iter().rev().take(100) {
                        rows.push(alloc::vec![k, format!("{}", e.status), format!("{}", e.body.len()), self.tx(&dur(now.saturating_sub(e.stored) as i64)), self.tx(&dur(e.expires as i64))]);
                    }
                }
                table(&rows)
            }
            _ => return self.lab_unknown(ni, "proxyctl", &rest, &proxy_help()),
        };
        self.out(ni, &text);
        Ok(())
    }

    // ---------------------------------------------------------------- dbpoolctl

    pub(crate) fn sh_dbpoolctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&dbpool_help());
            self.out(ni, &h);
            return Ok(());
        };
        if rest.iter().any(|w| w != "status") {
            return self.lab_unknown(ni, "dbpoolctl", &rest, &dbpool_help());
        }
        let Some(name) = self.lab_pick::<DbPool>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("dbpoolctl: auf diesem Gerät läuft kein DB-Pooler"));
        };
        let dev = self.dev_of(ni);
        let Some(v) = self.dbpool_view(dev, &name) else {
            return Ok(());
        };
        let lines = [
            msg!("DB-Pooler {}: Port {} vor {}:{}, Verbindung je {}, höchstens {} Verbindungen", name, port_msg(v.port, v.listening), v.db.0, v.db.1, if v.mode == crate::dbpool::PoolMode::Transaction { phrase("Transaktion") } else { phrase("Anweisung") }, v.size),
            msg!("Belegt {} (höchstens {}), Verbindungen zur Datenbank {}, wartend {} (höchstens {}), offene Transaktionen {}", v.in_use, v.max_in_use, v.db_conns, v.waiting, v.max_waiting, v.txns.len()),
            msg!("Anweisungen {}, gewartet {}, Warteschlange voll {}, zu lange gewartet {}, im Modus abgelehnt {}, stille Transaktionen beendet {}", v.requests, v.waited, v.refused, v.queue_timeouts, v.mode_refused, v.idle_kills),
            msg!("Warten p50 {}, p99 {}; Antwortzeit p99 {}", opt_ns(v.wait_p50), opt_ns(v.wait_p99), opt_ns(v.p99)),
        ];
        let mut s = String::new();
        for l in lines {
            s.push_str(&self.tx(&l));
            s.push('\n');
        }
        if !v.txns.is_empty() {
            s.push('\n');
            let mut rows = alloc::vec![self.head(phrase("Transaktion|offen seit|still seit|in Arbeit"))];
            for (id, open, silent, inflight) in &v.txns {
                rows.push(alloc::vec![format!("{}", id), self.tx(&dur(*open as i64)), self.tx(&dur(*silent as i64)), format!("{}", inflight)]);
            }
            s.push_str(&table(&rows));
        }
        self.out(ni, &s);
        Ok(())
    }

    // ---------------------------------------------------------------- kvctl

    pub(crate) fn sh_kvctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&kv_help());
            self.out(ni, &h);
            return Ok(());
        };
        let Some(name) = self.lab_pick::<KvNode>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("kvctl: auf diesem Gerät läuft kein KV-Knoten"));
        };
        let dev = self.dev_of(ni);
        let words: Vec<&str> = rest.iter().map(String::as_str).collect();
        let text = match words.as_slice() {
            [] | ["status"] => self.kv_status(dev, &name),
            ["ring"] => {
                let mut rows = alloc::vec![self.head(phrase("Token|Knoten"))];
                for (t, m) in self.kv_ring(dev, &name) {
                    rows.push(alloc::vec![format!("0x{:016x}", t), format!("{}", m)]);
                }
                table(&rows)
            }
            ["where", key] => self.kv_where_text(dev, &name, key),
            ["get", key] => {
                let m = match self.service::<KvNode>(dev, &name).and_then(|k| k.entry(key)) {
                    Some(e) if e.deleted => msg!("{}: gelöscht (Version {})", key, e.ver.text()),
                    Some(e) => msg!("{} = {} (Version {})", key, String::from_utf8_lossy(&e.value), e.ver.text()),
                    None => msg!("{}: nicht auf diesem Knoten", key),
                };
                format!("{}\n", self.tx(&m))
            }
            ["keys"] => {
                let mut rows = alloc::vec![self.head(phrase("Schlüssel|Version|Bytes"))];
                let mut more = 0usize;
                if let Some(k) = self.service::<KvNode>(dev, &name) {
                    for (i, (key, e)) in k.stored().enumerate() {
                        if i >= 100 {
                            more += 1;
                            continue;
                        }
                        let ver = if e.deleted { self.tx(&msg!("{} gelöscht", e.ver.text())) } else { e.ver.text() };
                        rows.push(alloc::vec![key.clone(), ver, format!("{}", e.value.len())]);
                    }
                }
                let mut t = table(&rows);
                if more > 0 {
                    t.push_str(&self.tx(&msg!("... und {} weitere", more)));
                    t.push('\n');
                }
                t
            }
            _ => return self.lab_unknown(ni, "kvctl", &rest, &kv_help()),
        };
        self.out(ni, &text);
        Ok(())
    }

    fn kv_status(&self, dev: u32, name: &str) -> String {
        let (Some(v), Some(k)) = (self.kv_view(dev, name), self.service::<KvNode>(dev, name)) else {
            return String::new();
        };
        let me = match v.me {
            Some(a) => Msg::plain(format!("{}", a)),
            None => phrase("nur Koordinator"),
        };
        let mut s = self.tx(&msg!("KV-Knoten {}: {}, Port {}, Heartbeats UDP {} alle {} (ausgefallen nach {})", name, me, v.port, k.hb_port, dur(k.beat as i64), dur(k.down_after as i64)));
        s.push('\n');
        let strong = (v.w + v.r) as usize > v.rf;
        let rr = if v.read_repair { phrase("an") } else { phrase("aus") };
        s.push_str(&self.tx(&msg!("Ring: {} Knoten mit je {} vnodes, RF {}, Konsistenz {} (W {}, R {}{}), Read Repair {}", v.members.len(), v.vnodes, v.rf, v.consistency.name(), v.w, v.r, if strong { ", W + R > RF" } else { ", W + R \u{2264} RF" }, rr)));
        s.push_str("\n\n");
        let mut rows = alloc::vec![self.head(phrase("Knoten|Zustand|seit|Ring-Anteil"))];
        for m in &v.members {
            let share = v.shares.iter().find(|x| x.0 == m.addr).map(|x| self.tx(&ppm(x.1))).unwrap_or_default();
            let (state, since) = if Some(m.addr) == v.me {
                (phrase("dieser Knoten"), String::from("-"))
            } else if m.up {
                (phrase("erreichbar"), self.tx(&dur(m.since as i64)))
            } else {
                (phrase("ausgefallen"), self.tx(&dur(m.since as i64)))
            };
            rows.push(alloc::vec![format!("{}", m.addr), self.tx(&state), since, share]);
        }
        s.push_str(&table(&rows));
        let disk = self.node_index(dev).and_then(|ni| self.os(ni)).map(|o| o.disk.msg()).unwrap_or_default();
        let st = v.stats;
        let lines = [
            msg!("Schlüssel {} ({}), Disk {}", v.keys, size_text(v.bytes), disk),
            msg!("Anfragen {}: gelesen {}, geschrieben {}, nicht gefunden {}, nicht verfügbar {}, Disk voll {}", st.requests, st.reads, st.writes, st.not_found, st.unavailable, st.full),
            msg!("Weitergereicht {}, selbst geführt {}; Replikate geschrieben {}, gelesen {}", st.forwards, st.led, st.replica_writes, st.replica_reads),
            msg!("Veraltete Antworten {}, Read Repair gesendet {}, erhalten {}; Ausfälle erkannt {}", st.stale_reads, st.read_repairs, st.repaired, st.member_downs),
            msg!("Antwortzeit p50 {}, p95 {}, p99 {}", opt_ns(v.p50), opt_ns(v.p95), opt_ns(v.p99)),
        ];
        s.push('\n');
        for l in lines {
            s.push_str(&self.tx(&l));
            s.push('\n');
        }
        s
    }

    fn kv_where_text(&self, dev: u32, name: &str, key: &str) -> String {
        let reps = self.kv_where(dev, name, key);
        let rf = self.service::<KvNode>(dev, name).map(|k| k.rf).unwrap_or(0);
        let mut s = self.tx(&msg!("Schlüssel {}: Hash {}, Replikate (RF {}):", key, format!("0x{:016x}", hash64(key.as_bytes())), rf));
        s.push('\n');
        let mut rows = Vec::new();
        for r in reps {
            let held = match &r.entry {
                Some(e) if e.deleted => msg!("gelöscht, Version {}", e.ver.text()),
                Some(e) => msg!("Version {}, {} Bytes", e.ver.text(), e.value.len()),
                None if r.dev.is_none() => phrase("kein KV-Knoten unter der Adresse"),
                None => phrase("nichts gespeichert"),
            };
            let up = if r.up { phrase("erreichbar") } else { phrase("ausgefallen") };
            rows.push(alloc::vec![format!("  {}", r.addr), String::from(if r.leader { "Leader" } else { "Follower" }), self.tx(&up), self.tx(&held)]);
        }
        s.push_str(&table(&rows));
        s
    }

    // ---------------------------------------------------------------- dbctl

    pub(crate) fn sh_dbctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&db_help());
            self.out(ni, &h);
            return Ok(());
        };
        let Some(name) = self.lab_pick::<Db>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("dbctl: auf diesem Gerät läuft keine Datenbank"));
        };
        let dev = self.dev_of(ni);
        let words: Vec<&str> = rest.iter().map(String::as_str).collect();
        let text = match words.as_slice() {
            [] | ["status"] => self.db_status(dev, &name),
            ["tables"] => {
                let mut rows = alloc::vec![self.head(phrase("Tabelle|Schlüssel|Zeilen|geändert|Seiten|im Puffer|Baumtiefe|Indizes|Spalten"))];
                if let Some(d) = self.service::<Db>(dev, &name) {
                    for t in d.tables() {
                        rows.push(alloc::vec![t.name.clone(), t.key.clone(), format!("{}", t.rows), format!("{}", t.touched), format!("{}", t.heap_pages), format!("{}", t.held_pages), format!("{}", t.depth), if t.indexes.is_empty() { String::from("-") } else { t.indexes.join(", ") }, t.columns.join(", ")]);
                    }
                }
                table(&rows)
            }
            ["locks"] => {
                let list = self.service::<Db>(dev, &name).map_or(Vec::new(), |d| d.lock_list());
                if list.is_empty() {
                    format!("{}\n", self.tx(&phrase("keine Zeilensperren")))
                } else {
                    let mut rows = alloc::vec![self.head(phrase("Tabelle|Zeile|Transaktion|wartend"))];
                    for l in list {
                        rows.push(alloc::vec![l.table.clone(), format!("{}", l.key), format!("{}", l.holder), format!("{}", l.waiting)]);
                    }
                    table(&rows)
                }
            }
            ["explain", sql @ ..] if !sql.is_empty() => match self.service::<Db>(dev, &name).map(|d| d.explain_in(&sql.join(" "), self.console_lang)) {
                Some(Ok(t)) => format!("{}\n", t),
                Some(Err(e)) => return self.lab_err(ni, &format!("dbctl: {}\n", e.line())),
                None => String::new(),
            },
            ["replication"] => self.db_repl_text(dev, &name),
            ["promote"] => match self.db_promote(dev, &name) {
                Ok(()) => format!("{}\n", self.tx(&msg!("{} wird Primary: wendet zuerst an, was es schon hat, dann hören die anderen Mitglieder davon.", name))),
                Err(e) => {
                    let t = format!("dbctl: {}\n", self.tx(&e.msg()));
                    return self.lab_err(ni, &t);
                }
            },
            _ => return self.lab_unknown(ni, "dbctl", &rest, &db_help()),
        };
        self.out(ni, &text);
        Ok(())
    }

    fn db_status(&self, dev: u32, name: &str) -> String {
        let Some(v) = self.db_view(dev, name) else {
            return String::new();
        };
        let st = v.stats;
        let mut lines = alloc::vec![msg!("Datenbank {}: Port {}, {} von höchstens {} Verbindungen, {} Transaktionen offen, {} Anweisungen in Arbeit", name, port_msg(v.port, v.listening), v.conns, v.max_conns, v.txns, v.running)];
        if !v.metered {
            lines.push(phrase("Dieses Gerät simuliert keine Hardware: die CPU kostet nichts, Platte und Sperren kosten weiter Zeit."));
        }
        if v.recovering {
            lines.push(phrase("Liest das Log seit dem letzten Prüfpunkt: nimmt noch keine Anweisungen an."));
        }
        if let Some((need, cap)) = v.no_disk {
            lines.push(msg!("Die Tabellen brauchen {}, die Platte hat {}.", size_text(need), size_text(cap)));
        }
        lines.push(if v.pool_rate > 1 {
            msg!("Puffer: {} von {}, Treffer {}, zählt jede {}. Seite", size_text(v.pool_used), size_text(v.pool_bytes), pm(v.hit_pm), v.pool_rate)
        } else {
            msg!("Puffer: {} von {}, Treffer {}", size_text(v.pool_used), size_text(v.pool_bytes), pm(v.hit_pm))
        });
        lines.push(msg!("Log: Stand {}, auf der Platte {}, Prüfpunkt {}; Schreiben {}, Durability {}, Isolation {}", v.lsn, v.flushed, v.ckpt, st.wal_flushes, if v.durable { "full" } else { "relaxed" }, v.iso.name()));
        lines.push(msg!("Anweisungen {}: gelesen {}, eingefügt {}, geändert {}, gelöscht {}; Commits {}, Rollbacks {}", st.statements, st.selects, st.inserts, st.updates, st.deletes, st.commits, st.rollbacks));
        lines.push(msg!("Fehler {}: Syntax {}, Konflikte {}, Verklemmungen {}, Sperren zu spät {}, still abgebrochen {}, abgewiesen {}", st.errors, st.syntax_errors, st.conflicts, st.deadlocks, st.lock_timeouts, st.idle_aborts, st.refused));
        lines.push(msg!("Sperren gewartet {}; Zeilen angesehen {}, Seiten gelesen {}; geänderte Zeilen {}, vergessen {}", st.lock_waits, st.rows_examined, st.pages_read, v.touched, st.forgotten));
        lines.push(msg!("Nach Abstürzen: {} Mal, zuletzt {} ms, verlorene Commits {}", st.recoveries, st.recovery_ms, st.lost_commits));
        lines.push(msg!("Antwortzeit p50 {}, p95 {}, p99 {}", opt_ns(v.p50), opt_ns(v.p95), opt_ns(v.p99)));
        let mut s = String::new();
        for l in lines {
            s.push_str(&self.tx(&l));
            s.push('\n');
        }
        s
    }

    fn db_repl_text(&self, dev: u32, name: &str) -> String {
        let (Some(v), Some(d)) = (self.db_repl_view(dev, name), self.db_view(dev, name)) else {
            return format!("{}\n", self.tx(&phrase("Diese Datenbank hat keine Replikation.")));
        };
        let st = v.stats;
        let addr = |a: Option<netpkt::Ipv4>| format!("{}", a.unwrap_or(netpkt::Ipv4::UNSPECIFIED));
        let mut lines = Vec::new();
        let mut table_rows: Vec<Vec<String>> = Vec::new();
        match v.role {
            crate::db::ReplRole::Primary => {
                lines.push(msg!("Primary in Epoche {}, Modus {} (wartet auf {} Replikate), Log-Port {}", v.epoch, v.mode.name(), v.sync_count, v.port));
                if v.checking {
                    lines.push(phrase("Fragt nach einem Neustart erst seine Mitglieder, ob es einen neueren Primary gibt: nimmt bis dahin keine Schreibzugriffe an."));
                }
                if v.fallback {
                    lines.push(phrase("Fällt gerade auf async zurück: kein Replikat hat rechtzeitig bestätigt."));
                }
                if v.rival.is_some() {
                    lines.push(msg!("Split Brain: auch {} nimmt Schreibzugriffe an.", addr(v.rival)));
                }
                lines.push(msg!("Log: Stand {}, auf der Platte {}", d.lsn, d.flushed));
                if v.followers.is_empty() {
                    lines.push(phrase("Kein Replikat verbunden."));
                } else {
                    table_rows.push(self.head(phrase("Replikat|empfangen|geschrieben|angewendet|Rückstand|Bytes|Zustand")));
                    for f in &v.followers {
                        let state = if f.copying { phrase("kopiert") } else { phrase("folgt") };
                        table_rows.push(alloc::vec![format!("{}", f.addr), format!("{}", f.received), format!("{}", f.flushed), format!("{}", f.applied), format!("{} ms", f.lag_ms), format!("{}", f.lag_bytes), self.tx(&state)]);
                    }
                }
            }
            crate::db::ReplRole::Replica => {
                lines.push(msg!("Replikat von {} in Epoche {}, Log-Port {}", addr(v.primary), v.epoch, v.port));
                if v.promoting {
                    lines.push(phrase("Wird Primary: wendet zuerst an, was es schon hat."));
                } else if let Some(p) = v.copying {
                    lines.push(msg!("Bekommt eine Kopie der Daten ({} %): beantwortet bis dahin keine Lesezugriffe.", p));
                } else if !v.connected {
                    lines.push(phrase("Erreicht seinen Primary nicht: versucht es weiter."));
                }
                if v.fenced {
                    lines.push(phrase("War Primary und wurde abgegrenzt: ein neuerer Primary ist da."));
                }
                lines.push(msg!("Position: empfangen {}, geschrieben {}, angewendet {}; Rückstand {} ms ({} Bytes)", d.lsn, d.flushed, v.applied, v.lag_ms, v.lag_bytes));
                lines.push(msg!("Vom Commit auf dem Primary bis hier angewendet: p50 {}, p99 {}", opt_ns(v.lag_p50), opt_ns(v.lag_p99)));
            }
        }
        let mut tail = Vec::new();
        tail.push(msg!("Sätze gesendet {}, empfangen {}, angewendet {}; Commits angewendet {}; veraltete Lesezugriffe {}, gewartet {}", st.records_out, st.records_in, st.applied_records, st.applied_commits, st.stale_reads, st.read_waits));
        tail.push(msg!("Commits gewartet {}, auf async zurückgefallen {}; Kopien {}; Schreibzugriffe abgewiesen {}", st.sync_waits, st.sync_fallbacks, st.copies, st.refused_writes));
        tail.push(msg!("Verloren beim Failover {}, beim Abgrenzen {}; Commits im Split Brain {} (die andere Seite {})", st.failover_lost, st.fenced_lost, st.split_commits, st.rival_commits));
        let hist: Vec<String> = v.history.iter().map(|(e, at)| format!("{}@{}", e, at)).collect();
        tail.push(msg!("Epochen (Epoche@Position): {}", hist.join(", ")));
        let mut s = String::new();
        for l in &lines {
            s.push_str(&self.tx(l));
            s.push('\n');
        }
        if !table_rows.is_empty() {
            s.push_str(&table(&table_rows));
        }
        for l in &tail {
            s.push_str(&self.tx(l));
            s.push('\n');
        }
        s
    }

    // ---------------------------------------------------------------- cachectl

    pub(crate) fn sh_cachectl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&cache_help());
            self.out(ni, &h);
            return Ok(());
        };
        let Some(name) = self.lab_pick::<Cache>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("cachectl: auf diesem Gerät läuft kein Cache"));
        };
        let dev = self.dev_of(ni);
        let now = self.now;
        let words: Vec<&str> = rest.iter().map(String::as_str).collect();
        let text = match words.as_slice() {
            [] | ["status"] => {
                let Some(v) = self.cache_view(dev, &name) else {
                    return Ok(());
                };
                let lines = [
                    msg!("Cache {}: Port {} vor {}, Lebensdauer {}, höchstens {} Einträge und {}", name, v.port, v.backend, dur(v.ttl as i64), v.max_entries, size_text(v.max_bytes)),
                    msg!("Einträge {} ({}), Anfragen {}: Treffer {}, Fehlschläge {} ({} Treffer)", v.entries, size_text(v.bytes), v.requests, v.hits, v.misses, pm(v.hit_pm)),
                    msg!("Zusammengelegt {}, abgelaufen {}, verdrängt {}, ungültig gemacht {}", v.collapsed, v.expired, v.evictions, v.invalidations),
                    msg!("Backend: {} Anfragen, {} Fehler; Antwortzeit p50 {} (Treffer {}, Fehlschlag {}), p99 {}", v.backend_requests, v.backend_errors, opt_ns(v.p50), opt_ns(v.hit_p50), opt_ns(v.miss_p50), opt_ns(v.p99)),
                ];
                let mut s = String::new();
                for l in lines {
                    s.push_str(&self.tx(&l));
                    s.push('\n');
                }
                s
            }
            ["keys"] => {
                let mut rows = alloc::vec![self.head(phrase("Ziel|Status|Bytes|Alter|frisch bis"))];
                if let Some(c) = self.service::<Cache>(dev, &name) {
                    for k in c.keys().into_iter().rev().take(100) {
                        if let Some(e) = c.peek(&k) {
                            rows.push(alloc::vec![k.clone(), format!("{}", e.status), format!("{}", e.body.len()), self.tx(&dur(now.saturating_sub(e.stored) as i64)), self.tx(&dur(e.expires as i64))]);
                        }
                    }
                }
                table(&rows)
            }
            _ => return self.lab_unknown(ni, "cachectl", &rest, &cache_help()),
        };
        self.out(ni, &text);
        Ok(())
    }

    // ---------------------------------------------------------------- mqctl, workerctl

    pub(crate) fn sh_mqctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&mq_help());
            self.out(ni, &h);
            return Ok(());
        };
        if rest.iter().any(|w| w != "status") {
            return self.lab_unknown(ni, "mqctl", &rest, &mq_help());
        }
        let Some(name) = self.lab_pick::<Queue>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("mqctl: auf diesem Gerät läuft keine Queue"));
        };
        let dev = self.dev_of(ni);
        let Some(v) = self.mq_view(dev, &name) else {
            return Ok(());
        };
        let kept = if v.durable { phrase("auf der Disk") } else { phrase("nur im Speicher") };
        let head = msg!("Queue {}: Port {}, Sichtbarkeit {}, höchstens {} Zustellungen, {}", name, v.port, dur(v.visibility as i64), v.max_deliveries, kept);
        let lat = msg!("Angenommen bis bestätigt: p50 {}, p99 {}", opt_ns(v.p50), opt_ns(v.p99));
        let mut s = format!("{}\n{}\n\n", self.tx(&head), self.tx(&lat));
        let mut rows = alloc::vec![self.head(phrase("Topic|wartend|unterwegs|angenommen|ausgeliefert|erneut|bestätigt|zurück|aussortiert|älteste"))];
        for t in &v.topics {
            rows.push(alloc::vec![t.name.clone(), format!("{}", t.ready), format!("{}", t.inflight), format!("{}", t.published), format!("{}", t.delivered), format!("{}", t.redelivered), format!("{}", t.acked), format!("{}", t.nacked), format!("{}", t.dead), self.tx(&opt_ns(t.oldest))]);
        }
        s.push_str(&table(&rows));
        self.out(ni, &s);
        Ok(())
    }

    pub(crate) fn sh_workerctl(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let Ok((svc, rest)) = Engine::lab_args(args) else {
            let h = self.help_text(&worker_help());
            self.out(ni, &h);
            return Ok(());
        };
        if rest.iter().any(|w| w != "status") {
            return self.lab_unknown(ni, "workerctl", &rest, &worker_help());
        }
        let Some(name) = self.lab_pick::<Worker>(ni, svc.as_deref()) else {
            return self.lab_no(ni, msg!("workerctl: auf diesem Gerät läuft kein Worker"));
        };
        let dev = self.dev_of(ni);
        let (Some(v), Some(w)) = (self.worker_view(dev, &name), self.service::<Worker>(dev, &name)) else {
            return Ok(());
        };
        let head = msg!("Worker {}: {} Topic {}, {} Plätze, {} je Nachricht, Fehlerquote {}", name, v.queue, v.topic, v.concurrency, dur(w.time as i64), crate::fault::ppm_msg(w.error_ppm));
        let counts = msg!("Genommen {}, erledigt {}, zurückgegeben {}, Abfragen {} (leer {}), Fehler {}; Arbeit p50 {}, p99 {}", v.taken, v.done, v.failed, v.polls, v.empty, v.errors, opt_ns(v.p50), opt_ns(v.p99));
        let mut s = format!("{}\n{}\n\n", self.tx(&head), self.tx(&counts));
        let mut rows = alloc::vec![self.head(phrase("Platz|Zustand"))];
        for (i, st) in v.slots.iter().enumerate() {
            let t = match st {
                SlotState::Idle => phrase("pausiert"),
                SlotState::Asking => phrase("wartet auf eine Nachricht"),
                SlotState::Working { id, until } => msg!("arbeitet an Nachricht {} bis {}", id, dur(*until as i64)),
                SlotState::Acking { id } => msg!("meldet Nachricht {} zurück", id),
            };
            rows.push(alloc::vec![format!("{}", i + 1), self.tx(&t)]);
        }
        s.push_str(&table(&rows));
        self.out(ni, &s);
        Ok(())
    }
}
