// iptables -- packet filter and NAT for what a Linux device forwards, on its shell: the FORWARD chain of
// the table filter (fw.rs: FwdChain, on the sessions of the host firewall) and MASQUERADE in the table
// nat (nat.rs: Rule::Masquerade, on the engine of IOS-like NAT).  The words of the commands and the
// layout of -L and -S follow the original; the help and the refusals are our own words
//
//   Engine::sh_iptables(ni, args)       the command: -A, -I, -D, -C, -F, -Z, -P, -L, -S, -h
//   Engine::sh_iptables_save(ni, args)  the tables in the format of iptables-save ([-t TABLE] [-c])
//   config(&IpStack)                    the lines of the device's configuration (linux_config): the
//                                       MASQUERADE rules, the policy, the rules of FORWARD
//   parse(args) -> Result<Cmd, String>  the words as a command (Err: the complaint, exit status 2)
//
// The table filter has the chains INPUT, FORWARD and OUTPUT, nat PREROUTING, INPUT, OUTPUT and
// POSTROUTING; only FORWARD and POSTROUTING take rules here: what the device sends and receives
// itself is ufw's (cli/ufw.rs), DNAT and port forwarding are not modelled.  The other chains list as
// empty with the policy ACCEPT and refuse rules with that reason.  A rule of FORWARD:
//   [-i IF] [-o IF] [-p tcp|udp|icmp|all|N] [--dport PORT] [-s NET] [-d NET]
//   [-m conntrack --ctstate S,.. | -m state --state S,..] -j ACCEPT|DROP|REJECT [--reject-with T]
// and of POSTROUTING: [-s NET] -o IF -j MASQUERADE.  Options come in any order, as with the original
// "-p" before "--dport", "-m" before its options and "-j REJECT" before "--reject-with"; "-nvL" is
// "-n -v -L".  Not here: "!", user chains (-N, -X, -E), -R, other matches and targets, ip6tables.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use netpkt::icmp::UNREACH_PORT;
use netpkt::ipv4::{PROTO_ICMP, PROTO_TCP, PROTO_UDP};
use netpkt::{Ipv4, Prefix4};

use crate::action::Action;
use crate::fw::{ct_words, ipt_proto, FwOp, FwdOp, FwdRule, FwdTarget, CT_WORDS, REJECT_WITH};
use crate::ip::IpStack;
use crate::kernel::Engine;
use crate::nat::{NatAct, Rule};

const TRY: &str = "Try `iptables -h' or 'iptables --help' for more information.\n";

const HELP: &str = "Usage: iptables [-t nat] -A|-C CHAIN RULE\n       iptables [-t nat] -I CHAIN [N] RULE\n       iptables [-t nat] -D CHAIN RULE|N\n       iptables -P FORWARD ACCEPT|DROP\n       iptables [-t nat] -F|-Z [CHAIN]\n       iptables [-t nat] -L [CHAIN] [-n] [-v] [-x] [--line-numbers]\n       iptables [-t nat] -S [CHAIN]\nA rule of FORWARD (table filter, what this device forwards):\n  [-i IF] [-o IF] [-p tcp|udp|icmp|all] [--dport PORT] [-s NET] [-d NET]\n  [-m conntrack --ctstate STATES | -m state --state STATES]\n  -j ACCEPT|DROP|REJECT [--reject-with icmp-port-unreachable|icmp-host-unreachable|\n                                         icmp-net-unreachable|icmp-admin-prohibited]\n  STATES: NEW, ESTABLISHED, RELATED, INVALID, separated by commas\nA rule of POSTROUTING (table nat): [-s NET] -o IF -j MASQUERADE\nThe first rule that matches decides, else the policy. Accepted packets begin a session, so\nanswers are ESTABLISHED. In this simulator INPUT and OUTPUT stay empty (the device's own\ntraffic is ufw's), PREROUTING has no rules, and IPv6 is not filtered.\n";

/// the chains of a table
fn chains(nat: bool) -> &'static [&'static str] {
    if nat { &["PREROUTING", "INPUT", "OUTPUT", "POSTROUTING"] } else { &["INPUT", "FORWARD", "OUTPUT"] }
}

