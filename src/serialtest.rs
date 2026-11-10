// serialtest -- checks of the serial lines and the console cable (group "serial" of --selftest:
// jfn_sim::serial and ttyusb, their parts of link.rs, view.rs, pcap.rs, cli/ios.rs, cli/ses.rs and
// telnetd.rs): "clock rate" on the DCE end slows the line (the serialisation of a frame, the
// rounding to a usual rate, the link's rate as the limit), "no clock rate" takes the line
// protocol down at both ends (show interfaces, show ip interface brief, %LINEPROTO, LEDs, notes,
// the problem, show controllers) and "clock rate" brings it back, the DTE takes "clock rate"
// without effect, a plain serial cable ignores it, PPP at both ends carries pings with its header
// (the packet view, pcapng), PPP at one end only keeps the protocol down, the help, other
// interfaces and switches refuse, the running configuration and a new engine from it; screen,
// picocom and minicom on a PC with a console cable to a switch: show version and enable there,
// show users and show line, console messages, Ctrl+C, the session that waits for the next
// program, exit and Enter, clear line, the login of line con 0 with a hidden password, the errors
// without a cable, the device off at the start and meanwhile, the cable pulled, a second program
// on the line, the manual, the same run twice
//
// The WAN: R1 (JF-R4, Serial0/2/0 10.0.12.1/30) and R2 (JF-R4, Serial0/2/0 10.0.12.2/30) on one
// serial cable, R1 the DCE (serial-dce) unless a test says otherwise.  2048 kbit/s and 2 ms of
// line: a 100-byte datagram in 104 bytes of HDLC or PPP takes 406250 ns, so a ping's round trip is
// 2 x (406250 + 2000000) = 4812500 ns; at a clock rate of 64000 the frame takes 13 ms.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jfn_model::cable::CableKind;
use jfn_model::project::{PortRef, Project};
use jfn_sim::{Action, Encap, Engine, JobKind, Led, LineClock, Note, PingEv, PingOpts, ProblemKind, Report, SerialNote, SesKind, SEC};
use netpkt::decode;
use netpkt::Ipv4;

use crate::selftest::T;

const BUDGET: usize = 4_000_000;
const R1: u32 = 1;
const R2: u32 = 2;
const SE: &str = "Serial0/2/0";

fn s(x: &str) -> String {
    String::from(x)
}

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: s(port) }
}

fn engine(p: &Project) -> Engine {
    Engine::from_project(p).unwrap_or_else(|_| Engine::new(0))
}

fn run_for(e: &mut Engine, d: u64) {
    let t = e.now() + d;
    e.run_until(t, BUDGET);
}

