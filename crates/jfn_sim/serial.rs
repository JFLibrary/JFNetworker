// serial -- the serial WAN lines (S1): who gives the clock, the frame format, and the line
// protocol both ends decide together
//
//   LineClock / Encap / SerialCfg   what "clock rate" and "encapsulation" told a serial port
//                                   (Port::serial; the default: the clock the DCE gives, HDLC)
//   SerialFault                     why a line's protocol is down: no clock, or the ends run
//                                   different encapsulations
//   SerialNote                      the notes about it (Note::Serial: layer 1 for the clock, 2
//                                   for the frame format)
//   Engine::serial_fault(li)        that, for an engine link slot (link.rs: link_up asks it)
//   Engine::serial_bps(li)          the clock rate the DCE end sets, if any: the line's bits go
//                                   at most that fast (link.rs: start_tx)
//   Engine::serial_of(dev, port)    what a serial port was told
//   build_serial / Engine::serial_if_cmd   "clock rate N", "no clock rate", "encapsulation
//                                   hdlc|ppp", "no encapsulation" in interface mode and "show
//                                   controllers [serial N]" (cli/ios.rs dispatches them)
//   Engine::serial_show / serial_if_config / ios_controllers
//                                   "show interfaces"' encapsulation lines and why the line
//                                   protocol is down, the lines of the running configuration,
//                                   "show controllers serial"
//   frame_hdr(encap, ethertype) / is_ppp(frame) / PPP_IPV4 / PPP_IPV6
//                                   the first four bytes of a frame on the line, and how a
//                                   recorded frame tells PPP from Cisco HDLC
//   Engine::serial_problems         the problems SerialNoClock and SerialEncap (problems.rs)
//
// The clock (decision): a chosen end (cable kind serial-dce / serial-dte) makes its port the
// DCE.  As newer IOS does on a DCE cable it detects, that end gives a clock by default and the
// line runs at its link's rate, so every older project runs as before.  "clock rate N" on the
// DCE sets the line's bit rate: frames serialise at the lower of N and the link's rate
// (link_kbps, BW in "show interfaces" and OSPF's costs keep the link's rate, as IOS's
// "bandwidth" does).  Values are rounded to the nearest of the usual rates (STD_RATES), as the
// original rounds to what its interface supports.  "no clock rate" on the DCE takes the clock
// away: both ends keep their carrier and the line protocol goes down at both ("is up, line
// protocol is down", %LINEPROTO, red LEDs: link_up is false, so nothing crosses and the
// connected routes go).  On the DTE "clock rate" is taken, kept in the configuration and does
// nothing ("show controllers serial" says so).  A plain "serial" cable names no end: there is
// nothing to clock and the line runs at its link's rate whatever either end says.
//
// The frame format: "encapsulation hdlc" (the default: Cisco HDLC, address 0x0F, control 0,
// then the ethertype) or "encapsulation ppp" (the HDLC-like framing of RFC 1662: address 0xFF,
// control 0x03, then the PPP protocol, 0x0021 for IPv4 and 0x0057 for IPv6).  Both headers are
// four bytes, so the packet sits where it always sat.  Ends that differ keep the line protocol
// down at both.  Neither format sends anything of its own: no HDLC keepalives, no LCP, no IPCP
// on the wire -- the line protocol follows from both ends' configuration at once, as soon as one
// changes, and "LCP Open" means that both ends run PPP and the line is up.  A receiver takes
// either header (a frame of the other format cannot arrive: the line is down then).  The packet
// view decodes PPP as it decodes Cisco HDLC (view.rs), pcapng writes LINKTYPE_PPP_HDLC (50) for
// it (pcap.rs, runner.rs), SNMP's ifType is ppp(23) instead of propPointToPointSerial(22).
//
// The running configuration writes " encapsulation ppp" after the addresses, " clock rate N" or
// " no clock rate" after " shutdown", nothing for the defaults (so the goldens stay as they are);
// the startup configuration replays them through the command line.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use base::cmdtree::{ModeId, Parsed, Tree, F_NO};
use jfn_model::catalog::Media;
use jfn_model::msg;
use jfn_model::msg::Msg;

use crate::cli::ios::K;
use crate::kernel::Engine;
use crate::link::LinkRt;
use crate::problems::{Problem, ProblemKind, Severity};
use crate::trace::Note;

