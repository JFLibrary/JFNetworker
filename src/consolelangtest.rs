// consolelangtest -- the console's language (group consolelang of --selftest: Engine::console_lang,
// Action::ConsoleLang, jfn_model::msg::en): the Linux shell's own tools in English and back in
// German -- lbctl, kvctl, dbctl with the reasons of explain, cachectl, mqctl, workerctl, systemctl
// and journalctl --, their tables aligned in both languages, no German word left in the English
// texts; what stays as it is: IOS output (show version's model title in English in both, the
// radio's media type with its band), the database's SQL errors; a scenario run in English
// (run_scenario_in) changes only the console's words
//
// The lab is labtest's: hosts SRV1..SRV7 (10.0.0.1..7) on one switch, every ARP entry by hand.
// The units are tests/fixtures/units.jfnet (DEV1 holds the web service as files), the access
// points tests/fixtures/wlan_office.jfnet (AP1, a JF-AP6E: Dot11Radio0..2 send on 2.4, 5, 6 GHz).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use agent_core::jmap::Value;
use jfn_model::msg::Lang;
use jfn_model::project::{PortRef, Project, ServiceDef};
use jfn_sim::node::port_mac;
use jfn_sim::{Action, Engine, Report, MS, SEC};
use netpkt::{Ipv4, Prefix4};

use crate::selftest::T;

const BUDGET: usize = 50_000_000;
const UNITS: &str = include_str!("../tests/fixtures/units.jfnet");
const WLAN: &str = include_str!("../tests/fixtures/wlan_office.jfnet");
const DB: &str = r#"{"tables": [{"name": "users", "rows": 1000, "row_bytes": 200, "columns": ["email:text"], "indexes": ["email"]}]}"#;

fn s(x: &str) -> String {
    String::from(x)
}

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: String::from(port) }
}

fn addr(i: usize) -> Ipv4 {
    Ipv4([10, 0, 0, i as u8 + 1])
}

/// hosts SRV1..SRVn (10.0.0.1..n) on one switch, every ARP entry set by hand
fn net(n: usize) -> (Engine, Vec<u32>) {
    let mut p = Project::new("lang", 23);
    let sw = p.add_device("jf-s24p", 0, 200).unwrap_or(0);
    let hosts: Vec<u32> = (0..n).map(|i| p.add_device("server-1u", i as i32 * 100, 0).unwrap_or(0)).collect();
    for (i, h) in hosts.iter().enumerate() {
        let _ = p.connect(&pr(*h, "eth0"), &pr(sw, &format!("gi1/0/{}", i + 1)), None);
    }
    let mut e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    for (i, h) in hosts.iter().enumerate() {
        let _ = e.apply(Action::IfAddr { dev: *h, iface: s("eth0"), prefix: Prefix4 { addr: addr(i), len: 24 }, add: true });
    }
    for (i, a) in hosts.iter().enumerate() {
        for (j, b) in hosts.iter().enumerate() {
            if i != j {
                let _ = e.apply(Action::StaticArp { dev: *a, ip: addr(j), mac: Some(port_mac(*b, 0)) });
            }
        }
    }
    (e, hosts)
}

fn add(e: &mut Engine, dev: u32, kind: &str, name: &str, config: &str) -> bool {
    let def = ServiceDef { kind: s(kind), name: s(name), disabled: false, config: agent_core::json::parse(config.as_bytes()).unwrap_or(Value::Null) };
    e.apply(Action::Service { dev, def }).is_ok()
}

fn run_for(e: &mut Engine, d: u64) {
    let t = e.now() + d;
    e.run_until(t, BUDGET);
}