/// targets the original knows that this simulator does not model
const OTHER_TARGETS: &[&str] = &["LOG", "RETURN", "SNAT", "DNAT", "REDIRECT", "MARK", "CONNMARK", "TCPMSS", "CT", "NOTRACK", "QUEUE", "NFQUEUE", "TPROXY", "AUDIT", "CLASSIFY", "NFLOG", "TEE", "TRACE", "ULOG"];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Op {
    #[default]
    None,
    Append,
    Insert,
    Delete,
    Check,
    Flush,
    Zero,
    Policy,
    List,
    Rules,
    Help,
}

/// a command line as read
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Cmd {
    pub nat: bool,
    pub op: Op,
    pub chain: Option<String>,
    /// -I CHAIN N, -D CHAIN N (from 1)
    pub num: Option<usize>,
    pub policy: Option<String>,
    pub numeric: bool,
    pub verbose: bool,
    pub exact: bool,
    pub lines: bool,
    pub inif: Option<String>,
    pub outif: Option<String>,
    pub proto: Option<u8>,
    pub dport: Option<u16>,
    pub src: Option<Prefix4>,
    pub dst: Option<Prefix4>,
    pub state: u8,
    pub state_mod: bool,
    pub target: Option<String>,
    pub reject: Option<u8>,
    /// some option of a rule was given
    pub ruled: bool,
}

fn proto_of(w: &str) -> Result<u8, String> {
    match w {
        "all" => Ok(0),
        "tcp" => Ok(PROTO_TCP),
        "udp" => Ok(PROTO_UDP),
        "icmp" => Ok(PROTO_ICMP),
        _ => w.parse::<u8>().map_err(|_| format!("iptables: unknown protocol \"{}\" specified\n{}", w, TRY)),
    }
}

fn net_of(w: &str) -> Result<Prefix4, String> {
    let p = if w.contains('/') { Prefix4::parse(w).map(|p| p.trunc()) } else { Ipv4::parse(w).map(|addr| Prefix4 { addr, len: 32 }) };
    p.ok_or_else(|| format!("iptables: host/network `{}' not found\n{}", w, TRY))
}

fn port_of(w: &str) -> Result<u16, String> {
    let named = [("ftp", 21), ("ssh", 22), ("telnet", 23), ("smtp", 25), ("domain", 53), ("http", 80), ("pop3", 110), ("ntp", 123), ("imap", 143), ("snmp", 161), ("https", 443)];
    if let Some((_, p)) = named.iter().find(|(n, _)| *n == w) {
        return Ok(*p);
    }
    match w.parse::<u16>() {
        Ok(p) if p > 0 => Ok(p),
        _ => Err(format!("iptables: invalid port/service `{}' specified\n{}", w, TRY)),
    }
}

fn states_of(w: &str, opt: &str) -> Result<u8, String> {
    let mut bits = 0;
    for s in w.split(',') {
        match CT_WORDS.iter().find(|(_, x)| *x == s) {
            Some((b, _)) => bits |= b,
            None => return Err(format!("iptables: Bad {} \"{}\"\n", opt, s)),
        }
    }
    Ok(bits)
}