/// the first bytes of a PPP frame carrying IPv4 (address, control, protocol)
pub const PPP_IPV4: [u8; 4] = [0xFF, 0x03, 0x00, 0x21];
/// and IPv6
pub const PPP_IPV6: [u8; 4] = [0xFF, 0x03, 0x00, 0x57];

/// the usual clock rates in bit/s ("clock rate" rounds to the nearest)
pub const STD_RATES: [u32; 32] = [
    1200, 2400, 4800, 9600, 14400, 19200, 28800, 32000, 38400, 48000, 56000, 57600, 64000, 72000, 115200, 125000, 128000, 148000, 192000, 250000, 256000, 384000, 500000, 512000, 768000, 800000, 1000000, 1300000, 2000000, 4000000, 5300000, 8000000,
];

/// the help of the rate: the usual values in our words
const RATE_HELP: &str = "Bit/s, rounded to the nearest usual rate:\n      1200 2400 4800 9600 14400 19200 28800 32000 38400 48000 56000 57600\n      64000 72000 115200 125000 128000 148000 192000 250000 256000 384000\n      500000 512000 768000 800000 1000000 1300000 2000000 4000000 5300000\n      8000000";

/// what "clock rate" told a serial port
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LineClock {
    /// nothing: a DCE end clocks the line at its link's rate
    #[default]
    Auto,
    /// "clock rate N" (bit/s)
    Rate(u32),
    /// "no clock rate": a DCE end gives no clock
    Off,
}

/// the frame format of a serial port
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Encap {
    /// Cisco HDLC (the default)
    #[default]
    Hdlc,
    /// PPP in HDLC-like framing (RFC 1662)
    Ppp,
}

impl Encap {
    /// "HDLC", "PPP"
    pub fn name(self) -> &'static str {
        match self {
            Encap::Hdlc => "HDLC",
            Encap::Ppp => "PPP",
        }
    }
}

/// what a serial port was told
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SerialCfg {
    pub clock: LineClock,
    pub encap: Encap,
}

/// why the line protocol of a serial line is down
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SerialFault {
    /// its DCE end (0: Link::a, 1: Link::b) gives no clock
    NoClock { dce: u8 },
    /// its ends run these encapsulations (end 0's, end 1's)
    Encap([Encap; 2]),
}

/// a decision about a serial line (Note::Serial; ports are the noting device's)
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SerialNote {
    /// the line on a port has no clock: this DCE end gives none, or the DCE end opposite
    NoClock { port: u16, dce_here: bool },
    /// this end runs `own`, the far end `other`
    Encap { port: u16, own: Encap, other: Encap },
}

impl SerialNote {
    /// the sentence as a message (`port` names a port index)
    pub fn message(&self, port: &dyn Fn(u16) -> String) -> Msg {
        match self {
            SerialNote::NoClock { port: p, dce_here: true } => msg!("Serielle Leitung an {}: dieses DCE-Ende gibt keinen Takt (no clock rate); das Leitungsprotokoll bleibt aus.", port(*p)),
            SerialNote::NoClock { port: p, .. } => msg!("Serielle Leitung an {}: kein Takt vom DCE-Ende gegenüber; das Leitungsprotokoll bleibt aus.", port(*p)),
            SerialNote::Encap { port: p, own, other } => msg!("Serielle Leitung an {}: hier {}, gegenüber {}; das Leitungsprotokoll bleibt aus.", port(*p), own.name(), other.name()),
        }
    }
    /// 1: the clock, 2: the frame format
    pub fn layer(&self) -> u8 {
        match self {
            SerialNote::NoClock { .. } => 1,
            SerialNote::Encap { .. } => 2,
        }
    }
}

/// the four bytes before a packet of `ethertype` on a serial line of this format
pub fn frame_hdr(encap: Encap, ethertype: u16) -> [u8; 4] {
    match encap {
        Encap::Hdlc => {
            let e = ethertype.to_be_bytes();
            [0x0F, 0x00, e[0], e[1]]
        }
        Encap::Ppp => match ethertype {
            netpkt::eth::ET_IPV6 => PPP_IPV6,
            _ => PPP_IPV4,
        },
    }
}

/// a frame of a serial line that starts as PPP does (Cisco HDLC starts with 0x0F or 0x8F)
pub fn is_ppp(frame: &[u8]) -> bool {
    frame.len() >= 4 && frame[0] == 0xFF && frame[1] == 0x03
}