/// a command line on a device and what it printed
fn sh(e: &mut Engine, dev: u32, line: &str) -> String {
    let _ = e.take_reports();
    let _ = e.cli_line(dev, line);
    run_for(e, MS);
    let mut o = String::new();
    for r in e.take_reports() {
        if let Report::CliOut { dev: d, bytes, .. } = r
            && d == dev
        {
            o.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    o
}

fn lang(e: &mut Engine, l: Lang) -> bool {
    e.apply(Action::ConsoleLang { lang: l }).is_ok() && e.console_lang == l
}

/// German words of the tools (and the German quotes and umlauts): none may be left in English
const GERMAN: [&str; 26] = [
    "Anfragen", "Knoten", "Zustand", "seit", "verfügbar", "ausgefallen", "Fehler", "Einträge", "Plätze", "läuft", "Einheit", "Schlüssel", "Antwortzeit", "Gewicht", "Adresse", "gesamt", "Treffer", "wartend", "höchstens", "Prüfpunkt", "Puffer", "Quelle", "beim Start", "nach", "bisher", "\u{201e}",
];

fn german_in(o: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = GERMAN.iter().copied().filter(|w| o.split(|c: char| !c.is_alphanumeric() && c != '\u{201e}').any(|x| x == *w) || (w.contains(' ') && o.contains(*w))).collect();
    if o.contains(['ä', 'ö', 'ü', 'ß', 'Ä', 'Ö', 'Ü']) {
        found.push("umlaut");
    }
    found
}

/// the column a header cell starts at and the column of a cell in another line: equal when the
/// table is aligned
fn col(line: &str, cell: &str) -> Option<usize> {
    line.find(cell).map(|b| line[..b].chars().count())
}

fn line_with<'a>(o: &'a str, part: &str) -> &'a str {
    o.lines().find(|l| l.contains(part)).unwrap_or("")
}

pub(crate) fn run(t: &mut T) {
    msg_t(t);
    lab_t(t);
    db_t(t);
    units_t(t);
    ios_t(t);
    scenario_t(t);
}

// ---------------------------------------------------------------- the lookup

fn msg_t(t: &mut T) {
    t.eq("en: a row", jfn_model::msg::en("Konsole"), "Console");
    t.eq("en: no row: the template", jfn_model::msg::en("kein Text dieses Katalogs"), "kein Text dieses Katalogs");
    let m = jfn_model::msg!("Neustart in {}", jfn_model::msg::dur(1_500_000_000));
    t.eq("text_in German", m.text_in(Lang::De), s("Neustart in 1,500 s"));
    t.eq("text_in English, the time nested", m.text_in(Lang::En), s("restart in 1.500 s"));
    t.eq("parse", [Lang::parse("en"), Lang::parse("EN-us"), Lang::parse("de"), Lang::parse("fr")], [Some(Lang::En), Some(Lang::En), Some(Lang::De), None]);
    t.eq("German by default", Engine::new(0).console_lang, Lang::De);
    t.eq("words of the tables", jfn_model::experiment::words_in("größer 2 Kerne", &jfn_model::msg::en), s("bigger 2 cores"));
    t.eq("words: a bottleneck", jfn_model::experiment::words_in("Sperre DB db", &jfn_model::msg::en), s("Lock DB db"));
}

// ---------------------------------------------------------------- lbctl, kvctl, cachectl, mqctl, workerctl