/// the words of a command (the header has them)
pub fn parse(args: &[&str]) -> Result<Cmd, String> {
    // "-nvL" is "-n -v -L"
    let mut w: Vec<String> = Vec::new();
    for a in args {
        if a.len() > 2 && a.starts_with('-') && !a.starts_with("--") && a[1..].chars().all(|c| "nvxLSFZ".contains(c)) {
            w.extend(a[1..].chars().map(|c| format!("-{}", c)));
        } else {
            w.push(a.to_string());
        }
    }
    let mut c = Cmd::default();
    let mut said: Option<String> = None;
    let mut modules: Vec<String> = Vec::new();
    let mut i = 0;
    while i < w.len() {
        let a = w[i].clone();
        let arg = |k: usize| w.get(k + 1).cloned().ok_or_else(|| format!("iptables: option \"{}\" requires an argument\n{}", a, TRY));
        // the chain a command may name: a word that is no option
        let word = |k: usize| w.get(k + 1).filter(|x| !x.starts_with('-')).cloned();
        let op = match a.as_str() {
            "-A" | "--append" => Op::Append,
            "-I" | "--insert" => Op::Insert,
            "-D" | "--delete" => Op::Delete,
            "-C" | "--check" => Op::Check,
            "-F" | "--flush" => Op::Flush,
            "-Z" | "--zero" => Op::Zero,
            "-P" | "--policy" => Op::Policy,
            "-L" | "--list" => Op::List,
            "-S" | "--list-rules" => Op::Rules,
            "-h" | "--help" => Op::Help,
            _ => Op::None,
        };
        if op != Op::None {
            if let Some(prev) = &said {
                return Err(format!("iptables: cannot use {} with {}\n{}", a, prev, TRY));
            }
            said = Some(a.clone());
            c.op = op;
            match op {
                Op::Append | Op::Insert | Op::Delete | Op::Check => {
                    c.chain = Some(arg(i)?);
                    i += 1;
                    if matches!(op, Op::Insert | Op::Delete)
                        && let Some(n) = word(i).filter(|n| n.bytes().all(|b| b.is_ascii_digit()))
                    {
                        match n.parse::<usize>() {
                            Ok(k) if k > 0 => c.num = Some(k),
                            _ => return Err(format!("iptables: Invalid rule number `{}'\n{}", n, TRY)),
                        }
                        i += 1;
                    }
                }
                Op::Policy => {
                    let (Some(ch), Some(p)) = (word(i), word(i + 1)) else {
                        return Err(format!("iptables: -P requires a chain and a policy\n{}", TRY));
                    };
                    c.chain = Some(ch);
                    c.policy = Some(p);
                    i += 2;
                }
                Op::Flush | Op::Zero | Op::List | Op::Rules => {
                    if let Some(ch) = word(i) {
                        c.chain = Some(ch);
                        i += 1;
                    }
                }
                _ => {}
            }
            i += 1;
            continue;
        }
        match a.as_str() {
            "-t" | "--table" => {
                let t = arg(i)?;
                i += 1;
                match t.as_str() {
                    "filter" => c.nat = false,
                    "nat" => c.nat = true,
                    "mangle" | "raw" | "security" => return Err(format!("iptables: the table {} is not modelled here (filter and nat are)\n", t)),
                    _ => return Err(format!("iptables: can't initialize iptables table `{}': Table does not exist\n", t)),
                }
            }
            "-N" | "--new-chain" | "-X" | "--delete-chain" | "-E" | "--rename-chain" | "-R" | "--replace" => {
                return Err(format!("iptables: {} is not modelled here (no chains of your own, no replacing)\n", a));
            }
            "-n" | "--numeric" => c.numeric = true,
            "-v" | "--verbose" => c.verbose = true,
            "-x" | "--exact" => c.exact = true,
            "--line-numbers" => c.lines = true,
            "-i" | "--in-interface" => {
                c.inif = Some(arg(i)?);
                i += 1;
                c.ruled = true;
            }
            "-o" | "--out-interface" => {
                c.outif = Some(arg(i)?);
                i += 1;
                c.ruled = true;
            }
            "-p" | "--protocol" => {
                c.proto = Some(proto_of(&arg(i)?)?);
                i += 1;
                c.ruled = true;
            }
            "-s" | "--source" | "--src" => {
                c.src = Some(net_of(&arg(i)?)?);
                i += 1;
                c.ruled = true;
            }
            "-d" | "--destination" | "--dst" => {
                c.dst = Some(net_of(&arg(i)?)?);
                i += 1;
                c.ruled = true;
            }
            "-m" | "--match" => {
                let m = arg(i)?;
                i += 1;
                match m.as_str() {
                    "tcp" | "udp" => {
                        if c.proto != Some(if m == "tcp" { PROTO_TCP } else { PROTO_UDP }) {
                            return Err(format!("iptables: the match {} needs \"-p {}\" before it\n{}", m, m, TRY));
                        }
                    }
                    "conntrack" | "state" => modules.push(m),
                    _ => return Err(format!("iptables: the match {} is not modelled here (tcp, udp, conntrack and state are)\n", m)),
                }
                c.ruled = true;
            }
            "--dport" | "--destination-port" => {
                if !matches!(c.proto, Some(PROTO_TCP | PROTO_UDP)) {
                    return Err(format!("iptables: unknown option \"{}\"\n{}", a, TRY));
                }
                c.dport = Some(port_of(&arg(i)?)?);
                i += 1;
            }
            "--ctstate" | "--state" => {
                let m = if a == "--state" { "state" } else { "conntrack" };
                if !modules.iter().any(|x| x == m) {
                    return Err(format!("iptables: unknown option \"{}\"\n{}", a, TRY));
                }
                c.state = states_of(&arg(i)?, &a[2..])?;
                c.state_mod = m == "state";
                i += 1;
            }
            "-j" | "--jump" => {
                c.target = Some(arg(i)?);
                i += 1;
                c.ruled = true;
            }
            "--reject-with" => {
                if c.target.as_deref() != Some("REJECT") {
                    return Err(format!("iptables: unknown option \"{}\"\n{}", a, TRY));
                }
                let t = arg(i)?;
                i += 1;
                match REJECT_WITH.iter().find(|r| r.0 == t) {
                    Some(r) => c.reject = Some(r.1),
                    None => return Err(format!("iptables: unknown reject type \"{}\" (here: {})\n", t, REJECT_WITH.iter().map(|r| r.0).collect::<Vec<_>>().join(", "))),
                }
            }
            "!" => return Err(String::from("iptables: \"!\" (not) is not modelled here\n")),
            x if x.starts_with('-') => return Err(format!("iptables: unknown option \"{}\"\n{}", x, TRY)),
            x => return Err(format!("Bad argument `{}'\n{}", x, TRY)),
        }
        i += 1;
    }
    if said.is_none() {
        return Err(format!("iptables: no command specified\n{}", TRY));
    }
    Ok(c)
}