/// the ethertype of a PPP protocol number (None: not one the simulator carries)
pub fn ppp_ethertype(proto: u16) -> Option<u16> {
    match proto {
        0x0021 => Some(netpkt::eth::ET_IPV4),
        0x0057 => Some(netpkt::eth::ET_IPV6),
        _ => None,
    }
}

/// the usual rate nearest to `bps` (the lower one of two as near)
pub fn std_rate(bps: u32) -> u32 {
    STD_RATES.iter().copied().min_by_key(|&r| (r.abs_diff(bps), r)).unwrap_or(bps)
}

/// the commands: "clock rate", "encapsulation" in interface mode, "show controllers" in EXEC
pub(crate) fn build_serial(t: &mut Tree<K>, m: [ModeId; 3]) {
    let [user, prv, ifm] = m;
    t.add(ifm, "clock", "Clock of a serial line (given by its DCE end)", None, F_NO);
    t.add(ifm, "clock rate", "How fast the DCE end clocks the line", None, 0);
    t.add(ifm, "clock rate <1200-8000000>=bps", RATE_HELP, Some(K::ClockRate), 0);
    t.add_no(ifm, "clock rate", K::ClockRate);
    t.add(ifm, "encapsulation", "Frame format of this serial line", None, F_NO);
    t.add(ifm, "encapsulation hdlc=encap", "HDLC frames (the default)", Some(K::SerialEncap), 0);
    t.add(ifm, "encapsulation ppp=encap", "PPP frames (RFC 1662 framing)", Some(K::SerialEncap), 0);
    t.add_no(ifm, "encapsulation", K::SerialEncap);
    for x in [user, prv] {
        t.add(x, "show controllers", "The serial ports' hardware: cable end and clock", Some(K::ShowControllers), 0);
        t.add(x, "show controllers NAME1=if", "", Some(K::ShowControllers), 0);
    }
}

impl Engine {
    // what end `k` of a link was told
    fn serial_end(&self, l: &LinkRt, k: usize) -> Option<SerialCfg> {
        let (n, p) = l.ends[k];
        self.nodes.get(n)?.as_ref()?.ports.get(p as usize).map(|x| x.serial)
    }

    /// why the line protocol of serial link slot `li` stays down (None: it may come up, or it
    /// is no serial line)
    pub fn serial_fault(&self, li: usize) -> Option<SerialFault> {
        let l = self.links.get(li)?.as_ref()?;
        if !l.cable.is_serial() {
            return None;
        }
        let ends = [self.serial_end(l, 0)?, self.serial_end(l, 1)?];
        if let Some(d) = l.cable.dce_end()
            && ends[d].clock == LineClock::Off
        {
            return Some(SerialFault::NoClock { dce: d as u8 });
        }
        if ends[0].encap != ends[1].encap {
            return Some(SerialFault::Encap([ends[0].encap, ends[1].encap]));
        }
        None
    }

    /// the clock rate (bit/s) the DCE end of serial link slot `li` sets (None: none set, or no
    /// end chosen)
    pub(crate) fn serial_bps(&self, li: usize) -> Option<u64> {
        let l = self.links.get(li)?.as_ref()?;
        let d = l.cable.dce_end()?;
        match self.serial_end(l, d)?.clock {
            LineClock::Rate(n) => Some(n as u64),
            _ => None,
        }
    }

    /// what serial port `port` of device `dev` was told (None: no such device or port)
    pub fn serial_of(&self, dev: u32, port: &str) -> Option<SerialCfg> {
        let ni = self.node_index(dev)?;
        let pi = self.port_index(dev, port)?;
        Some(self.serial_cfg(ni, pi))
    }

    /// what a serial port of node `ni` was told
    pub(crate) fn serial_cfg(&self, ni: usize, pi: u16) -> SerialCfg {
        self.nodes.get(ni).and_then(|n| n.as_ref()).and_then(|n| n.ports.get(pi as usize)).map(|p| p.serial).unwrap_or_default()
    }

    // changes what serial port `pi` of node `ni` was told; its line again
    fn serial_set(&mut self, ni: usize, pi: u16, f: impl FnOnce(&mut SerialCfg)) {
        let li = match self.nodes.get_mut(ni) {
            Some(Some(n)) => match n.ports.get_mut(pi as usize) {
                Some(p) => {
                    f(&mut p.serial);
                    p.link
                }
                None => return,
            },
            _ => return,
        };
        if let Some(li) = li {
            self.refresh_link(li);
        }
    }

