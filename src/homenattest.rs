// homenattest -- checks of NAT and the forward filter of a Linux device (part of --selftest, group
// "homenat": jfn_sim's cli/iptables.rs, the FORWARD chain of fw.rs, MASQUERADE in nat.rs): a home
// network behind GW reaches nothing before "iptables -t nat -A POSTROUTING -o wan -j MASQUERADE" and
// everything after it (ping and TCP, two hosts behind one address, the translations, -L and -S as the
// original lays them out, counters), -D and -F undo; the FORWARD chain drops what comes unasked from
// outside and lets the answers to the LAN's own connections in (ESTABLISHED, an ICMP error RELATED),
// REJECT with its ICMP, rules by number, flushing back to nothing; the refusals; the configuration
// round trip (running configuration -> a new engine -> the same lines and behaviour), an interface
// added later, iptables-save; a home router placed from the palette with its defaults, a file's and
// the template's untouched; a DHCP server added the way "Dienst hinzufügen" adds it; IOS-like devices
// refuse it all
//
// The network (device ids in order): PC1 (192.168.1.10, TCP echo on port 7) on lan1 and PC2
// (192.168.1.11) on lan2 of GW, a home router (br0 192.168.1.1/24, wan 100.64.0.2/30); ISP between
// 100.64.0.0/30 (Gi0/0/0, .1) and 203.0.113.0/24 (Gi0/0/1, .1) without a route back to the home
// network unless a check gives it one; WEB (203.0.113.10, TCP echo on port 80) behind ISP.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use agent_core::jmap::Value;
use jfn_model::catalog;
use jfn_model::project::{PortRef, Project, ServiceDef};
use jfn_sim::fw::{FwOp, FwdOp};
use jfn_sim::fwtrace::FwNote;
use jfn_sim::nat::{NatAct, Rule, Side};
use jfn_sim::{Action, Engine, IcmpKind, JobKind, Note, PduProto, PduSender, PduSpec, PingEv, PingOpts, Report, MS, SEC};
use netpkt::ipv4::{PROTO_ICMP, PROTO_TCP};
use netpkt::Ipv4;

use crate::doc::{edits, Doc};
use crate::selftest::T;
use crate::workspace::devwin::{svcedit, svcform};

const BUDGET: usize = 4_000_000;
const PC1: u32 = 1;
const PC2: u32 = 2;
const GW: u32 = 3;
const ISP: u32 = 4;
const WEB: u32 = 5;

const GW_BASE: &str = "hostname GW\nip addr add 100.64.0.2/30 dev wan\nip addr add 192.168.1.1/24 dev br0\nip route add default via 100.64.0.1\n";
const ISP_CFG: &str = "hostname ISP\n!\ninterface GigabitEthernet0/0/0\n ip address 100.64.0.1 255.255.255.252\n no shutdown\n!\ninterface GigabitEthernet0/0/1\n ip address 203.0.113.1 255.255.255.0\n no shutdown\n!\n";
const ROUTE_BACK: &str = "ip route 192.168.1.0 255.255.255.0 100.64.0.2\n!\n";
const MASQ: &str = "iptables -t nat -A POSTROUTING -o wan -j MASQUERADE\n";
const FILTER: &str = "iptables -P FORWARD DROP\niptables -A FORWARD -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT\niptables -A FORWARD -i br0 -o wan -j ACCEPT\n";

fn s(x: &str) -> String {
    String::from(x)
}

fn ip(x: &str) -> Ipv4 {
    Ipv4::parse(x).unwrap_or(Ipv4::UNSPECIFIED)
}

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: s(port) }
}

fn echo(port: u16) -> ServiceDef {
    let config = agent_core::json::parse(format!("{{\"port\": {}}}", port).as_bytes()).unwrap_or(Value::Null);
    ServiceDef { kind: s("tcp-echo"), name: s("echo"), disabled: false, config }
}