fn lab_t(t: &mut T) {
    let (mut e, h) = net(6);
    let mut ok = add(&mut e, h[0], "lb", "lb", "{\"backends\": [{\"name\": \"web1\", \"addr\": \"10.0.0.2\"}], \"health\": {\"interval_ms\": 1000}}");
    ok &= add(&mut e, h[1], "http-server", "web", "{}");
    ok &= add(&mut e, h[2], "kv", "kv", "{\"nodes\": [\"10.0.0.3\", \"10.0.0.4\"], \"vnodes\": 2, \"rf\": 2}");
    ok &= add(&mut e, h[3], "kv", "kv", "{\"nodes\": [\"10.0.0.3\", \"10.0.0.4\"], \"vnodes\": 2, \"rf\": 2}");
    ok &= add(&mut e, h[4], "cache", "cache", "{\"backend\": \"http://10.0.0.2/\"}");
    ok &= add(&mut e, h[5], "mq", "mq", "{\"topics\": [\"jobs\"]}");
    ok &= add(&mut e, h[5], "worker", "worker", "{\"queue\": \"http://10.0.0.6:9000/\", \"concurrency\": 2}");
    t.check("set up", ok);
    run_for(&mut e, 2 * SEC);
    let _ = sh(&mut e, h[2], "curl -s -X PUT -d welt http://10.0.0.3:7000/kv/hallo");
    run_for(&mut e, SEC);
    // German first, as every golden has it
    let de = sh(&mut e, h[0], "lbctl");
    t.check(&format!("German: lbctl {:?}", de.lines().next()), de.starts_with("Loadbalancer lb: HTTP auf Port 80, Verfahren round-robin, Health Checks http GET / alle 1,000 s (Timeout 1,000 s, verfügbar nach 2, ausgefallen nach 3 Prüfungen)\n"));
    t.check("German: lbctl table", de.contains("Backend  Adresse      Gewicht  Zustand    seit  offen  gesamt  Fehler  p99\n"));
    t.check("to English", lang(&mut e, Lang::En));
    // lbctl
    let o = sh(&mut e, h[0], "lbctl");
    t.check(&format!("lbctl: head {:?}", o.lines().next()), o.starts_with("Load balancer lb: HTTP on port 80, algorithm round-robin, health checks http GET / every 1.000 s (timeout 1.000 s, available after 2, down after 3 checks)\n"));
    t.check(&format!("lbctl: counts {:?}", o.lines().nth(1)), o.lines().nth(1).is_some_and(|l| l.starts_with("Requests ") && l.contains(", finished ") && l.contains("(no backend 0), retries 0")));
    t.check("lbctl: latency", o.lines().nth(2).is_some_and(|l| l.starts_with("Response time p50 ")));
    let head = line_with(&o, "Backend");
    let row = line_with(&o, "web1");
    t.eq("lbctl: head of the table", head, "Backend  Address      Weight  State      since  open  total  errors  p99");
    t.check(&format!("lbctl: a backend {:?}", row), row.contains("available"));
    t.eq("lbctl: aligned (State)", col(row, "available"), col(head, "State"));
    t.eq("lbctl: no German left", german_in(&o), Vec::<&str>::new());
    t.check("lbctl: help", sh(&mut e, h[0], "lbctl -h").starts_with("lbctl [status] [-s SERVICE]   shows the load balancer: settings, figures and every backend\n"));
    let u = sh(&mut e, h[0], "lbctl frobnicate");
    t.check(&format!("lbctl: unknown word {:?}", u), u.starts_with("lbctl: frobnicate is unknown\nlbctl [status] [-s SERVICE]"));
    t.eq("lbctl: no load balancer here", sh(&mut e, h[1], "lbctl"), s("lbctl: no load balancer runs on this device\n"));
    // kvctl
    let o = sh(&mut e, h[2], "kvctl");
    t.check(&format!("kvctl: head {:?}", o.lines().next()), o.starts_with("KV node kv: 10.0.0.3, port 7000, heartbeats UDP 7946 every 500.000 ms (down after 1.500 s)\n"));
    t.check("kvctl: ring", o.contains("Ring: 2 nodes with 2 vnodes each, RF 2, consistency strong (W 2, R 2, W + R > RF), read repair on\n"));
    t.check("kvctl: members", o.contains("10.0.0.3  this node") && o.contains("10.0.0.4  reachable"));
    let head = line_with(&o, "Ring share");
    t.check(&format!("kvctl: head of the table {:?}", head), head.starts_with("Node      State      since"));
    t.eq("kvctl: aligned", col(line_with(&o, "reachable"), "reachable"), col(head, "State"));
    t.check(&format!("kvctl: figures {}", o), o.contains("Keys 1 (") && o.contains("Requests 1: read 0, written 1") && o.contains("Stale answers 0, read repairs sent 0"));
    t.eq("kvctl: no German left", german_in(&o), Vec::<&str>::new());
    let w = sh(&mut e, h[2], "kvctl where hallo");
    t.check(&format!("kvctl where: {}", w), w.starts_with("Key hallo: hash 0x") && w.contains(", replicas (RF 2):\n") && w.matches("version 1.").count() == 2 && w.contains("reachable"));
    t.eq("kvctl get: missing", sh(&mut e, h[3], "kvctl get nix"), s("nix: not on this node\n"));
    t.check("kvctl get", sh(&mut e, h[3], "kvctl get hallo").starts_with("hallo = welt (version 1."));
    t.check("kvctl keys", sh(&mut e, h[2], "kvctl keys").starts_with("Key    Version"));
    t.check("kvctl ring", sh(&mut e, h[2], "kvctl ring").starts_with("Token               Node\n"));
    // cachectl
    let o = sh(&mut e, h[4], "cachectl");
    t.check(&format!("cachectl: {:?}", o.lines().next()), o.starts_with("Cache cache: port 80 in front of http://10.0.0.2/, lifetime 30.000 s, at most 1000 entries and 64 MiB\n"));
    t.check(&format!("cachectl: the decimal point {}", o), o.contains("hits 0, misses 0 (0.0 % hits)") && o.contains("Collapsed 0, expired 0"));
    t.eq("cachectl: no German left", german_in(&o), Vec::<&str>::new());
    t.check("cachectl keys", sh(&mut e, h[4], "cachectl keys").starts_with("Target  Status  Bytes  Age  fresh until"));
    // mqctl, workerctl
    let o = sh(&mut e, h[5], "mqctl");
    t.check(&format!("mqctl: {}", o), o.starts_with("Queue mq: port 9000, visibility 5.000 s, at most 5 deliveries, on disk\nPublished to acknowledged: p50 ") && o.contains("Topic  waiting  in flight  published"));
    let head = line_with(&o, "Topic");
    t.eq("mqctl: aligned", col(line_with(&o, "jobs"), "0"), col(head, "waiting"));
    t.eq("mqctl: no German left", german_in(&o), Vec::<&str>::new());
    let o = sh(&mut e, h[5], "workerctl");
    t.check(&format!("workerctl: {}", o), o.starts_with("Worker worker: http://10.0.0.6:9000/ topic jobs, 2 slots, 50.000 ms per message, error rate 0 %\nTaken ") && o.contains("Slot  State\n") && o.contains("1     waiting for a message"));
    t.eq("workerctl: no German left", german_in(&o), Vec::<&str>::new());
    // and German again
    t.check("back to German", lang(&mut e, Lang::De));
    let again = sh(&mut e, h[0], "lbctl");
    t.check("German again: lbctl", again.starts_with("Loadbalancer lb: HTTP auf Port 80") && again.contains("Backend  Adresse"));
    t.check("German again: mqctl", sh(&mut e, h[5], "mqctl").starts_with("Queue mq: Port 9000, Sichtbarkeit 5,000 s, höchstens 5 Zustellungen, auf der Disk\n"));
    t.check("German again: workerctl", sh(&mut e, h[5], "workerctl").contains("1      wartet auf eine Nachricht"));
}