/// what a command does with the rule it names
enum Built {
    Fwd(FwdRule),
    Masq(Rule),
}

/// the rule of a command (-A, -I, -D, -C): Err with the complaint
fn build(c: &Cmd) -> Result<Built, String> {
    let chain = c.chain.as_deref().unwrap_or("");
    let want = if c.nat { "POSTROUTING" } else { "FORWARD" };
    if chain != want {
        return Err(if !chains(c.nat).contains(&chain) {
            String::from("iptables: No chain/target/match by that name.\n")
        } else if c.nat {
            format!("iptables: the chain {} takes no rules here: the table nat knows POSTROUTING with MASQUERADE\n", chain)
        } else {
            format!("iptables: the chain {} takes no rules here: the device's own traffic is ufw's\n", chain)
        });
    }
    let Some(target) = c.target.as_deref() else {
        return Err(format!("iptables: a rule needs -j here ({})\n", if c.nat { "MASQUERADE" } else { "ACCEPT, DROP or REJECT" }));
    };
    let known = ["ACCEPT", "DROP", "REJECT", "MASQUERADE"].contains(&target) || OTHER_TARGETS.contains(&target);
    if !known {
        return Err(format!("iptables: Couldn't load target `{}':No such file or directory\n{}", target, TRY));
    }
    if c.nat {
        if target != "MASQUERADE" {
            return Err(String::from("iptables: the table nat takes MASQUERADE only here\n"));
        }
        if c.inif.is_some() {
            return Err(String::from("iptables: Can't use -i with POSTROUTING\n"));
        }
        if c.proto.is_some() || c.dst.is_some() || c.state != 0 {
            return Err(String::from("iptables: in the table nat this simulator takes -s and -o only\n"));
        }
        let Some(iface) = c.outif.clone().filter(|x| !x.ends_with('+')) else {
            return Err(String::from("iptables: MASQUERADE needs -o IF here: the interface whose address it takes\n"));
        };
        return Ok(Built::Masq(Rule::Masquerade { iface, src: c.src }));
    }
    let target = match target {
        "ACCEPT" => FwdTarget::Accept,
        "DROP" => FwdTarget::Drop,
        "REJECT" => FwdTarget::Reject(c.reject.unwrap_or(UNREACH_PORT)),
        "MASQUERADE" => return Err(String::from("iptables: MASQUERADE belongs to the table nat: -t nat -A POSTROUTING -o IF -j MASQUERADE\n")),
        t => return Err(format!("iptables: the target {} is not modelled here (ACCEPT, DROP and REJECT are)\n", t)),
    };
    let proto = c.proto.filter(|p| *p != 0);
    Ok(Built::Fwd(FwdRule { inif: c.inif.clone(), outif: c.outif.clone(), proto, dport: c.dport, src: c.src, dst: c.dst, state: c.state, state_mod: c.state_mod, target, packets: 0, bytes: 0 }))
}