/// the network: GW boots with `gw` (its addresses and route first unless `bare`), ISP knows the way
/// back when `back`
fn project(gw: &str, bare: bool, back: bool) -> Project {
    let mut p = Project::new("home", 23);
    for m in ["pc", "pc", "home-router", "jf-r4", "server-1u"] {
        let _ = p.add_device(m, 0, 0);
    }
    let _ = p.connect(&pr(PC1, "eth0"), &pr(GW, "lan1"), None);
    let _ = p.connect(&pr(PC2, "eth0"), &pr(GW, "lan2"), None);
    let _ = p.connect(&pr(GW, "wan"), &pr(ISP, "gi0/0/0"), None);
    let _ = p.connect(&pr(ISP, "gi0/0/1"), &pr(WEB, "eth0"), None);
    let host = |name: &str, a: &str, g: &str| format!("hostname {}\nip addr add {} dev eth0\nip route add default via {}\n", name, a, g);
    let cfgs = [
        (PC1, "PC1", host("PC1", "192.168.1.10/24", "192.168.1.1")),
        (PC2, "PC2", host("PC2", "192.168.1.11/24", "192.168.1.1")),
        (GW, "GW", if bare { s(gw) } else { format!("{}{}", GW_BASE, gw) }),
        (ISP, "ISP", format!("{}{}end\n", ISP_CFG, if back { ROUTE_BACK } else { "" })),
        (WEB, "WEB", host("WEB", "203.0.113.10/24", "203.0.113.1")),
    ];
    for (id, name, c) in cfgs {
        if let Some(d) = p.device_mut(id) {
            d.name = s(name);
            d.startup_config = c;
        }
    }
    if let Some(d) = p.device_mut(PC1) {
        d.services.push(echo(7));
    }
    if let Some(d) = p.device_mut(WEB) {
        d.services.push(echo(80));
    }
    p
}

fn run_for(e: &mut Engine, span: u64) {
    let t = e.now() + span;
    e.run_until(t, BUDGET);
}

/// the engine of a project, booted, with the neighbours known (IOS drops the packet that needs ARP)
fn boot(p: &Project) -> Engine {
    let mut e = Engine::from_project(p).unwrap_or_else(|_| Engine::new(0));
    run_for(&mut e, SEC);
    for a in ["100.64.0.2", "203.0.113.10"] {
        let _ = e.ping(ISP, ip(a), PingOpts { count: 1, ..PingOpts::default() });
    }
    for dev in [PC1, PC2] {
        let _ = e.ping(dev, ip("192.168.1.1"), PingOpts { count: 1, ..PingOpts::default() });
    }
    run_for(&mut e, 3 * SEC);
    let _ = e.take_reports();
    e
}

fn engine(gw: &str, back: bool) -> Engine {
    boot(&project(gw, false, back))
}