// ---------------------------------------------------------------- dbctl

fn db_t(t: &mut T) {
    let (mut e, h) = net(1);
    t.check("db: set up", add(&mut e, h[0], "db", "db", DB));
    run_for(&mut e, SEC);
    let de = sh(&mut e, h[0], "dbctl explain SELECT * FROM users WHERE id = 1");
    t.check(&format!("German: explain {}", de), de.contains("\"why\":\"Gleichheit auf dem Schlüssel: der Baum führt zur Zeile.\""));
    t.check("to English", lang(&mut e, Lang::En));
    let o = sh(&mut e, h[0], "dbctl");
    t.check(&format!("dbctl: {}", o), o.starts_with("Database db: port 7100, 0 of at most ") && o.contains("This device simulates no hardware: the CPU costs nothing") && o.contains("\nBuffer pool: ") && o.contains("\nStatements 0: read 0") && o.contains("\nResponse time p50 "));
    let hits = line_with(&o, "Buffer pool").split("hits ").nth(1).and_then(|r| r.split(" %").next()).unwrap_or("");
    t.check(&format!("dbctl: the buffer's hits with a point {:?}", hits), hits.contains('.') && !hits.contains(','));
    t.eq("dbctl: no German left", german_in(&o), Vec::<&str>::new());
    let o = sh(&mut e, h[0], "dbctl tables");
    t.check(&format!("dbctl tables: {}", o), o.starts_with("Table  Key  Rows  changed  Pages") && o.contains("users"));
    t.eq("dbctl locks", sh(&mut e, h[0], "dbctl locks"), s("no row locks\n"));
    let o = sh(&mut e, h[0], "dbctl explain SELECT * FROM users WHERE id = 1");
    t.check(&format!("dbctl explain: the key {}", o), o.contains("\"plan\":\"index\"") && o.contains("\"why\":\"Equality on the key: the tree leads to the row.\""));
    let o = sh(&mut e, h[0], "dbctl explain \"SELECT * FROM users WHERE email = 'a@b'\"");
    t.check(&format!("dbctl explain: an index {}", o), o.contains("\"why\":\"There is an index on this column: the tree, then the rows.\""));
    let o = sh(&mut e, h[0], "dbctl explain SELECT * FROM users");
    t.check(&format!("dbctl explain: all pages {}", o), o.contains("\"plan\":\"scan\"") && o.contains("\"why\":\"No condition: every page of the table.\""));
    // the database's own errors stay as its clients get them
    let o = sh(&mut e, h[0], "dbctl explain SELECT");
    t.check(&format!("dbctl explain: the SQL error stays German {}", o), o.starts_with("dbctl: Zeichen 7:"));
    t.check("dbctl: unknown word", sh(&mut e, h[0], "dbctl nix").starts_with("dbctl: nix is unknown\ndbctl [status] [-s SERVICE]   shows the database"));
    let help = sh(&mut e, h[0], "dbctl -h");
    let want = format!("{:<30}shows the database: state, buffer pool, log and figures\n{:<30}its tables: rows, changed rows, pages, indexes\n", "dbctl [status] [-s SERVICE]", "dbctl tables");
    t.check(&format!("dbctl: help aligned {}", help), help.starts_with(&want) && help.contains(&format!("\n{:<30}how a statement would run", "dbctl explain STATEMENT")));
}