/// the rule of MASQUERADE as "-S" writes it after "-A POSTROUTING"
fn masq_spec(r: &Rule) -> String {
    match r {
        Rule::Masquerade { iface, src: Some(n) } => format!("-s {} -o {} -j MASQUERADE", n, iface),
        Rule::Masquerade { iface, src: None } => format!("-o {} -j MASQUERADE", iface),
        _ => String::new(),
    }
}

/// the lines of the device's configuration
pub fn config(st: &IpStack) -> String {
    let mut s = String::new();
    for (_, r) in st.nat.rules.iter().filter(|(_, r)| matches!(r, Rule::Masquerade { .. })) {
        s.push_str(&format!("iptables -t nat -A POSTROUTING {}\n", masq_spec(r)));
    }
    if st.fw.fwd.drop {
        s.push_str("iptables -P FORWARD DROP\n");
    }
    for r in &st.fw.fwd.rules {
        s.push_str(&format!("iptables -A FORWARD {}\n", r.spec()));
    }
    s
}

// ---------------------------------------------------------------- -L

/// how -L lays out
#[derive(Clone, Copy)]
struct Look {
    numeric: bool,
    verbose: bool,
    exact: bool,
    lines: bool,
}

/// a counter in its column (K, M, G, T unless exact), or in running text
fn count(n: u64, exact: bool, table: bool) -> String {
    let (v, unit, width) = if exact {
        (n, "", 8)
    } else if n <= 99_999 {
        (n, "", 5)
    } else {
        let (mut v, mut u) = ((n + 500) / 1000, "K");
        for next in ["M", "G", "T"] {
            if v <= 9999 {
                break;
            }
            v = (v + 500) / 1000;
            u = next;
        }
        (v, u, 4)
    };
    if table { format!("{:>w$}{} ", v, unit, w = width) } else { format!("{}{} ", v, unit) }
}

/// a row of -L
struct Row {
    pkts: u64,
    bytes: u64,
    target: String,
    proto: u8,
    inif: Option<String>,
    outif: Option<String>,
    src: Option<Prefix4>,
    dst: Option<Prefix4>,
    /// what the matches and the target add (each with a space before it)
    extra: String,
}

fn addr_text(n: &Option<Prefix4>, numeric: bool) -> String {
    match n {
        None if !numeric => String::from("anywhere"),
        None => String::from("0.0.0.0/0"),
        Some(p) if p.len == 32 => format!("{}", p.addr),
        Some(p) => format!("{}", p),
    }
}

/// a chain as -L lays it out
fn chain_text(name: &str, policy: &str, counts: (u64, u64), rows: &[Row], o: Look) -> String {
    let mut s = format!("Chain {} (policy {}", name, policy);
    if o.verbose {
        s.push_str(&format!(" {}packets, {}bytes", count(counts.0, o.exact, false), count(counts.1, o.exact, false)));
    }
    s.push_str(")\n");
    if o.lines {
        s.push_str(&format!("{:<4} ", "num"));
    }
    if o.verbose {
        if o.exact {
            s.push_str(&format!("{:>8} {:>10} ", "pkts", "bytes"));
        } else {
            s.push_str(&format!("{:>5} {:>5} ", "pkts", "bytes"));
        }
    }
    s.push_str(&format!("{:<9} ", "target"));
    s.push_str(" prot opt");
    if o.verbose {
        s.push_str(&format!(" {:<6} {:<6} ", "in", "out"));
    }
    s.push_str(&format!(" {:<19}  {:<19} \n", "source", "destination"));
    for (k, r) in rows.iter().enumerate() {
        if o.lines {
            s.push_str(&format!("{:<4} ", k + 1));
        }
        if o.verbose {
            s.push_str(&count(r.pkts, o.exact, true));
            s.push_str(&count(r.bytes, o.exact, true));
        }
        s.push_str(&format!("{:<9}  {:<5}-- ", r.target, ipt_proto(r.proto)));
        if o.verbose {
            let any = if o.numeric { "*" } else { "any" };
            s.push_str(&format!(" {:<6} {:<6} ", r.inif.as_deref().unwrap_or(any), r.outif.as_deref().unwrap_or(any)));
        }
        s.push_str(&format!(" {:<19}  {:<19} {}\n", addr_text(&r.src, o.numeric), addr_text(&r.dst, o.numeric), r.extra));
    }
    s
}