    /// "clock rate N", "no clock rate", "encapsulation hdlc|ppp", "no encapsulation" for one
    /// interface; any other than a serial port refuses as IOS does (the marker under the
    /// command)
    pub(crate) fn serial_if_cmd(&mut self, ni: usize, name: &str, p: &Parsed<K>, line: &str) -> Result<(), String> {
        let dev = self.dev_of(ni);
        let pi = if name.contains('.') { None } else { self.port_index(dev, name) };
        let serial = pi.filter(|&pi| self.nodes.get(ni).and_then(|n| n.as_ref()).is_some_and(|n| n.ports.get(pi as usize).is_some_and(|x| x.spec.media == Media::Serial)));
        let Some(pi) = serial else {
            let at = line.len() - line.trim_start().len();
            return self.ios_marker(ni, line, at);
        };
        match p.cmd {
            K::ClockRate => {
                let clock = if p.no { LineClock::Off } else { LineClock::Rate(std_rate(p.num("bps").unwrap_or(64000) as u32)) };
                self.serial_set(ni, pi, |c| c.clock = clock);
            }
            _ => {
                let encap = if p.no || p.text("encap") == Some("hdlc") { Encap::Hdlc } else { Encap::Ppp };
                self.serial_set(ni, pi, |c| c.encap = encap);
            }
        }
        Ok(())
    }

    /// the notes of a serial line whose protocol went down for a serial reason (link.rs:
    /// refresh_link, at both powered ends)
    pub(crate) fn serial_down(&mut self, li: usize) {
        let Some(f) = self.serial_fault(li) else { return };
        let Some(Some(l)) = self.links.get(li) else { return };
        let ends = l.ends;
        for k in 0..2 {
            let (ni, pi) = ends[k];
            let (dev, power) = match self.nodes.get(ni) {
                Some(Some(n)) => (n.dev, n.power),
                _ => continue,
            };
            if !power {
                continue;
            }
            let note = match f {
                SerialFault::NoClock { dce } => SerialNote::NoClock { port: pi, dce_here: dce as usize == k },
                SerialFault::Encap(e) => SerialNote::Encap { port: pi, own: e[k], other: e[1 - k] },
            };
            self.note(dev, None, Note::Serial(note));
        }
    }

    // the end (0, 1) of serial port `pi` of node `ni` on its line, the line's slot
    fn serial_line_of(&self, ni: usize, pi: u16) -> Option<(usize, &LinkRt, u8)> {
        let li = self.nodes.get(ni)?.as_ref()?.ports.get(pi as usize)?.link?;
        let l = self.links.get(li)?.as_ref()?;
        Some((li, l, l.end_of(ni, pi)?))
    }

    /// "show interfaces" of serial port `pi`: the encapsulation lines (`proto`: the line
    /// protocol is up) and, when it is down for a serial reason, a line in our words that says
    /// why
    pub(crate) fn serial_show(&self, ni: usize, pi: u16, proto: bool) -> String {
        let cfg = self.serial_cfg(ni, pi);
        let mut s = match (cfg.encap, proto) {
            (Encap::Hdlc, _) => String::from("  Encapsulation HDLC, crc 16, loopback not set\n  Keepalive set (10 sec)\n"),
            (Encap::Ppp, true) => String::from("  Encapsulation PPP, LCP Open\n  Open: IPCP, crc 16, loopback not set\n  Keepalive set (10 sec)\n"),
            (Encap::Ppp, false) => String::from("  Encapsulation PPP, LCP Closed, crc 16, loopback not set\n  Keepalive set (10 sec)\n"),
        };
        if let Some((li, _, end)) = self.serial_line_of(ni, pi) {
            match self.serial_fault(li) {
                Some(SerialFault::NoClock { dce }) if dce == end => s.push_str("  Line protocol down: this DCE end gives no clock (no clock rate)\n"),
                Some(SerialFault::NoClock { .. }) => s.push_str("  Line protocol down: no clock from the DCE end\n"),
                Some(SerialFault::Encap(e)) => s.push_str(&format!("  Line protocol down: the far end runs {}, this end {}\n", e[1 - end as usize].name(), e[end as usize].name())),
                None => {}
            }
        }
        s
    }