// ---------------------------------------------------------------- systemctl, journalctl

fn units_t(t: &mut T) {
    let Ok(p) = Project::from_text(UNITS) else {
        t.check("units: the fixture", false);
        return;
    };
    let dev1 = p.find("DEV1").map_or(0, |d| d.id);
    let mut e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    run_for(&mut e, 500 * MS);
    let de = sh(&mut e, dev1, "systemctl list-units --all");
    t.check(&format!("German: list-units {}", de), de.starts_with("Einheit           Quelle  Aktiv     Im Detail  Beschreibung\n") && de.ends_with("6 Einheiten\n"));
    t.check("to English", lang(&mut e, Lang::En));
    let o = sh(&mut e, dev1, "systemctl list-units --all");
    let head = o.lines().next().unwrap_or("");
    t.eq("list-units: head", head, format!("{:<18}{:<8}{:<10}{:<6}{}", "Unit", "Source", "Active", "Sub", "Description").as_str());
    t.eq("list-units: aligned", col(line_with(&o, "web.service"), "File"), col(head, "Source"));
    t.check(&format!("list-units: the count {}", o), o.ends_with("6 units\n") && line_with(&o, "kaputt.service").contains("Error"));
    t.check("list-unit-files", sh(&mut e, dev1, "systemctl list-unit-files").starts_with(&format!("{:<18}At boot", "Unit")));
    let _ = sh(&mut e, dev1, "systemctl start web");
    run_for(&mut e, 100 * MS);
    let o = sh(&mut e, dev1, "systemctl status web");
    t.check(&format!("status: state {}", o), o.starts_with("web.service: Der Webdienst\n  State:     running (active/running) since 50") && o.contains(" ms\n  Source:    /etc/systemd/system/web.service (at boot: disabled)\n  Program:   jfs /root/app/server.jfs\n  Restart:   on-failure after 2 s, 0 so far\n  PID:       "));
    t.check(&format!("status: the unit manager's line in the log {}", o), o.contains("systemd: web.service: started: jfs /root/app/server.jfs\n"));
    t.eq("status: no German left (but the service's own description and log)", german_in(&o.replace("Der Webdienst", "").replace("bereit, Version 2", "")), Vec::<&str>::new());
    let o = sh(&mut e, dev1, "systemctl");
    t.check(&format!("list without words {}", o), o.starts_with("Unit") && o.ends_with("\n1 unit\n"));
    t.eq("status of the device", sh(&mut e, dev1, "systemctl status"), s("DEV1: 6 units, 1 running, 0 failed\n"));
    t.eq("start of a unit that does not exist", sh(&mut e, dev1, "systemctl start nix"), s("systemctl start: The unit nix.service does not exist.\n"));
    let o = sh(&mut e, dev1, "systemctl start kaputt");
    t.check(&format!("a unit that cannot start {}", o), o.starts_with("systemctl start: [Service] needs ExecStart. (systemctl status kaputt.service, journalctl -u kaputt.service)"));
    let o = sh(&mut e, dev1, "systemctl status kaputt");
    t.check(&format!("status with issues {}", o), o.contains("  State:     Error (failed/failed) since ") && o.contains("  Reason:    [Service] needs ExecStart.\n") && o.contains("  Warning: Line 3: there is no Quatsch here, the line does not count.\n") && o.contains("  Error: [Service] needs ExecStart.\n"));
    t.eq("an option it does not know", sh(&mut e, dev1, "systemctl --frob"), s("systemctl: the option --frob is unknown\n"));
    t.eq("a verb it does not know", sh(&mut e, dev1, "systemctl frob"), s("systemctl: the command frob is unknown (systemctl --help)\n"));
    t.eq("a verb without a unit", sh(&mut e, dev1, "systemctl start"), s("systemctl start: which unit? (systemctl start NAME)\n"));
    t.eq("journalctl: no lines", sh(&mut e, dev1, "journalctl -u nix"), s("No lines.\n"));
    t.eq("journalctl: -n without a number", sh(&mut e, dev1, "journalctl -n x"), s("journalctl: -n needs a number\n"));
    t.eq("journalctl: an option it does not know", sh(&mut e, dev1, "journalctl --frob"), s("journalctl: --frob is unknown\n"));
    t.check("back to German", lang(&mut e, Lang::De));
    let o = sh(&mut e, dev1, "systemctl status web");
    t.check(&format!("German again: status {}", o), o.contains("\n  Zustand:   läuft (active/running) seit 50") && o.contains(" ms\n  Quelle:    /etc/systemd/system/web.service (beim Start: disabled)\n  Programm:  jfs /root/app/server.jfs\n  Neustart:  on-failure nach 2 s, bisher 0\n"));
}