/// what each device's console printed since the last call
fn outs(e: &mut Engine) -> BTreeMap<u32, String> {
    let mut m: BTreeMap<u32, String> = BTreeMap::new();
    for r in e.take_reports() {
        if let Report::CliOut { dev, bytes, .. } = r {
            m.entry(dev).or_default().push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    m
}

/// lines typed at a device's console, each followed by `d` of virtual time: what every console
/// printed meanwhile
fn all(e: &mut Engine, dev: u32, lines: &[&str], d: u64) -> BTreeMap<u32, String> {
    let _ = e.take_reports();
    let mut m: BTreeMap<u32, String> = BTreeMap::new();
    for l in lines {
        let _ = e.cli_line(dev, l);
        run_for(e, d);
        for (k, v) in outs(e) {
            m.entry(k).or_default().push_str(&v);
        }
    }
    if lines.is_empty() {
        run_for(e, d);
        m = outs(e);
    }
    m
}

/// lines typed at a device's console: what it printed
fn cli(e: &mut Engine, dev: u32, lines: &[&str], d: u64) -> String {
    all(e, dev, lines, d).remove(&dev).unwrap_or_default()
}

/// lines typed in session `ses` of a device: what it printed (Report::SesOut)
fn ses(e: &mut Engine, dev: u32, sid: u32, lines: &[&str], d: u64) -> String {
    let _ = e.take_reports();
    for l in lines {
        let _ = e.cli_ses_line(dev, sid, l);
        run_for(e, d);
    }
    let mut o = String::new();
    for r in e.take_reports() {
        if let Report::SesOut { dev: x, ses: y, bytes, .. } = r
            && x == dev
            && y == sid
        {
            o.push_str(&String::from_utf8_lossy(&bytes));
        }
    }
    o
}

/// "configure terminal", the lines in interface mode of Serial0/2/0, "end" (from the privileged
/// level): what every console printed
fn serial_conf(e: &mut Engine, dev: u32, lines: &[&str]) -> BTreeMap<u32, String> {
    let mut l = alloc::vec!["configure terminal", "interface Serial0/2/0"];
    l.extend_from_slice(lines);
    l.push("end");
    all(e, dev, &l, SEC)
}

fn r_cfg(name: &str, addr: &str, extra: &str) -> String {
    format!("hostname {}\n!\ninterface Serial0/2/0\n ip address {} 255.255.255.252\n{}!\nend\n", name, addr, extra)
}

/// R1 and R2 on a serial cable of this kind -> (project, link id)
fn wan(kind: Option<CableKind>, r1: &str, r2: &str) -> (Project, u32) {
    let mut p = Project::new("wan", 21);
    let _ = p.add_device("jf-r4", 0, 0);
    let _ = p.add_device("jf-r4", 200, 0);
    let l = p.connect(&pr(R1, "se0/2/0"), &pr(R2, "Serial0/2/0"), kind).unwrap_or(0);
    for (id, name, c) in [(R1, "R1", r_cfg("R1", "10.0.12.1", r1)), (R2, "R2", r_cfg("R2", "10.0.12.2", r2))] {
        if let Some(d) = p.device_mut(id) {
            d.name = s(name);
            d.startup_config = c;
        }
    }
    (p, l)
}

/// the engine of the WAN, both consoles at the privileged level
fn wan_engine(kind: Option<CableKind>, r1: &str, r2: &str) -> (Engine, Project, u32) {
    let (p, l) = wan(kind, r1, r2);
    let mut e = engine(&p);
    run_for(&mut e, SEC);
    let _ = cli(&mut e, R1, &["enable"], 0);
    let _ = cli(&mut e, R2, &["enable"], 0);
    (e, p, l)
}

/// one ping of 100 bytes from `dev` to `dst`: what it saw
fn ping(e: &mut Engine, dev: u32, dst: &str) -> Vec<PingEv> {
    let id = e.ping(dev, Ipv4::parse(dst).unwrap_or(Ipv4::UNSPECIFIED), PingOpts { count: 1, size: 72, ..PingOpts::default() }).unwrap_or(0);
    e.run_idle(BUDGET);
    match e.job(id).map(|j| &j.kind) {
        Some(JobKind::Ping(p)) => p.events.clone(),
        _ => Vec::new(),
    }
}

/// the round trip of one ping from R1 to R2 (None: no answer)
fn rtt(e: &mut Engine) -> Option<u64> {
    match ping(e, R1, "10.0.12.2").as_slice() {
        [PingEv::Reply { rtt, .. }] => Some(*rtt),
        _ => None,
    }
}

fn led(e: &Engine, l: u32) -> Option<[Led; 2]> {
    e.view().links.iter().find(|x| x.id == l).map(|x| x.led)
}

pub fn run(t: &mut T) {
    clock_rate(t);
    no_clock(t);
    dte_and_plain(t);
    ppp(t);
    ppp_one_end(t);
    refused(t);
    round_trip(t);
    console(t);
    console_login(t);
    console_errors(t);
    console_off_unplugged(t);
}

/// "clock rate" on the DCE end: the line's bits at that rate, rounded, at most the link's
fn clock_rate(t: &mut T) {
    let (mut e, _, l) = wan_engine(Some(CableKind::SerialDce), "", "");
    t.check("the line is up", e.link_idx_up(l));
    t.eq("the link's rate by default", rtt(&mut e), Some(4_812_500));
    let sc = cli(&mut e, R1, &["show controllers serial 0/2/0"], 0);
    t.check("the DCE gives the link's rate", sc.contains("Interface Serial0/2/0\n") && sc.contains("DCE V.35, clock rate 2048000\n"));
    t.check("the DTE has its clocks", cli(&mut e, R2, &["show controllers Serial0/2/0"], 0).contains("DTE V.35 TX and RX clocks detected.\n"));
    let _ = serial_conf(&mut e, R1, &["clock rate 64000"]);
    // 104 bytes at 64000 bit/s: 13 ms each way, plus 2 ms of line
    t.eq("64000 bit/s", rtt(&mut e), Some(30_000_000));
    t.check("frames take 13 ms", e.trace.hops().last().is_some_and(|h| h.t_rx - h.t_tx == 15_000_000));
    t.check("show controllers says it", cli(&mut e, R1, &["show controllers"], 0).contains("DCE V.35, clock rate 64000\n"));
    t.check("the link keeps its bandwidth", cli(&mut e, R1, &["show interfaces Serial0/2/0"], 0).contains("BW 2048 Kbit/sec"));
    // 70000 is no usual rate: the nearest is 72000
    let _ = serial_conf(&mut e, R1, &["clock rate 70000"]);
    t.check("rounded to 72000", e.running_config(R1).contains(" clock rate 72000\n") && e.serial_of(R1, SE).is_some_and(|c| c.clock == LineClock::Rate(72000)));
    t.eq("72000 bit/s", rtt(&mut e), Some(2 * (104 * 8 * 1_000_000_000 / 72_000 + 2_000_000)));
    // more than the link carries: the link's rate
    let _ = serial_conf(&mut e, R1, &["clock rate 4000000"]);
    t.eq("at most the link's rate", rtt(&mut e), Some(4_812_500));
    t.check("show controllers says the limit", cli(&mut e, R1, &["show controllers serial 0/2/0"], 0).contains("DCE V.35, clock rate 4000000\nthe line runs at 2048000 bit/s, the most its link carries\n"));
    let h = cli(&mut e, R1, &["configure terminal", "interface Serial0/2/0", "clock rate ?"], 0);
    t.check("the help of the rate", h.contains("<1200-8000000>") && h.contains("64000 72000 115200") && h.contains("2000000 4000000 5300000"));
    let h = cli(&mut e, R1, &["clock ?"], 0);
    t.check("the help of clock", h.contains("rate") && h.contains("How fast the DCE end clocks the line"));
    let _ = cli(&mut e, R1, &["end"], 0);
}

/// "no clock rate" on the DCE end: up, line protocol down at both ends, and back
fn no_clock(t: &mut T) {
    let (mut e, _, l) = wan_engine(Some(CableKind::SerialDce), "", "");
    let pi = e.port_index(R1, SE).unwrap_or(99);
    let o = serial_conf(&mut e, R1, &["no clock rate"]);
    let (o1, o2) = (o.get(&R1).cloned().unwrap_or_default(), o.get(&R2).cloned().unwrap_or_default());
    t.check("%LINEPROTO at the DCE", o1.contains("%LINEPROTO-5-UPDOWN: line protocol on Serial0/2/0 is down") && !o1.contains("%LINK-3-UPDOWN: Serial0/2/0 is down"));
    t.check("%LINEPROTO at the DTE", o2.contains("%LINEPROTO-5-UPDOWN: line protocol on Serial0/2/0 is down") && !o2.contains("%LINK-3-UPDOWN"));
    t.check("the link carries nothing", !e.link_idx_up(l));
    t.eq("both LEDs red", led(&e, l), Some([Led::Down, Led::Down]));
    let s1 = cli(&mut e, R1, &["show interfaces Serial0/2/0"], 0);
    t.check("the DCE: up, protocol down, why", s1.starts_with("Serial0/2/0 is up, line protocol is down\n") && s1.contains("  Serial cable end: DCE\n") && s1.contains("  Line protocol down: this DCE end gives no clock (no clock rate)\n"));
    let s2 = cli(&mut e, R2, &["show interfaces Serial0/2/0"], 0);
    t.check("the DTE: up, protocol down, why", s2.starts_with("Serial0/2/0 is up, line protocol is down\n") && s2.contains("  Line protocol down: no clock from the DCE end\n"));
    t.check("show ip interface brief", cli(&mut e, R1, &["show ip interface brief"], 0).contains("Serial0/2/0            10.0.12.1       YES NVRAM  up                    down\n"));
    t.check("show controllers at the DCE", cli(&mut e, R1, &["show controllers serial 0/2/0"], 0).contains("DCE V.35, no clock\n"));
    t.check("show controllers at the DTE", cli(&mut e, R2, &["show controllers serial 0/2/0"], 0).contains("DTE V.35 clocks stopped: the DCE end gives no clock\n"));
    t.eq("no ping", rtt(&mut e), None);
    t.check("no connected route", !cli(&mut e, R1, &["show ip route"], 0).contains("10.0.12.0"));
    let notes: Vec<Note> = e.trace.events().filter(|x| matches!(x.note, Note::Serial(_))).map(|x| x.note.clone()).collect();
    t.check("the notes at both ends", notes.contains(&Note::Serial(SerialNote::NoClock { port: pi, dce_here: true })) && notes.contains(&Note::Serial(SerialNote::NoClock { port: pi, dce_here: false })));
    t.eq("the note's sentence", Note::Serial(SerialNote::NoClock { port: 0, dce_here: true }).message(&|_| s("Se0/2/0"), &|_| s("")).text(), s("Serielle Leitung an Se0/2/0: dieses DCE-Ende gibt keinen Takt (no clock rate); das Leitungsprotokoll bleibt aus."));
    t.eq("a note of layer 1", Note::Serial(SerialNote::NoClock { port: 0, dce_here: false }).layer(), 1);
    let probs = e.problems();
    t.eq(
        "the problem",
        probs.iter().filter(|x| x.kind == ProblemKind::SerialNoClock).map(|x| (x.link, x.text.clone())).collect::<Vec<_>>(),
        alloc::vec![(Some(l), s("Serielle Leitung zwischen R1 Se0/2/0 und R2 Se0/2/0: das DCE-Ende R1 Se0/2/0 gibt keinen Takt (no clock rate); das Leitungsprotokoll bleibt aus."))]
    );
    t.check("the configuration says it", e.running_config(R1).contains(" no clock rate\n"));
    // a clock again: up at both ends
    let o = serial_conf(&mut e, R1, &["clock rate 64000"]);
    t.check("%LINEPROTO up at both", [R1, R2].iter().all(|d| o.get(d).is_some_and(|x| x.contains("%LINEPROTO-5-UPDOWN: line protocol on Serial0/2/0 is up"))));
    t.eq("up again", (e.link_idx_up(l), led(&e, l)), (true, Some([Led::Up, Led::Up])));
    t.eq("pings at 64000", rtt(&mut e), Some(30_000_000));
    t.check("no problem now", e.problems().iter().all(|x| x.kind != ProblemKind::SerialNoClock));
    // the DCE as the second end (serial-dte names the first end DTE)
    let (mut f, _, l2) = wan_engine(Some(CableKind::SerialDte), "", "");
    let _ = serial_conf(&mut f, R1, &["no clock rate"]);
    t.check("no clock rate at the DTE does nothing", f.link_idx_up(l2));
    let _ = serial_conf(&mut f, R2, &["no clock rate"]);
    t.check("at the DCE (the second end) it does", !f.link_idx_up(l2) && cli(&mut f, R2, &["show interfaces Serial0/2/0"], 0).contains("this DCE end gives no clock"));
}

/// the DTE takes "clock rate" and does nothing with it; a plain serial cable has no DCE
fn dte_and_plain(t: &mut T) {
    let (mut e, _, l) = wan_engine(Some(CableKind::SerialDce), "", "");
    let o = serial_conf(&mut e, R2, &["clock rate 9600"]);
    t.check("taken at the DTE", !o.get(&R2).is_some_and(|x| x.contains("% Invalid")) && e.serial_of(R2, SE).is_some_and(|c| c.clock == LineClock::Rate(9600)));
    t.eq("no effect", rtt(&mut e), Some(4_812_500));
    t.check("show controllers says so", cli(&mut e, R2, &["show controllers serial 0/2/0"], 0).contains("DTE V.35 TX and RX clocks detected.\nclock rate 9600 is set here; a DTE end takes its clock from the DCE end, so it does nothing\n"));
    t.check("kept in the configuration", e.running_config(R2).contains(" clock rate 9600\n"));
    t.check("still up", e.link_idx_up(l));
    // a plain serial cable (older files): nothing to clock
    let (mut f, _, lp) = wan_engine(None, "", "");
    let _ = serial_conf(&mut f, R1, &["no clock rate"]);
    let _ = serial_conf(&mut f, R2, &["clock rate 64000"]);
    t.check("a plain cable stays up", f.link_idx_up(lp));
    t.eq("at its link's rate", rtt(&mut f), Some(4_812_500));
    let sc = cli(&mut f, R2, &["show controllers serial 0/2/0"], 0);
    t.check("show controllers without an end", sc.contains("V.35 cable without a chosen end: no DCE, the line runs at its link's 2048000 bit/s\n") && sc.contains("clock rate 64000 is set here; with no DCE end it does nothing\n"));
    t.check("no cable", cli(&mut f, R1, &["show controllers serial 0/2/1"], 0).contains("Interface Serial0/2/1\nHardware is a synchronous serial port (2048 kbit/s)\nNo serial cable attached\n"));
    // the defaults write nothing (the goldens stay as they are)
    let (g, _, _) = wan_engine(Some(CableKind::SerialDce), "", "");
    let c = g.running_config(R1);
    t.check("nothing for the defaults", !c.contains("clock rate") && !c.contains("encapsulation"));
}

/// PPP at both ends: pings with its header, decoded, in the capture
fn ppp(t: &mut T) {
    let (mut e, _, l) = wan_engine(Some(CableKind::SerialDce), "", "");
    let _ = serial_conf(&mut e, R1, &["encapsulation ppp"]);
    t.check("PPP at one end: down", !e.link_idx_up(l));
    let _ = serial_conf(&mut e, R2, &["encapsulation ppp"]);
    t.check("PPP at both: up", e.link_idx_up(l));
    let first = e.trace.next_hop_id();
    t.eq("a ping over PPP", rtt(&mut e), Some(4_812_500));
    let hops: Vec<_> = e.trace.hops().filter(|h| h.id >= first).cloned().collect();
    t.check("PPP framing", hops.len() == 2 && hops.iter().all(|h| h.hdlc && h.bytes.len() == 104 && h.bytes[..4] == [0xFF, 0x03, 0x00, 0x21]));
    t.check("decoded as ICMP", hops.iter().all(|h| h.mask & decode::P_ICMP != 0));
    if let Some(h) = hops.first() {
        let ls = jfn_sim::view::hop_layers(h);
        t.check("the packet view: PPP first", ls.first().is_some_and(|x| x.title == "PPP" && x.len == 4 && x.fields.iter().any(|f| f.name == "Protocol" && f.value.contains("0x0021"))));
        t.check("then IPv4 at byte 4", ls.get(1).is_some_and(|x| x.off == 4));
        t.eq("the summary", jfn_sim::view::hop_info(h).0, decode::Proto::Icmp);
        t.eq("its link type", jfn_sim::pcap::linktype(h), 50);
    }
    let cap = e.pcapng(&jfn_sim::PcapOpts::default()).bytes;
    t.check("pcapng: LINKTYPE_PPP_HDLC", (0..cap.len().saturating_sub(10)).any(|i| cap[i..i + 4] == [1, 0, 0, 0] && cap[i + 8..i + 10] == [50, 0]));
    let si = cli(&mut e, R1, &["show interfaces Serial0/2/0"], 0);
    t.check("show interfaces: LCP Open", si.contains("Serial0/2/0 is up, line protocol is up\n") && si.contains("  Encapsulation PPP, LCP Open\n  Open: IPCP, crc 16, loopback not set\n"));
    t.check("the running configuration", e.running_config(R1).contains(" ip address 10.0.12.1 255.255.255.252\n encapsulation ppp\n"));
    t.eq("IPv6 in PPP", jfn_sim::serial::frame_hdr(Encap::Ppp, netpkt::eth::ET_IPV6), [0xFF, 0x03, 0x00, 0x57]);
    t.eq("IPv4 in HDLC as before", jfn_sim::serial::frame_hdr(Encap::Hdlc, netpkt::eth::ET_IPV4), [0x0F, 0x00, 0x08, 0x00]);
    // back to HDLC at both ends ("no encapsulation" is HDLC)
    let _ = serial_conf(&mut e, R1, &["no encapsulation"]);
    let _ = serial_conf(&mut e, R2, &["encapsulation hdlc"]);
    let first = e.trace.next_hop_id();
    t.eq("HDLC again", rtt(&mut e), Some(4_812_500));
    t.check("HDLC framing", e.trace.hops().filter(|h| h.id >= first).all(|h| h.bytes[..4] == [0x0F, 0x00, 0x08, 0x00]));
}

/// PPP at one end only: the line protocol down at both, both say why
fn ppp_one_end(t: &mut T) {
    let (mut e, _, l) = wan_engine(Some(CableKind::SerialDce), "", "");
    let pi = e.port_index(R1, SE).unwrap_or(99);
    let o = serial_conf(&mut e, R1, &["encapsulation ppp"]);
    t.check("%LINEPROTO at both", [R1, R2].iter().all(|d| o.get(d).is_some_and(|x| x.contains("%LINEPROTO-5-UPDOWN: line protocol on Serial0/2/0 is down"))));
    let s1 = cli(&mut e, R1, &["show interfaces Serial0/2/0"], 0);
    t.check("PPP: LCP Closed, why", s1.starts_with("Serial0/2/0 is up, line protocol is down\n") && s1.contains("  Encapsulation PPP, LCP Closed, crc 16, loopback not set\n") && s1.contains("  Line protocol down: the far end runs HDLC, this end PPP\n"));
    let s2 = cli(&mut e, R2, &["show interfaces Serial0/2/0"], 0);
    t.check("HDLC: why", s2.contains("  Encapsulation HDLC, crc 16, loopback not set\n") && s2.contains("  Line protocol down: the far end runs PPP, this end HDLC\n"));
    t.eq("no ping", rtt(&mut e), None);
    t.eq("both LEDs red", led(&e, l), Some([Led::Down, Led::Down]));
    t.check("the note", e.trace.events().any(|x| x.dev == R1 && x.note == Note::Serial(SerialNote::Encap { port: pi, own: Encap::Ppp, other: Encap::Hdlc })));
    t.eq("a note of layer 2", Note::Serial(SerialNote::Encap { port: 0, own: Encap::Ppp, other: Encap::Hdlc }).layer(), 2);
    t.eq(
        "the problem",
        e.problems().iter().filter(|x| x.kind == ProblemKind::SerialEncap).map(|x| x.text.clone()).collect::<Vec<_>>(),
        alloc::vec![s("Serielle Leitung zwischen R1 Se0/2/0 und R2 Se0/2/0: PPP an R1 Se0/2/0, HDLC an R2 Se0/2/0; das Leitungsprotokoll bleibt aus.")]
    );
    let _ = serial_conf(&mut e, R1, &["encapsulation hdlc"]);
    t.check("the same again: up", e.link_idx_up(l) && e.problems().iter().all(|x| x.kind != ProblemKind::SerialEncap));
}

/// other interfaces and devices refuse; the help
fn refused(t: &mut T) {
    let (mut e, _, _) = wan_engine(Some(CableKind::SerialDce), "", "");
    let o = cli(&mut e, R1, &["configure terminal", "interface GigabitEthernet0/0/0", "encapsulation ppp", "clock rate 64000"], 0);
    t.eq("an Ethernet port refuses both", o.matches("% Invalid input detected at '^' marker.").count(), 2);
    t.check("nothing changed there", e.serial_of(R1, "GigabitEthernet0/0/0").is_some_and(|c| c.encap == Encap::Hdlc && c.clock == LineClock::Auto));
    let h = cli(&mut e, R1, &["interface Serial0/2/0", "encapsulation ?"], 0);
    t.check("the help of encapsulation", h.contains("hdlc") && h.contains("HDLC frames (the default)") && h.contains("ppp") && h.contains("PPP frames (RFC 1662 framing)"));
    let _ = cli(&mut e, R1, &["end"], 0);
    t.check("show controllers in the help", cli(&mut e, R1, &["show ?"], 0).contains("controllers"));
    t.eq("show controllers of an Ethernet port", cli(&mut e, R1, &["show controllers gi0/0/0"], 0), s("% GigabitEthernet0/0/0 is no serial port: show controllers shows the serial ports\n"));
    // a switch has no serial ports: no such commands
    let mut p = Project::new("sw", 2);
    let sw = p.add_device("jf-s24p", 0, 0).unwrap_or(0);
    let mut f = engine(&p);
    run_for(&mut f, SEC);
    let o = cli(&mut f, sw, &["enable", "configure terminal", "interface GigabitEthernet1/0/1", "clock rate 64000", "end", "show controllers"], 0);
    t.eq("a switch knows none", o.matches("% Invalid input detected at '^' marker.").count(), 2);
}

/// the running configuration through a new engine: the same settings, the same line
fn round_trip(t: &mut T) {
    for (what, r1, r2) in [("PPP and a clock rate", " encapsulation ppp\n clock rate 64000\n", " encapsulation ppp\n clock rate 9600\n"), ("no clock rate", " no clock rate\n", ""), ("PPP at one end", " encapsulation ppp\n", "")] {
        let (mut e, mut p, l) = wan_engine(Some(CableKind::SerialDce), r1, r2);
        let (c1, c2) = (e.running_config(R1), e.running_config(R2));
        t.check(&format!("{}: the configuration has it", what), c1.contains(r1.lines().next().unwrap_or("")) && c2.contains(r2.lines().next().unwrap_or("")));
        for (id, c) in [(R1, c1.clone()), (R2, c2.clone())] {
            if let Some(d) = p.device_mut(id) {
                d.startup_config = c;
            }
        }
        let mut f = engine(&p);
        run_for(&mut f, SEC);
        t.eq(&format!("{}: the same settings", what), [f.serial_of(R1, SE), f.serial_of(R2, SE)], [e.serial_of(R1, SE), e.serial_of(R2, SE)]);
        t.eq(&format!("{}: the same line", what), (f.link_idx_up(l), rtt(&mut f)), (e.link_idx_up(l), rtt(&mut e)));
        t.eq(&format!("{}: the same configuration again", what), (f.running_config(R1), f.running_config(R2)), (c1, c2));
        t.check(&format!("{}: no boot errors", what), f.boot_errors(R1).is_empty() && f.boot_errors(R2).is_empty());
    }
}

// ---------------------------------------------------------------- the console cable

const PC1: u32 = 1;
const SW1: u32 = 2;
const PC2: u32 = 3;

/// PC1 with a console cable on usb0 to SW1 (`sw`: its startup configuration), PC2 without one
/// -> (project, the console cable's id)
fn lab(sw: &str) -> (Project, u32) {
    let mut p = Project::new("console", 31);
    let _ = p.add_device("pc", 0, 0);
    let _ = p.add_device("jf-s24p", 200, 0);
    let _ = p.add_device("pc", 0, 200);
    let l = p.connect(&pr(PC1, "usb0"), &pr(SW1, "Console"), None).unwrap_or(0);
    for (id, name, c) in [(PC1, "PC1", s("hostname PC1\n")), (SW1, "SW1", format!("hostname SW1\n!\n{}end\n", sw)), (PC2, "PC2", s("hostname PC2\n"))] {
        if let Some(d) = p.device_mut(id) {
            d.name = s(name);
            d.startup_config = c;
        }
    }
    (p, l)
}

fn lab_engine(sw: &str) -> (Engine, u32) {
    let (p, l) = lab(sw);
    let mut e = engine(&p);
    run_for(&mut e, SEC);
    (e, l)
}

/// the cable sessions of a device: (id, a program has the line)
fn cables(e: &Engine, dev: u32) -> Vec<(u32, bool)> {
    e.node(dev).map(|n| n.cli.sessions.iter().filter(|i| i.kind == SesKind::Cable).map(|i| (i.id, i.tty.is_some())).collect()).unwrap_or_default()
}

/// screen on PC1: show version and enable on SW1, what SW1 shows of it, Ctrl+C, the session
/// that waits, exit and Enter, clear line, console messages -> PC1's whole output
fn console_flow(t: &mut T) -> String {
    let (mut e, l) = lab_engine("");
    t.check("the console cable is there", e.link_rt(l).is_some_and(|x| x.cable == CableKind::Console));
    let mut all_out = String::new();
    let o = cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC);
    all_out.push_str(&o);
    t.eq("screen opens SW1's console line", o, s("Connected to /dev/ttyUSB0 at 9600 baud. Ctrl+C ends screen.\nSW1>"));
    t.check("PC1's console runs it and reads lines", e.cli_busy(PC1) && e.cli_reads(PC1) && !e.cli_secret(PC1));
    t.eq("a cable session on SW1", cables(&e, SW1).len(), 1);
    let o = cli(&mut e, PC1, &["show version"], SEC);
    all_out.push_str(&o);
    t.check("show version there", o.contains("JFNetworker device software 1.0 for the JF-S24P") && o.contains("SW1 is up for") && o.ends_with("SW1>"));
    let o = cli(&mut e, PC1, &["enable"], SEC);
    all_out.push_str(&o);
    t.eq("enable there", o, s("SW1#"));
    t.eq("SW1's own console as it was", e.cli_prompt(SW1), s("SW1>"));
    let u = cli(&mut e, SW1, &["show users"], 0);
    t.check("show users: the cable session on con 0 from PC1 usb0", u.contains("*   con 0 ") && u.lines().any(|x| x.starts_with("    con 0      -          15") && x.ends_with("PC1 usb0")));
    let sl = cli(&mut e, SW1, &["show line"], 0);
    t.check("show line: con 0 from PC1 usb0", sl.contains("*   con 0      in use   -          -          PC1 usb0\n"));
    // console messages reach the cable, as a console shows them
    let m = all(&mut e, SW1, &["enable", "configure terminal", "end"], SEC);
    let o = m.get(&PC1).cloned().unwrap_or_default();
    all_out.push_str(&o);
    t.check("console messages over the cable", o.contains("%SYS-5-CONFIG_I: configuration changed at the console"));
    let o = cli(&mut e, PC1, &["show users"], SEC);
    all_out.push_str(&o);
    t.check("show users over the cable", o.lines().any(|x| x.starts_with("*   con 0      -          15") && x.ends_with("PC1 usb0")));
    // Ctrl+C ends screen; the session waits on the line
    let _ = e.take_reports();
    let _ = e.cli_interrupt(PC1);
    run_for(&mut e, SEC);
    all_out.push_str(&outs(&mut e).remove(&PC1).unwrap_or_default());
    t.check("Ctrl+C ends screen", !e.cli_busy(PC1) && e.cli_prompt(PC1) == "root@PC1:~# ");
    t.eq("the session waits without a terminal", cables(&e, SW1).iter().map(|x| x.1).collect::<Vec<_>>(), alloc::vec![false]);
    t.check("show users: no place now", cli(&mut e, SW1, &["show users"], 0).lines().any(|x| x.starts_with("    con 0      -          15") && !x.contains("PC1")));
    let o = cli(&mut e, PC1, &["picocom -b 9600 /dev/ttyUSB0"], SEC);
    all_out.push_str(&o);
    t.eq("picocom finds the session as it was", o, s("picocom on /dev/ttyUSB0, 9600 baud\nTerminal ready (Ctrl+C ends picocom)\nSW1#"));
    t.eq("still one session", cables(&e, SW1).len(), 1);
    // exit: as at the console, then Enter
    let o = cli(&mut e, PC1, &["exit"], SEC);
    all_out.push_str(&o);
    t.check("exit: press Enter", o.contains("SW1: session closed.") && o.contains("Console ready, press Enter to log in.") && e.cli_busy(PC1));
    let o = cli(&mut e, PC1, &[""], SEC);
    all_out.push_str(&o);
    t.eq("Enter: the user level", o, s("SW1>"));
    // clear line console 0 from SW1's console
    let m = all(&mut e, SW1, &["clear line console 0"], SEC);
    let o = m.get(&PC1).cloned().unwrap_or_default();
    all_out.push_str(&o);
    t.check("clear line reaches the cable", o.contains("% This line was cleared from another session.") && o.contains("Console ready, press Enter to log in."));
    let _ = e.take_reports();
    let _ = e.cli_interrupt(PC1);
    run_for(&mut e, SEC);
    let o = cli(&mut e, PC1, &["minicom -D /dev/ttyUSB0"], SEC);
    all_out.push_str(&o);
    t.check("minicom", o.starts_with("minicom on /dev/ttyUSB0, 9600 8N1. Ctrl+C ends minicom.\n") && e.cli_busy(PC1));
    let o = cli(&mut e, PC1, &["", "show version"], SEC);
    all_out.push_str(&o);
    t.check("minicom: Enter, show version", o.starts_with("SW1>") && o.contains("JFNetworker device software 1.0") && o.ends_with("SW1>"));
    all_out.push_str(&format!("|{:x}", jfn_sim::keyframe::digest(&e)));
    all_out
}

/// the flow, and the same run again: the same output and state
fn console(t: &mut T) {
    let a = console_flow(t);
    let b = console_flow(t);
    t.check("the same run twice", !a.is_empty() && a == b);
}

/// line con 0 with a password: asked over the cable, hidden
fn console_login(t: &mut T) {
    let (mut e, _) = lab_engine("line con 0\n password cisco\n login\n!\n");
    let o = cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC);
    t.check("the console's password", o.ends_with("Password: ") && e.cli_secret(PC1));
    let o = cli(&mut e, PC1, &["falsch"], SEC);
    t.check("a wrong password", o.contains("% Access denied") && o.ends_with("Password: "));
    let o = cli(&mut e, PC1, &["cisco"], SEC);
    t.eq("logged in", o, s("SW1>"));
    t.check("echo again", !e.cli_secret(PC1));
    let o = cli(&mut e, PC1, &["exit"], SEC);
    t.check("exit: the login again", o.contains("Console session closed.") && o.ends_with("Password: "));
    t.check("SW1's own console has its own login", e.cli_prompt(SW1) == "Password: ");
}