    /// the running configuration's lines of serial port `pi`: before " shutdown" (" encapsulation
    /// ppp") and after it (" clock rate N", " no clock rate"); empty for the defaults
    pub(crate) fn serial_if_config(&self, ni: usize, pi: u16) -> (String, String) {
        let Some(Some(n)) = self.nodes.get(ni) else {
            return (String::new(), String::new());
        };
        let Some(p) = n.ports.get(pi as usize).filter(|p| p.spec.media == Media::Serial) else {
            return (String::new(), String::new());
        };
        let encap = if p.serial.encap == Encap::Ppp { String::from(" encapsulation ppp\n") } else { String::new() };
        let clock = match p.serial.clock {
            LineClock::Auto => String::new(),
            LineClock::Rate(r) => format!(" clock rate {}\n", r),
            LineClock::Off => String::from(" no clock rate\n"),
        };
        (encap, clock)
    }

    /// "show controllers [serial N]": per serial port its cable end and the clock
    pub(crate) fn ios_controllers(&self, ni: usize, only: Option<&str>) -> String {
        let Some(Some(n)) = self.nodes.get(ni) else {
            return String::new();
        };
        let mut s = String::new();
        for (pi, p) in n.ports.iter().enumerate() {
            if p.spec.media != Media::Serial || only.is_some_and(|o| !o.eq_ignore_ascii_case(&p.spec.name)) {
                continue;
            }
            let pi = pi as u16;
            if !s.is_empty() {
                s.push('\n');
            }
            s.push_str(&format!("Interface {}\nHardware is a synchronous serial port ({} kbit/s)\n", p.spec.name, p.spec.kbps));
            let own = p.serial.clock;
            let Some((li, l, end)) = self.serial_line_of(ni, pi) else {
                s.push_str("No serial cable attached\n");
                continue;
            };
            let bps = self.link_kbps(li) as u64 * 1000;
            let Some(dce) = l.cable.dce_end() else {
                s.push_str(&format!("V.35 cable without a chosen end: no DCE, the line runs at its link's {} bit/s\n", bps));
                if let LineClock::Rate(r) = own {
                    s.push_str(&format!("clock rate {} is set here; with no DCE end it does nothing\n", r));
                }
                continue;
            };
            let far = self.serial_end(l, dce).map(|c| c.clock).unwrap_or_default();
            if dce == end as usize {
                match own {
                    LineClock::Auto => s.push_str(&format!("DCE V.35, clock rate {}\n", bps)),
                    LineClock::Rate(r) => {
                        s.push_str(&format!("DCE V.35, clock rate {}\n", r));
                        if r as u64 > bps {
                            s.push_str(&format!("the line runs at {} bit/s, the most its link carries\n", bps));
                        }
                    }
                    LineClock::Off => s.push_str("DCE V.35, no clock\n"),
                }
            } else {
                if far == LineClock::Off {
                    s.push_str("DTE V.35 clocks stopped: the DCE end gives no clock\n");
                } else {
                    s.push_str("DTE V.35 TX and RX clocks detected.\n");
                }
                if let LineClock::Rate(r) = own {
                    s.push_str(&format!("clock rate {} is set here; a DTE end takes its clock from the DCE end, so it does nothing\n", r));
                }
            }
        }
        // an interface that is no serial port (our words; the original shows its chip)
        if s.is_empty()
            && let Some(o) = only
        {
            s = format!("% {} is no serial port: show controllers shows the serial ports\n", o);
        }
        s
    }

    /// the problems: serial lines without a clock or with two frame formats (problems.rs)
    pub(crate) fn serial_problems(&self, out: &mut Vec<Problem>) {
        for (li, l) in self.links.iter().enumerate() {
            let Some(l) = l else { continue };
            let Some(f) = self.serial_fault(li) else { continue };
            let dev = |k: usize| self.dev_of(l.ends[k].0);
            let name = |k: usize| self.end_name((dev(k), l.ends[k].1));
            let (kind, m) = match f {
                SerialFault::NoClock { dce } => (ProblemKind::SerialNoClock, msg!("Serielle Leitung zwischen {} und {}: das DCE-Ende {} gibt keinen Takt (no clock rate); das Leitungsprotokoll bleibt aus.", name(0), name(1), name(dce as usize))),
                SerialFault::Encap(e) => (ProblemKind::SerialEncap, msg!("Serielle Leitung zwischen {} und {}: {} an {}, {} an {}; das Leitungsprotokoll bleibt aus.", name(0), name(1), e[0].name(), name(0), e[1].name(), name(1))),
            };
            out.push(Problem { kind, severity: Severity::Error, dev: dev(0), port: Some(l.ends[0].1), link: Some(l.id), others: alloc::vec![dev(1)], text: m.text(), msg: m });
        }
    }
}