fn fwd_row(r: &FwdRule) -> Row {
    let mut extra = String::new();
    if let (Some(p), Some(d)) = (r.proto, r.dport) {
        extra.push_str(&format!(" {} dpt:{}", ipt_proto(p), d));
    }
    if r.state != 0 {
        extra.push_str(&format!(" {} {}", if r.state_mod { "state" } else { "ctstate" }, ct_words(r.state)));
    }
    let target = match r.target {
        FwdTarget::Accept => "ACCEPT",
        FwdTarget::Drop => "DROP",
        FwdTarget::Reject(c) => {
            extra.push_str(&format!(" reject-with {}", REJECT_WITH.iter().find(|x| x.1 == c).map_or("icmp-port-unreachable", |x| x.0)));
            "REJECT"
        }
    };
    Row { pkts: r.packets, bytes: r.bytes, target: String::from(target), proto: r.proto.unwrap_or(0), inif: r.inif.clone(), outif: r.outif.clone(), src: r.src, dst: r.dst, extra }
}

impl Engine {
    /// the rows of POSTROUTING: the MASQUERADE rules with what they began
    fn masq_rows(&self, ni: usize) -> Vec<Row> {
        let Some(n) = self.nat_part(ni) else { return Vec::new() };
        n.rules
            .iter()
            .filter_map(|(id, r)| match r {
                Rule::Masquerade { iface, src } => {
                    let (pkts, bytes) = n.masq_hits.get(id).copied().unwrap_or((0, 0));
                    Some(Row { pkts, bytes, target: String::from("MASQUERADE"), proto: 0, inif: None, outif: Some(iface.clone()), src: *src, dst: None, extra: String::new() })
                }
                _ => None,
            })
            .collect()
    }

    /// -L [CHAIN]
    fn ipt_list(&self, ni: usize, c: &Cmd) -> String {
        let o = Look { numeric: c.numeric, verbose: c.verbose, exact: c.exact, lines: c.lines };
        let Some(st) = self.stack(ni) else { return String::new() };
        let mut parts: Vec<String> = Vec::new();
        for ch in chains(c.nat) {
            if c.chain.as_deref().is_some_and(|x| x != *ch) {
                continue;
            }
            parts.push(match (c.nat, *ch) {
                (false, "FORWARD") => {
                    let f = &st.fw.fwd;
                    let rows: Vec<Row> = f.rules.iter().map(fwd_row).collect();
                    chain_text(ch, if f.drop { "DROP" } else { "ACCEPT" }, (f.packets, f.bytes), &rows, o)
                }
                (true, "POSTROUTING") => chain_text(ch, "ACCEPT", (0, 0), &self.masq_rows(ni), o),
                _ => chain_text(ch, "ACCEPT", (0, 0), &[], o),
            });
        }
        parts.join("\n")
    }

    /// -S [CHAIN]
    fn ipt_rules(&self, ni: usize, c: &Cmd) -> String {
        let Some(st) = self.stack(ni) else { return String::new() };
        let only = |ch: &str| c.chain.as_deref().is_none_or(|x| x == ch);
        let mut s = String::new();
        for ch in chains(c.nat) {
            if only(ch) {
                let drop = !c.nat && *ch == "FORWARD" && st.fw.fwd.drop;
                s.push_str(&format!("-P {} {}\n", ch, if drop { "DROP" } else { "ACCEPT" }));
            }
        }
        if c.nat && only("POSTROUTING") {
            for (_, r) in st.nat.rules.iter().filter(|(_, r)| matches!(r, Rule::Masquerade { .. })) {
                s.push_str(&format!("-A POSTROUTING {}\n", masq_spec(r)));
            }
        }
        if !c.nat && only("FORWARD") {
            for r in &st.fw.fwd.rules {
                s.push_str(&format!("-A FORWARD {}\n", r.spec()));
            }
        }
        s
    }