// ---------------------------------------------------------------- IOS stays English

fn ios_t(t: &mut T) {
    let Ok(p) = Project::from_text(WLAN) else {
        t.check("ios: the fixture", false);
        return;
    };
    let (r1, ap1) = (p.find("R1").map_or(0, |d| d.id), p.find("AP1").map_or(0, |d| d.id));
    let mut e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    run_for(&mut e, SEC);
    for l in [Lang::De, Lang::En] {
        t.check("language", lang(&mut e, l));
        let o = sh(&mut e, r1, "show version");
        t.check(&format!("{:?}: show version names the model in English {:?}", l, o.lines().next()), o.starts_with("JFNetworker device software 1.0 for the JF-R4 (Edge router 4× 1G, 2× 10G SFP+, 2× serial)\n"));
        let o = sh(&mut e, ap1, "show version");
        t.check(&format!("{:?}: the access point's title {:?}", l, o.lines().next()), o.lines().next().is_some_and(|x| x.starts_with("JFNetworker device software 1.0 for the JF-AP6E (WLAN access point") && !x.contains("gespeist")));
        for (port, band) in [("Dot11Radio0", "2.4 GHz"), ("Dot11Radio1", "5 GHz"), ("Dot11Radio2", "6 GHz")] {
            let o = sh(&mut e, ap1, &format!("show interfaces {}", port));
            let m = line_with(&o, "media type is");
            t.check(&format!("{:?}: {} sends on {}: {:?}", l, port, band, m), m.ends_with(&format!("media type is WLAN radio {}", band)));
        }
        let o = sh(&mut e, ap1, "show interfaces TwoGigabitEthernet0");
        t.check(&format!("{:?}: copper stays RJ45 {:?}", l, line_with(&o, "media type")), line_with(&o, "media type").ends_with("media type is RJ45"));
    }
}

// ---------------------------------------------------------------- a scenario in English

fn scenario_t(t: &mut T) {
    let Ok(p) = Project::from_text(UNITS) else {
        t.check("scenario: the fixture", false);
        return;
    };
    let Ok(sc) = jfn_model::scenario::parse("run 500ms\nDEV1: systemctl list-units --all\nrun 100ms\nexpect DEV1 output contains \"6 units\"\n") else {
        t.check("scenario: parses", false);
        return;
    };
    let en = jfn_sim::run_scenario_in(&p, &sc, "lang", false, Lang::En);
    t.check(&format!("scenario: English console {}", en.report), en.ok && en.report.contains("Unit              Source  Active") && en.report.contains("6 units"));
    t.check("scenario: the report's own words stay German", en.report.starts_with("# JFNetworker-Szenario lang: "));
    let de = jfn_sim::run_scenario(&p, &sc, "lang", false);
    t.check("scenario: German by default", !de.ok && de.report.contains("Einheit           Quelle") && de.report.contains("6 Einheiten"));
    t.eq("scenario: in_ with German is run_scenario", jfn_sim::run_scenario_in(&p, &sc, "lang", false, Lang::De).report, de.report);
}