/// what a device printed since the last call
fn out(e: &mut Engine, dev: u32) -> String {
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

/// a line on a Linux device's shell: what it printed
fn sh(e: &mut Engine, dev: u32, line: &str) -> String {
    let _ = out(e, dev);
    let _ = e.cli_line(dev, line);
    run_for(e, 10 * MS);
    out(e, dev)
}

/// answers to `n` pings
fn pings(e: &mut Engine, dev: u32, dst: &str, n: u32) -> usize {
    let id = e.ping(dev, ip(dst), PingOpts { count: n, ..PingOpts::default() }).unwrap_or(0);
    run_for(e, (n as u64 + 3) * SEC);
    match e.job(id).map(|j| &j.kind) {
        Some(JobKind::Ping(p)) => p.events.iter().filter(|x| matches!(x, PingEv::Reply { .. })).count(),
        _ => 0,
    }
}

/// did a TCP connection go through: no failure and something echoed
fn tcp_ok(e: &mut Engine, dev: u32, dst: &str, sport: u16, dport: u16) -> bool {
    let name = format!("pdu{}", e.now());
    let spec = PduSpec { proto: PduProto::Tcp, dst: s(dst), sport, dport, size: 20, ttl: None, count: 1, interval: MS };
    let _ = e.add_service(dev, &name, Box::new(PduSender::new(spec)), true);
    run_for(e, 3 * SEC);
    e.service::<PduSender>(dev, &name).is_some_and(|x| x.failed == 0 && x.replies >= 1)
}

fn mark(e: &Engine) -> usize {
    e.trace.events().count()
}

/// the notes of a device since event `at`
fn notes(e: &Engine, dev: u32, at: usize) -> Vec<Note> {
    e.trace.events().skip(at).filter(|x| x.dev == dev).map(|x| x.note.clone()).collect()
}

fn fw_notes(e: &Engine, at: usize) -> Vec<FwNote> {
    notes(e, GW, at).into_iter().filter_map(|n| if let Note::Fw(f) = n { Some(f) } else { None }).collect()
}

pub fn run(t: &mut T) {
    masquerade(t);
    forward_filter(t);
    refusals(t);
    round_trip(t);
    palette(t);
    dhcp_dialog(t);
    ios_untouched(t);
}

// MASQUERADE on wan: before nothing comes back, after everything does
fn masquerade(t: &mut T) {
    let mut e = engine("", false);
    t.check("without NAT: nothing", e.nat(GW).is_some_and(|n| !n.active()) && e.firewall(GW).is_some_and(|f| !f.active()));
    t.eq("no route back: no answer", pings(&mut e, PC1, "203.0.113.10", 2), 0);
    t.eq("the rule says nothing", sh(&mut e, GW, MASQ.trim_end()), s(""));
    t.check("wan outside, br0 and lo inside", e.nat(GW).is_some_and(|n| n.sides.values().filter(|x| **x == Side::Outside).count() == 1 && n.sides.len() == 3));
    let at = mark(&e);
    t.eq("ping through NAT", pings(&mut e, PC1, "203.0.113.10", 3), 3);
    t.check("WEB answers the router's address", notes(&e, WEB, at).iter().any(|n| matches!(n, Note::EchoAnswered { to } if *to == ip("100.64.0.2"))));
    t.check("GW notes the translation", notes(&e, GW, at).iter().any(|n| matches!(n, Note::NatCreate { proto: PROTO_ICMP, local, global, .. } if local.0 == ip("192.168.1.10") && global.0 == ip("100.64.0.2"))));
    t.check("TCP through NAT", tcp_ok(&mut e, PC1, "203.0.113.10", 40001, 80));
    t.eq("a second host", pings(&mut e, PC2, "203.0.113.10", 2), 2);
    let rows = e.nat_translations(GW);
    let behind = |a: &str, p: u8| rows.iter().any(|r| r.proto == p && r.inside_local.0 == ip(a) && r.inside_global.0 == ip("100.64.0.2"));
    t.check("translations: both hosts behind one address", behind("192.168.1.10", PROTO_TCP) && behind("192.168.1.10", PROTO_ICMP) && behind("192.168.1.11", PROTO_ICMP));
    t.eq(
        "-t nat -L -n",
        sh(&mut e, GW, "iptables -t nat -L -n"),
        s("Chain PREROUTING (policy ACCEPT)\ntarget     prot opt source               destination         \n\nChain INPUT (policy ACCEPT)\ntarget     prot opt source               destination         \n\nChain OUTPUT (policy ACCEPT)\ntarget     prot opt source               destination         \n\nChain POSTROUTING (policy ACCEPT)\ntarget     prot opt source               destination         \nMASQUERADE  all  --  0.0.0.0/0            0.0.0.0/0           \n"),
    );
    // three connections began: PC1's ping, its TCP connection, PC2's ping (the first packet each)
    let v = sh(&mut e, GW, "iptables -t nat -nvL POSTROUTING");
    t.check("-L -v: header", v.starts_with("Chain POSTROUTING (policy ACCEPT 0 packets, 0 bytes)\n pkts bytes target     prot opt in     out     source               destination         \n"));
    t.check("-L -v: the rule counts connections", v.lines().nth(2).is_some_and(|l| l.starts_with("    3   ") && l.ends_with(" MASQUERADE  all  --  *      wan     0.0.0.0/0            0.0.0.0/0           ")));
    t.eq("-t nat -S", sh(&mut e, GW, "iptables -t nat -S"), s("-P PREROUTING ACCEPT\n-P INPUT ACCEPT\n-P OUTPUT ACCEPT\n-P POSTROUTING ACCEPT\n-A POSTROUTING -o wan -j MASQUERADE\n"));
    t.check("without -n: anywhere", sh(&mut e, GW, "iptables -t nat -L POSTROUTING").contains("MASQUERADE  all  --  anywhere             anywhere            \n"));
    t.check("-C finds it", sh(&mut e, GW, "iptables -t nat -C POSTROUTING -o wan -j MASQUERADE; echo $?").ends_with("0\n"));
    t.check("-Z", {
        let _ = sh(&mut e, GW, "iptables -t nat -Z");
        e.nat(GW).is_some_and(|n| n.masq_hits.values().all(|h| *h == (0, 0)))
    });
    // -D undoes it all: no sides, no translations, nothing comes back
    t.eq("-D", sh(&mut e, GW, "iptables -t nat -D POSTROUTING -o wan -j MASQUERADE"), s(""));
    t.check("-D: no rule, no side, no translation", e.nat(GW).is_some_and(|n| n.rules.is_empty() && !n.active() && n.sessions().next().is_none()));
    t.eq("-D: no answer again", pings(&mut e, PC1, "203.0.113.10", 2), 0);
    t.check("-D: no TCP again", !tcp_ok(&mut e, PC1, "203.0.113.10", 40000, 80));
    t.eq("-D of a rule that is gone", sh(&mut e, GW, "iptables -t nat -D POSTROUTING -o wan -j MASQUERADE"), s("iptables: Bad rule (does a matching rule exist in that chain?).\n"));
    // with a source network; -F takes everything
    let _ = sh(&mut e, GW, "iptables -t nat -A POSTROUTING -s 192.168.1.10/32 -o wan -j MASQUERADE");
    t.eq("-s: PC1 gets out", pings(&mut e, PC1, "203.0.113.10", 2), 2);
    t.eq("-s: PC2 does not", pings(&mut e, PC2, "203.0.113.10", 2), 0);
    t.eq("-s in -S", sh(&mut e, GW, "iptables -t nat -S POSTROUTING"), s("-P POSTROUTING ACCEPT\n-A POSTROUTING -s 192.168.1.10/32 -o wan -j MASQUERADE\n"));
    let _ = sh(&mut e, GW, "iptables -t nat -F");
    t.check("-F", e.nat(GW).is_some_and(|n| n.rules.is_empty() && !n.active()));
}

// the FORWARD chain: unasked from outside dropped, the LAN's answers in
fn forward_filter(t: &mut T) {
    let mut e = engine(MASQ, true);
    // MASQUERADE leaves the answers to connections from outside alone
    t.eq("no filter: WEB reaches PC1", pings(&mut e, WEB, "192.168.1.10", 2), 2);
    t.check("no filter: TCP from outside", tcp_ok(&mut e, WEB, "192.168.1.10", 41000, 7));
    t.check("no filter: no firewall", e.firewall(GW).is_some_and(|f| !f.active()));
    for l in FILTER.lines() {
        t.eq(l, sh(&mut e, GW, l), s(""));
    }
    let at = mark(&e);
    t.eq("unasked ping dropped", pings(&mut e, WEB, "192.168.1.10", 2), 0);
    t.check("dropped by the policy, noted", fw_notes(&e, at).iter().any(|n| matches!(n, FwNote::Drop { pair, class, .. } if pair == "iptables" && class == "-P FORWARD DROP")));
    t.check("unasked TCP dropped", !tcp_ok(&mut e, WEB, "192.168.1.10", 41001, 7));
    let at = mark(&e);
    t.eq("the LAN's ping answered", pings(&mut e, PC1, "203.0.113.10", 3), 3);
    let fn_ = fw_notes(&e, at);
    t.check("it began a session", fn_.iter().any(|n| matches!(n, FwNote::NewSession { pair, class, .. } if pair == "iptables" && class == "-A FORWARD -i br0 -o wan -j ACCEPT")));
    t.check("the answers belong to it", fn_.iter().any(|n| matches!(n, FwNote::Session { back: true, .. })));
    t.check("the LAN's TCP", tcp_ok(&mut e, PC1, "203.0.113.10", 41002, 80));
    // an ICMP error from outside about the LAN's packet: translated back, RELATED
    let at = mark(&e);
    let id = e.ping(PC1, ip("203.0.113.10"), PingOpts { count: 1, ttl: Some(2), ..PingOpts::default() }).unwrap_or(0);
    run_for(&mut e, 3 * SEC);
    let ev = match e.job(id).map(|j| &j.kind) {
        Some(JobKind::Ping(p)) => p.events.clone(),
        _ => Vec::new(),
    };
    t.check("time exceeded comes back", ev.iter().any(|x| matches!(x, PingEv::Error { kind: IcmpKind::TtlExceeded, .. })));
    t.check("as RELATED", fw_notes(&e, at).iter().any(|n| matches!(n, FwNote::Related { .. })));
    t.eq(
        "-L FORWARD -n",
        sh(&mut e, GW, "iptables -L FORWARD -n"),
        s("Chain FORWARD (policy DROP)\ntarget     prot opt source               destination         \nACCEPT     all  --  0.0.0.0/0            0.0.0.0/0            ctstate RELATED,ESTABLISHED\nACCEPT     all  --  0.0.0.0/0            0.0.0.0/0           \n"),
    );
    t.eq("-S", sh(&mut e, GW, "iptables -S"), format!("-P INPUT ACCEPT\n-P FORWARD DROP\n-P OUTPUT ACCEPT\n{}", FILTER.replace("iptables -P FORWARD DROP\n", "").replace("iptables ", "")));
    let v = sh(&mut e, GW, "iptables -L FORWARD -v -n --line-numbers");
    let lines: Vec<&str> = v.lines().collect();
    t.check("-v: the policy counted", lines.first().is_some_and(|l| l.starts_with("Chain FORWARD (policy DROP ") && !l.contains("DROP 0 packets")));
    t.eq("-v --line-numbers: header", lines.get(1).copied(), Some("num   pkts bytes target     prot opt in     out     source               destination         "));
    t.check("-v: the rules with their interfaces", lines.get(3).is_some_and(|l| l.starts_with("2    ") && l.contains(" ACCEPT     all  --  br0    wan     0.0.0.0/0")));
    t.check("-L without a chain: three", sh(&mut e, GW, "iptables -L").matches("Chain ").count() == 3);
    // REJECT from the top: a port unreachable for WEB
    let _ = sh(&mut e, GW, "iptables -I FORWARD 1 -i wan -p tcp --dport 7 -j REJECT");
    let at = mark(&e);
    t.check("REJECT: no connection", !tcp_ok(&mut e, WEB, "192.168.1.10", 41003, 7));
    t.check("REJECT: noted", fw_notes(&e, at).iter().any(|n| matches!(n, FwNote::Reject { class, .. } if class == "-A FORWARD -i wan -p tcp -m tcp --dport 7 -j REJECT --reject-with icmp-port-unreachable")));
    t.check("REJECT: port unreachable", notes(&e, GW, at).iter().any(|n| matches!(n, Note::IcmpSent { typ: 3, code: 3, to } if *to == ip("203.0.113.10"))));
    t.check("REJECT in -L", sh(&mut e, GW, "iptables -L FORWARD -n").contains("REJECT     tcp  --  0.0.0.0/0            0.0.0.0/0            tcp dpt:7 reject-with icmp-port-unreachable\n"));
    t.check("the LAN still gets out", pings(&mut e, PC2, "203.0.113.10", 2) == 2);
    // -D by number and by words, -F, the policy back: nothing left to do
    t.eq("-D by number", sh(&mut e, GW, "iptables -D FORWARD 1"), s(""));
    t.eq("-D by number: too big", sh(&mut e, GW, "iptables -D FORWARD 9"), s("iptables: Index of deletion too big.\n"));
    t.eq("-I: too far", sh(&mut e, GW, "iptables -I FORWARD 9 -j DROP"), s("iptables: Index of insertion too big.\n"));
    t.eq("-D by words", sh(&mut e, GW, "iptables -D FORWARD -i br0 -o wan -j ACCEPT"), s(""));
    t.eq("-D by words: one left", e.firewall(GW).map(|f| f.fwd.rules.len()), Some(1));
    let _ = sh(&mut e, GW, "iptables -F");
    t.check("-F keeps the policy", e.firewall(GW).is_some_and(|f| f.fwd.rules.is_empty() && f.fwd.drop && f.active()));
    t.eq("policy DROP alone: the LAN stays in", pings(&mut e, PC1, "203.0.113.10", 2), 0);
    let _ = sh(&mut e, GW, "iptables -P FORWARD ACCEPT");
    t.check("nothing left: the firewall sleeps, its sessions are gone", e.firewall(GW).is_some_and(|f| !f.active() && f.sessions().next().is_none()));
    let at = mark(&e);
    t.eq("open again", pings(&mut e, WEB, "192.168.1.10", 2), 2);
    t.check("and no firewall note", fw_notes(&e, at).is_empty());
}

// what the original would complain about, in our words
fn refusals(t: &mut T) {
    let mut e = engine("", false);
    let says = |e: &mut Engine, line: &str, what: &str| sh(e, GW, line).contains(what);
    t.check("unknown option", says(&mut e, "iptables -A FORWARD --foo", "iptables: unknown option \"--foo\"\nTry `iptables -h' or 'iptables --help' for more information.\n"));
    t.check("its status", sh(&mut e, GW, "iptables -A FORWARD --foo; echo $?").ends_with("\n2\n"));
    t.check("no command", says(&mut e, "iptables", "iptables: no command specified"));
    t.check("two commands", says(&mut e, "iptables -A FORWARD -D FORWARD", "iptables: cannot use -D with -A"));
    t.check("--dport without -p", says(&mut e, "iptables -A FORWARD --dport 80 -j ACCEPT", "unknown option \"--dport\""));
    t.check("--ctstate without -m conntrack", says(&mut e, "iptables -A FORWARD --ctstate NEW -j ACCEPT", "unknown option \"--ctstate\""));
    t.check("a bad state", says(&mut e, "iptables -A FORWARD -m conntrack --ctstate NEU -j ACCEPT", "iptables: Bad ctstate \"NEU\""));
    t.check("a bad protocol", says(&mut e, "iptables -A FORWARD -p hugo -j ACCEPT", "unknown protocol \"hugo\""));
    t.check("a bad address", says(&mut e, "iptables -A FORWARD -s 10.0.0.300 -j ACCEPT", "host/network `10.0.0.300' not found"));
    t.check("a bad port", says(&mut e, "iptables -A FORWARD -p tcp --dport 0 -j ACCEPT", "invalid port/service `0'"));
    t.check("no chain by that name", says(&mut e, "iptables -L FOO", "iptables: No chain/target/match by that name.\n"));
    t.check("INPUT takes no rules", says(&mut e, "iptables -A INPUT -j DROP", "the chain INPUT takes no rules here: the device's own traffic is ufw's"));
    t.check("PREROUTING takes no rules", says(&mut e, "iptables -t nat -A PREROUTING -j MASQUERADE", "the chain PREROUTING takes no rules here"));
    t.check("a target not modelled", says(&mut e, "iptables -A FORWARD -j LOG", "the target LOG is not modelled here"));
    t.check("an unknown target", says(&mut e, "iptables -A FORWARD -j HUGO", "Couldn't load target `HUGO'"));
    t.check("MASQUERADE in filter", says(&mut e, "iptables -A FORWARD -o wan -j MASQUERADE", "MASQUERADE belongs to the table nat"));
    t.check("MASQUERADE without -o", says(&mut e, "iptables -t nat -A POSTROUTING -j MASQUERADE", "MASQUERADE needs -o IF here"));
    t.check("-i with POSTROUTING", says(&mut e, "iptables -t nat -A POSTROUTING -i br0 -o wan -j MASQUERADE", "Can't use -i with POSTROUTING"));
    t.check("a rule needs -j", says(&mut e, "iptables -A FORWARD -i br0", "a rule needs -j here"));
    t.check("a table not modelled", says(&mut e, "iptables -t mangle -L", "the table mangle is not modelled here"));
    t.check("a bad policy", says(&mut e, "iptables -P FORWARD REJECT", "Bad policy name"));
    t.check("INPUT keeps ACCEPT", says(&mut e, "iptables -P INPUT DROP", "the chain INPUT keeps the policy ACCEPT here"));
    t.eq("ACCEPT where it is ACCEPT anyway", sh(&mut e, GW, "iptables -P OUTPUT ACCEPT"), s(""));
    t.check("! is not modelled", says(&mut e, "iptables -A FORWARD ! -i br0 -j DROP", "\"!\" (not) is not modelled here"));
    t.check("user chains are not modelled", says(&mut e, "iptables -N MEINE", "-N is not modelled here"));
    t.check("-h is honest", says(&mut e, "iptables -h", "INPUT and OUTPUT stay empty"));
    t.check("man knows it", sh(&mut e, GW, "man iptables").contains("Paketfilter und NAT für das, was dieses Gerät weiterleitet"));
    t.check("nothing of it stuck", e.firewall(GW).is_some_and(|f| !f.active()) && e.nat(GW).is_some_and(|n| n.rules.is_empty()));
}

// running configuration -> a new engine -> the same lines and behaviour; an interface added later
fn round_trip(t: &mut T) {
    let mut e = engine("", true);
    for l in [MASQ.trim_end(), "iptables -P FORWARD DROP", "iptables -A FORWARD -m state --state ESTABLISHED,RELATED -j ACCEPT", "iptables -A FORWARD -i br0 -o wan -j ACCEPT", "iptables -A FORWARD -s 203.0.113.0/24 -d 192.168.1.10 -p tcp --dport 7 -j ACCEPT", "iptables -I FORWARD 2 -i wan -p icmp -j REJECT --reject-with icmp-admin-prohibited"] {
        t.eq(l, sh(&mut e, GW, l), s(""));
    }
    let cfg = e.running_config(GW);
    let want = "iptables -t nat -A POSTROUTING -o wan -j MASQUERADE\niptables -P FORWARD DROP\niptables -A FORWARD -m state --state RELATED,ESTABLISHED -j ACCEPT\niptables -A FORWARD -i wan -p icmp -j REJECT --reject-with icmp-admin-prohibited\niptables -A FORWARD -i br0 -o wan -j ACCEPT\niptables -A FORWARD -s 203.0.113.0/24 -d 192.168.1.10/32 -p tcp -m tcp --dport 7 -j ACCEPT\n";
    t.check("in the configuration", cfg.contains(want));
    let mut e2 = boot(&project(&cfg, true, true));
    t.eq("it boots to the same", e2.running_config(GW), cfg);
    t.eq("the LAN gets out", pings(&mut e2, PC1, "203.0.113.10", 2), 2);
    let at = mark(&e2);
    t.eq("a ping from outside is rejected", pings(&mut e2, WEB, "192.168.1.10", 1), 0);
    t.check("admin prohibited", notes(&e2, GW, at).iter().any(|n| matches!(n, Note::IcmpSent { typ: 3, code: 13, .. })));
    t.check("the port that is open", tcp_ok(&mut e2, WEB, "192.168.1.10", 42000, 7));
    t.check("-m state as written", sh(&mut e2, GW, "iptables -L FORWARD -n").contains(" state RELATED,ESTABLISHED\n"));
    let (pk, by) = e2.firewall(GW).map_or((0, 0), |f| (f.fwd.packets, f.fwd.bytes));
    let rules: String = want.lines().filter(|l| l.starts_with("iptables -A FORWARD")).map(|l| format!("{}\n", &l[9..])).collect();
    t.eq(
        "iptables-save",
        sh(&mut e2, GW, "iptables-save"),
        format!("*filter\n:INPUT ACCEPT [0:0]\n:FORWARD DROP [{}:{}]\n:OUTPUT ACCEPT [0:0]\n{}COMMIT\n*nat\n:PREROUTING ACCEPT [0:0]\n:INPUT ACCEPT [0:0]\n:OUTPUT ACCEPT [0:0]\n:POSTROUTING ACCEPT [0:0]\n-A POSTROUTING -o wan -j MASQUERADE\nCOMMIT\n", pk, by, rules),
    );
    t.check("iptables-save -t nat", sh(&mut e2, GW, "iptables-save -t nat").starts_with("*nat\n"));
    // an interface that comes later is inside; one that MASQUERADE names before it exists is outside
    let _ = sh(&mut e2, GW, "iptables -t nat -A POSTROUTING -o wan.20 -j MASQUERADE");
    let _ = sh(&mut e2, GW, "ip link add link wan name wan.10 type vlan id 10");
    let _ = sh(&mut e2, GW, "ip link add link wan name wan.20 type vlan id 20");
    let side = |e: &Engine, name: &str| e.ifaces(GW).iter().find(|f| f.name == name).and_then(|f| e.nat(GW).and_then(|n| n.side(f.index)));
    t.eq("a new interface: inside", side(&e2, "wan.10"), Some(Side::Inside));
    t.eq("a named one: outside", side(&e2, "wan.20"), Some(Side::Outside));
    t.eq("wan stays outside", side(&e2, "wan"), Some(Side::Outside));
    let _ = sh(&mut e2, GW, "ip link del wan.10");
    t.eq("gone: no side", side(&e2, "wan.10"), None);
    t.eq("NAT still works", pings(&mut e2, PC2, "203.0.113.10", 2), 2);
}

// a home router from the palette brings NAT and the filter; files keep what they say
fn palette(t: &mut T) {
    let mut doc = Doc::new();
    doc.replace(Project::new("p", 1), None);
    let Ok((id, e)) = edits::add_placed(&mut doc.proj, "home-router", 0, 0) else {
        t.check("placed", false);
        return;
    };
    doc.record(e);
    let d = doc.proj.device(id).cloned().unwrap_or_default();
    t.eq("the defaults", d.startup_config.as_str(), catalog::HOME_ROUTER_STARTUP);
    t.check("in one step", doc.undo() && doc.proj.device(id).is_none() && doc.redo());
    t.eq("redo brings them", doc.proj.device(id).map(|x| x.startup_config.clone()), Some(s(catalog::HOME_ROUTER_STARTUP)));
    t.check("another tile brings none", edits::add_placed(&mut doc.proj, "pc", 0, 0).is_ok_and(|(i, _)| doc.proj.device(i).is_some_and(|x| x.startup_config.is_empty())));
    let mut p = Project::new("file", 1);
    let h = p.add_device("home-router", 0, 0).unwrap_or(0);
    t.check("a file's home router has none", p.device(h).is_some_and(|x| x.startup_config.is_empty()));
    // the placed router at work: addresses typed at its console, the rest from the defaults
    let mut e = boot(&project(catalog::HOME_ROUTER_STARTUP, true, true));
    for l in GW_BASE.lines().skip(1) {
        let _ = sh(&mut e, GW, l);
    }
    // (ISP asked for GW's address before it had one)
    let _ = e.ping(ISP, ip("100.64.0.2"), PingOpts { count: 1, ..PingOpts::default() });
    run_for(&mut e, 2 * SEC);
    t.check("NAT and filter at once", e.nat(GW).is_some_and(|n| n.masquerades()) && e.firewall(GW).is_some_and(|f| f.fwd.drop && f.fwd.rules.len() == 2));
    t.eq("the LAN gets out", pings(&mut e, PC1, "203.0.113.10", 3), 3);
    t.check("translated", e.nat_translations(GW).iter().any(|r| r.inside_local.0 == ip("192.168.1.10")));
    t.eq("nothing comes in unasked", pings(&mut e, WEB, "192.168.1.10", 2), 0);
    t.check("the running configuration has the same lines", e.running_config(GW).contains(catalog::HOME_ROUTER_STARTUP));
    // the template "Heimnetz": its gateway as it was
    let home = Project::from_text(include_str!("../assets/templates/heimnetz.jfnet")).unwrap_or_default();
    let gw = home.find("Gateway1").map(|x| x.id).unwrap_or(0);
    t.check("the template's gateway has no iptables", home.device(gw).is_some_and(|x| !x.startup_config.contains("iptables")));
    let mut e = Engine::from_project(&home).unwrap_or_else(|_| Engine::new(0));
    run_for(&mut e, SEC);
    t.check("and neither NAT nor a filter", e.nat(gw).is_some_and(|n| !n.active()) && e.firewall(gw).is_some_and(|f| !f.active()));
}

// a DHCP server the way "Dienst hinzufügen" adds it
fn dhcp_dialog(t: &mut T) {
    let mut p = project(&format!("{}{}", MASQ, FILTER), false, false);
    if let Some(d) = p.device_mut(PC2) {
        d.startup_config = s("hostname PC2\n");
        d.services.push(ServiceDef { kind: s("dhcp-client"), name: s("dhcp-client@eth0"), disabled: false, config: agent_core::json::parse(b"{\"iface\": \"eth0\"}").unwrap_or(Value::Null) });
    }
    let e = boot(&p);
    let dev = |id: u32| p.device(id).cloned().unwrap_or_default();
    t.check("the dialog offers it on a Linux device", svcform::add_kinds(&dev(GW)).last() == Some(&"dhcp-server") && svcform::add_kinds(&dev(WEB)).contains(&"dhcp-server"));
    t.check("not on an IOS-like one", !svcform::add_kinds(&dev(ISP)).contains(&"dhcp-server"));
    t.eq("its usual name", svcform::free_name(&dev(GW), "dhcp-server"), s("dhcp-server"));
    let Ok(def) = svcform::new_def(&dev(GW), "dhcp-server", " dhcp-server ", &e) else {
        t.check("a new DHCP server", false);
        return;
    };
    let want = agent_core::json::parse(br#"{"pools": [{"name": "LAN", "network": "192.168.1.0/24", "range": ["192.168.1.100", "192.168.1.199"], "routers": ["192.168.1.1"], "lease": 3600}]}"#).unwrap_or(Value::Null);
    t.eq("a pool for br0's network", def.config.clone(), want);
    t.eq("the name trimmed", def.name.as_str(), "dhcp-server");
    t.check("the simulator takes it", svcedit::trial(&dev(GW), &def).is_ok());
    let srv = svcform::new_config("dhcp-server", &e, WEB);
    t.eq("a server: its own network", srv.get("pools").and_then(|x| x.as_array()).and_then(|a| a.first()).and_then(|x| x.get("network")).and_then(|x| x.as_str()), Some("203.0.113.0/24"));
    t.eq("another kind starts empty", svcform::new_config("http-server", &e, GW), agent_core::jmap::empty_object());
    t.check("an empty name is refused", svcform::new_def(&dev(GW), "dhcp-server", "  ", &e).is_err_and(|m| m == "Der Dienst braucht einen Namen."));
    // added: PC2 gets an address and goes out through NAT and the filter
    if let Some(d) = p.device_mut(GW) {
        d.services.push(def.clone());
    }
    let gw = p.device(GW).cloned().unwrap_or_default();
    t.check("a name that is taken is refused", svcform::new_def(&gw, "dhcp-server", "DHCP-Server", &e).is_err_and(|m| m == "GW hat schon einen Dienst „DHCP-Server“."));
    let mut e = boot(&p);
    run_for(&mut e, 10 * SEC);
    let addr = e.ifaces(PC2).iter().find(|f| f.name == "eth0").and_then(|f| f.addrs.first().map(|a| a.addr));
    t.eq("PC2 has an address from the pool", addr, Some(ip("192.168.1.199")));
    t.eq("and gets out", pings(&mut e, PC2, "203.0.113.10", 2), 2);
}

// an IOS-like device takes none of it, a home router without the lines does nothing new
fn ios_untouched(t: &mut T) {
    let mut e = engine("", true);
    let masq = Rule::Masquerade { iface: s("GigabitEthernet0/0/0"), src: None };
    t.check("no MASQUERADE on IOS", e.apply(Action::Nat { dev: ISP, act: NatAct::Rule { rule: masq, add: true } }).is_err());
    t.check("no FORWARD chain on IOS", e.apply(Action::Fw { dev: ISP, op: FwOp::Fwd(FwdOp::Policy(true)) }).is_err());
    let at = mark(&e);
    t.eq("routing as before", pings(&mut e, PC1, "203.0.113.10", 2), 2);
    t.check("no NAT or firewall note", !notes(&e, GW, at).iter().any(|n| matches!(n, Note::Fw(_) | Note::NatCreate { .. } | Note::NatTranslate { .. } | Note::NatNoRule { .. })));
}