/// no cable, the wrong file, usage, the manual, a second program on the line
fn console_errors(t: &mut T) {
    let (mut e, _) = lab_engine("");
    let o = cli(&mut e, PC2, &["screen /dev/ttyUSB0"], SEC);
    t.eq("no cable", (o, e.cli_busy(PC2)), (s("Cannot open /dev/ttyUSB0: No such file or directory\n"), false));
    t.check("its status", cli(&mut e, PC2, &["echo $?"], SEC).starts_with('1'));
    t.eq("picocom without a cable", cli(&mut e, PC2, &["picocom /dev/ttyUSB0"], SEC), s("FATAL: cannot open /dev/ttyUSB0: No such file or directory\n"));
    t.eq("minicom without a cable", cli(&mut e, PC2, &["minicom -D /dev/ttyUSB0"], SEC), s("minicom: cannot open /dev/ttyUSB0: No such file or directory\n"));
    t.eq("another file", cli(&mut e, PC1, &["screen /dev/ttyS0"], SEC), s("Cannot open /dev/ttyS0: No such file or directory\n"));
    t.check("screen alone", cli(&mut e, PC1, &["screen"], SEC).starts_with("usage: screen /dev/ttyUSB0 [BAUD]"));
    t.check("a bad speed", cli(&mut e, PC1, &["picocom -b fast /dev/ttyUSB0"], SEC).starts_with("usage: picocom"));
    t.check("the manual", cli(&mut e, PC1, &["man screen"], SEC).contains("Konsolenkabel") && cli(&mut e, PC1, &["which picocom minicom"], SEC) == "/usr/bin/picocom\n/usr/bin/minicom\n");
    // a second program on the line
    let _ = cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC);
    let s2 = e.cli_open(PC1).unwrap_or(0);
    let o = ses(&mut e, PC1, s2, &["screen /dev/ttyUSB0"], SEC);
    t.check("busy", o.contains("Cannot open /dev/ttyUSB0: Device or resource busy") && !e.cli_ses_busy(PC1, s2) && e.cli_busy(PC1));
    t.eq("the first one still talks", cli(&mut e, PC1, &["enable"], SEC), s("SW1#"));
}