    fn ipt_fwd(&mut self, ni: usize, op: FwdOp) -> bool {
        let dev = self.dev_of(ni);
        self.apply(Action::Fw { dev, op: FwOp::Fwd(op) }).is_ok()
    }

    fn ipt_nat(&mut self, ni: usize, act: NatAct) -> bool {
        let dev = self.dev_of(ni);
        self.apply(Action::Nat { dev, act }).is_ok()
    }

    /// the MASQUERADE rules of the device in their order
    fn masq_rules(&self, ni: usize) -> Vec<Rule> {
        self.nat_part(ni).map(|n| n.rules.iter().filter(|(_, r)| matches!(r, Rule::Masquerade { .. })).map(|(_, r)| r.clone()).collect()).unwrap_or_default()
    }

    /// the command on the shell
    pub(crate) fn sh_iptables(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let c = match parse(args) {
            Ok(c) => c,
            Err(e) => {
                self.sh_set_code(ni, 2);
                return self.sh_err(ni, &e);
            }
        };
        if let Some(ch) = c.chain.as_deref()
            && !chains(c.nat).contains(&ch)
        {
            return self.sh_err(ni, "iptables: No chain/target/match by that name.\n");
        }
        let all_or = |name: &str| c.chain.as_deref().is_none_or(|x| x == name);
        match c.op {
            Op::None | Op::Help => {
                self.out(ni, HELP);
                Ok(())
            }
            Op::List => {
                let s = self.ipt_list(ni, &c);
                self.out(ni, &s);
                Ok(())
            }
            Op::Rules => {
                let s = self.ipt_rules(ni, &c);
                self.out(ni, &s);
                Ok(())
            }
            Op::Flush => {
                if c.nat && all_or("POSTROUTING") {
                    for r in self.masq_rules(ni) {
                        self.ipt_nat(ni, NatAct::Rule { rule: r, add: false });
                    }
                } else if !c.nat && all_or("FORWARD") {
                    self.ipt_fwd(ni, FwdOp::Flush);
                }
                Ok(())
            }
            Op::Zero => {
                if c.nat && all_or("POSTROUTING") {
                    self.ipt_nat(ni, NatAct::ZeroMasq);
                } else if !c.nat && all_or("FORWARD") {
                    self.ipt_fwd(ni, FwdOp::Zero);
                }
                Ok(())
            }
            Op::Policy => {
                let (ch, p) = (c.chain.clone().unwrap_or_default(), c.policy.clone().unwrap_or_default());
                if p != "ACCEPT" && p != "DROP" {
                    return self.sh_err(ni, "iptables: Bad policy name: ACCEPT or DROP\n");
                }
                if c.nat || ch != "FORWARD" {
                    // these chains pass everything here; ACCEPT says what they do already
                    if p == "ACCEPT" {
                        return Ok(());
                    }
                    return self.sh_err(ni, &format!("iptables: the chain {} keeps the policy ACCEPT here{}\n", ch, if c.nat { "" } else { ": the device's own traffic is ufw's" }));
                }
                self.ipt_fwd(ni, FwdOp::Policy(p == "DROP"));
                Ok(())
            }
            Op::Append | Op::Insert | Op::Delete | Op::Check => {
                // -D CHAIN N: by its number
                if c.op == Op::Delete
                    && let Some(n) = c.num
                {
                    if c.ruled {
                        self.sh_set_code(ni, 2);
                        return self.sh_err(ni, &format!("iptables: -D takes a rule or a number, not both\n{}", TRY));
                    }
                    let ok = if c.nat {
                        match self.masq_rules(ni).get(n - 1).cloned() {
                            Some(r) => self.ipt_nat(ni, NatAct::Rule { rule: r, add: false }),
                            None => false,
                        }
                    } else if matches!(c.chain.as_deref(), Some("FORWARD")) {
                        self.ipt_fwd(ni, FwdOp::DeleteAt(n - 1))
                    } else {
                        false
                    };
                    return if ok { Ok(()) } else { self.sh_err(ni, "iptables: Index of deletion too big.\n") };
                }
                let built = match build(&c) {
                    Ok(b) => b,
                    Err(e) => return self.sh_err(ni, &e),
                };
                const NO_RULE: &str = "iptables: Bad rule (does a matching rule exist in that chain?).\n";
                match built {
                    Built::Masq(rule) => {
                        let have = self.masq_rules(ni).contains(&rule);
                        match c.op {
                            Op::Append | Op::Insert => {
                                self.ipt_nat(ni, NatAct::Rule { rule, add: true });
                                Ok(())
                            }
                            _ if !have => self.sh_err(ni, NO_RULE),
                            Op::Delete => {
                                self.ipt_nat(ni, NatAct::Rule { rule, add: false });
                                Ok(())
                            }
                            _ => Ok(()),
                        }
                    }
                    Built::Fwd(rule) => {
                        let have = self.stack(ni).is_some_and(|st| st.fw.fwd.rules.iter().any(|r| r.same(&rule)));
                        let n = self.stack(ni).map_or(0, |st| st.fw.fwd.rules.len());
                        match c.op {
                            Op::Append => {
                                self.ipt_fwd(ni, FwdOp::Insert { rule, at: None });
                                Ok(())
                            }
                            Op::Insert => {
                                let at = c.num.unwrap_or(1) - 1;
                                if at > n {
                                    return self.sh_err(ni, "iptables: Index of insertion too big.\n");
                                }
                                self.ipt_fwd(ni, FwdOp::Insert { rule, at: Some(at) });
                                Ok(())
                            }
                            _ if !have => self.sh_err(ni, NO_RULE),
                            Op::Delete => {
                                self.ipt_fwd(ni, FwdOp::Delete(rule));
                                Ok(())
                            }
                            _ => Ok(()),
                        }
                    }
                }
            }
        }
    }

    /// iptables-save [-t TABLE] [-c]
    pub(crate) fn sh_iptables_save(&mut self, ni: usize, args: &[&str]) -> Result<(), String> {
        let mut only: Option<bool> = None;
        let mut counters = false;
        let mut k = 0;
        while k < args.len() {
            match args[k] {
                "-c" | "--counters" => counters = true,
                "-t" | "--table" => {
                    k += 1;
                    match args.get(k).copied() {
                        Some("filter") => only = Some(false),
                        Some("nat") => only = Some(true),
                        Some(t) => return self.sh_err(ni, &format!("iptables-save: the table {} is not modelled here (filter and nat are)\n", t)),
                        None => return self.sh_err(ni, "iptables-save: option \"-t\" requires an argument\n"),
                    }
                }
                o => return self.sh_err(ni, &format!("iptables-save: unknown option \"{}\"\n", o)),
            }
            k += 1;
        }
        let Some(st) = self.stack(ni) else { return Ok(()) };
        let c = |p: u64, b: u64| if counters { format!("[{}:{}] ", p, b) } else { String::new() };
        let mut s = String::new();
        if only != Some(true) {
            let f = &st.fw.fwd;
            s.push_str(&format!("*filter\n:INPUT ACCEPT [0:0]\n:FORWARD {} [{}:{}]\n:OUTPUT ACCEPT [0:0]\n", if f.drop { "DROP" } else { "ACCEPT" }, f.packets, f.bytes));
            for r in &f.rules {
                s.push_str(&format!("{}-A FORWARD {}\n", c(r.packets, r.bytes), r.spec()));
            }
            s.push_str("COMMIT\n");
        }
        let masq = self.masq_rows(ni);
        if only == Some(true) || (only.is_none() && !masq.is_empty()) {
            s.push_str("*nat\n:PREROUTING ACCEPT [0:0]\n:INPUT ACCEPT [0:0]\n:OUTPUT ACCEPT [0:0]\n:POSTROUTING ACCEPT [0:0]\n");
            for (r, row) in self.masq_rules(ni).iter().zip(masq.iter()) {
                s.push_str(&format!("{}-A POSTROUTING {}\n", c(row.pkts, row.bytes), masq_spec(r)));
            }
            s.push_str("COMMIT\n");
        }
        self.out(ni, &s);
        Ok(())
    }
}