/// the device off at the start and meanwhile, back on; the cable pulled
fn console_off_unplugged(t: &mut T) {
    let (mut e, l) = lab_engine("");
    let _ = e.apply(Action::Power { dev: SW1, on: false });
    let o = cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC);
    t.eq("the device off: nothing answers", o, s("Connected to /dev/ttyUSB0 at 9600 baud. Ctrl+C ends screen.\n(No answer on /dev/ttyUSB0: the device at the far end is off. Enter tries again.)\n"));
    t.check("screen waits", e.cli_busy(PC1) && cables(&e, SW1).is_empty());
    t.eq("Enter while it is off", cli(&mut e, PC1, &[""], SEC), s(""));
    let _ = e.apply(Action::Power { dev: SW1, on: true });
    run_for(&mut e, SEC);
    t.eq("on again: Enter finds it", cli(&mut e, PC1, &[""], SEC), s("SW1>"));
    let _ = cli(&mut e, PC1, &["enable"], SEC);
    let _ = e.take_reports();
    let _ = e.apply(Action::Power { dev: SW1, on: false });
    run_for(&mut e, SEC);
    let o = outs(&mut e).remove(&PC1).unwrap_or_default();
    t.eq("off meanwhile", o, s("\n(No answer on /dev/ttyUSB0 any more: the device at the far end went off. Enter tries again.)\n"));
    t.check("the session went with it, screen waits", cables(&e, SW1).is_empty() && e.cli_busy(PC1));
    let _ = e.apply(Action::Power { dev: SW1, on: true });
    run_for(&mut e, SEC);
    t.eq("a new session after the restart", cli(&mut e, PC1, &[""], SEC), s("SW1>"));
    // the cable pulled: screen ends, the session stays on the line
    let _ = e.take_reports();
    let _ = e.apply(Action::Unplug { link: l, down: true });
    run_for(&mut e, SEC);
    let o = outs(&mut e).remove(&PC1).unwrap_or_default();
    t.eq("the cable pulled", (o, e.cli_busy(PC1)), (s("\n[screen is terminating]\n"), false));
    t.eq("the session stays", cables(&e, SW1).iter().map(|x| x.1).collect::<Vec<_>>(), alloc::vec![false]);
    t.eq("no cable now", cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC), s("Cannot open /dev/ttyUSB0: No such file or directory\n"));
    let _ = e.apply(Action::Unplug { link: l, down: false });
    t.eq("plugged in again: as it was", cli(&mut e, PC1, &["screen /dev/ttyUSB0"], SEC), s("Connected to /dev/ttyUSB0 at 9600 baud. Ctrl+C ends screen.\nSW1>"));
    // the cable removed while screen runs
    let _ = e.take_reports();
    let _ = e.apply(Action::Disconnect(l));
    run_for(&mut e, SEC);
    t.check("the cable removed", outs(&mut e).remove(&PC1).is_some_and(|o| o.contains("[screen is terminating]")) && !e.cli_busy(PC1));
}
