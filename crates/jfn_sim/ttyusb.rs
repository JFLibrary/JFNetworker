// ttyusb -- the console cable at work (S1): a terminal program on a Linux host ("screen
// /dev/ttyUSB0", "picocom", "minicom") that talks to the device at the other end of the console
// cable on the host's USB port, through a command-line session there on its console line
//
//   ConsoleClient            the program (a console program like telnetd.rs's TelnetClient,
//                            without TCP: the cable is the path); far(): the device and session
//                            it talks to
//   Tty                      what a cable session knows of its terminal (SesInfo::tty): the host
//                            and its port, the program, the cable, the lines typed there
//   Engine::sh_serial_term   the shell's screen / picocom / minicom
//   Engine::tty_attach / tty_input / tty_wake / tty_out / tty_detach
//                            the program takes the line (a new session, or the one that waits),
//                            its typed lines into the session, the session's output back
//   Engine::tty_dropped / tty_unplugged   the device went off (the session went with it), the
//                            cable is gone (the program ends)
//   Engine::tty_exit / tty_cleared        exit / logout and "clear line console 0" in a cable
//                            session: back to the console's login or "press Enter"
//
// The session (SesKind::Cable, cli/ses.rs) is the device's console line as the cable reaches
// it: line "con 0" with the console's login ("line con 0": password, login local, AAA), console
// messages always (as a console shows them), "exit" back to the login or to "press Enter" as at
// the console, listed by "show users" with the host's port as its place and on the "con 0" row of
// "show line".  The app's own console (the double click) stays a session of its own.  The cable
// session stays when the program ends, the cable is pulled or the host goes off -- a console line
// keeps its user logged in -- and the next program on the cable finds it as it was, with its
// prompt again.  Switching the device off ends it.
//
// The program: no console cable on usbN (or a path other than /dev/ttyUSBn): "Cannot open
// /dev/ttyUSB0: No such file or directory" (picocom and minicom in their words).  The device at
// the far end off (or without a command line): it opens, and a line in our words says that
// nothing answers; a line typed after the device came back finds the session, as Enter on a
// real console would.  The device going off meanwhile: the same.  The cable unplugged or removed:
// the program ends ("[screen is terminating]").  A second program on the same line: "Device or
// resource busy".  The escape is Ctrl+C in the shell, as for telnet (the shell ends the program;
// no Ctrl+C reaches the far device).  The speed is taken and not checked (the console line runs
// at 9600 baud), and text arrives at once, not at the speed of the line.  Lines go as whole lines
// (the console has its own line editor and ends a hidden line itself), and a password typed for
// the far end's login stays hidden (prog_hides in telnetd.rs asks the far session).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;

use jfn_model::cable::CableKind;
use jfn_model::catalog::CliStyle;

use crate::cli::ses::{SesKind, W_TTY};
use crate::cli::Mode;
use crate::kernel::{Engine, Ev};
use crate::os::{ConsoleEv, Metric, OsApi, Service, SvcEvent};

/// which terminal program
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prog {
    Screen,
    Picocom,
    Minicom,
}

impl Prog {
    fn name(self) -> &'static str {
        match self {
            Prog::Screen => "screen",
            Prog::Picocom => "picocom",
            Prog::Minicom => "minicom",
        }
    }
    fn usage(self) -> &'static str {
        match self {
            Prog::Screen => "usage: screen /dev/ttyUSB0 [BAUD]\n(this screen opens serial lines only)\n",
            Prog::Picocom => "usage: picocom [-b BAUD] /dev/ttyUSB0\n",
            Prog::Minicom => "usage: minicom -D /dev/ttyUSB0 [-b BAUD]\n",
        }
    }
    fn no_file(self, path: &str) -> String {
        match self {
            Prog::Screen => format!("Cannot open {}: No such file or directory\n", path),
            Prog::Picocom => format!("FATAL: cannot open {}: No such file or directory\n", path),
            Prog::Minicom => format!("minicom: cannot open {}: No such file or directory\n", path),
        }
    }
    fn busy(self, path: &str) -> String {
        match self {
            Prog::Screen => format!("Cannot open {}: Device or resource busy\n", path),
            Prog::Picocom => format!("FATAL: cannot lock {}: Resource temporarily unavailable\n", path),
            Prog::Minicom => format!("Device {} is locked.\n", path),
        }
    }
    fn banner(self, path: &str, baud: u32) -> String {
        match self {
            Prog::Screen => format!("Connected to {} at {} baud. Ctrl+C ends screen.\n", path, baud),
            Prog::Picocom => format!("picocom on {}, {} baud\nTerminal ready (Ctrl+C ends picocom)\n", path, baud),
            Prog::Minicom => format!("minicom on {}, {} 8N1. Ctrl+C ends minicom.\n", path, baud),
        }
    }
    fn gone(self, path: &str) -> String {
        match self {
            Prog::Screen => String::from("\n[screen is terminating]\n"),
            Prog::Picocom => format!("\nTerminating: {} is gone\n", path),
            Prog::Minicom => format!("\nminicom: {} is gone\n", path),
        }
    }
}

/// what a cable session knows of its terminal (SesInfo::tty; None: no program on the cable)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Tty {
    /// the host and its port with the cable ("PC1 usb0" in show users)
    pub dev: u32,
    pub port: u16,
    /// the program there (its service id)
    pub svc: u32,
    /// the cable's engine slot
    pub link: usize,
    /// lines typed there that wait for the session
    pub inq: VecDeque<String>,
    /// a wake-up for them is scheduled
    pub woken: bool,
    /// a program found the session waiting: its prompt again
    pub fresh: bool,
}

/// why a program cannot take the line
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TtyErr {
    /// no console cable (any more)
    NoCable,
    /// the device at the far end is off, or has no command line
    Off,
    /// another program has the line
    Busy,
}

/// the terminal program of a console cable ("screen", "picocom", "minicom")
#[derive(Clone, Debug)]
pub struct ConsoleClient {
    prog: Prog,
    /// "/dev/ttyUSB0"
    path: String,
    /// the host's port of it ("usb0")
    port: String,
    baud: u32,
    /// the cable's engine slot
    link: Option<usize>,
    /// the device and its session at the far end while the line is open
    far: Option<(u32, u32)>,
}

impl ConsoleClient {
    /// the device and session at the far end while the program has the line
    pub fn far(&self) -> Option<(u32, u32)> {
        self.far
    }

    // the session at the far end takes the line (`first`: at the start, the banner and a line
    // when nothing answers)
    fn attach(&mut self, os: &mut OsApi, first: bool) {
        let Some(li) = self.link else {
            return;
        };
        let (ni, id) = (os.ni, os.id);
        let r = os.eng.tty_attach(ni, id, li);
        if first && matches!(r, Ok(_) | Err(TtyErr::Off)) {
            os.print(&self.prog.banner(&self.path, self.baud));
        }
        match r {
            Ok(f) => self.far = Some(f),
            Err(TtyErr::Off) => {
                if first {
                    os.print(&format!("(No answer on {}: the device at the far end is off. Enter tries again.)\n", self.path));
                }
            }
            Err(TtyErr::Busy) => {
                os.print_err(&self.prog.busy(&self.path));
                os.exit(1);
            }
            Err(TtyErr::NoCable) => {
                os.print_err(&self.prog.gone(&self.path));
                os.exit(1);
            }
        }
    }

    /// the session at the far end went with its device (or closed): nothing answers until a
    /// line typed later finds the device again
    pub(crate) fn far_gone(&mut self, os: &mut OsApi, off: bool) {
        self.far = None;
        if off {
            os.print(&format!("\n(No answer on {} any more: the device at the far end went off. Enter tries again.)\n", self.path));
        } else {
            os.print(&format!("\n(The far end closed {}. Enter opens it again.)\n", self.path));
        }
    }

    /// the cable is gone: the program ends (the session at the far end stays, as a console
    /// line keeps its user)
    pub(crate) fn cable_gone(&mut self, os: &mut OsApi) {
        if let Some((dev, s)) = self.far.take() {
            let (ni, id) = (os.ni, os.id);
            os.eng.tty_detach(dev, s, ni, id);
        }
        os.print_err(&self.prog.gone(&self.path));
        os.exit(1);
    }
}

impl Service for ConsoleClient {
    fn kind(&self) -> &'static str {
        self.prog.name()
    }
    fn start(&mut self, os: &mut OsApi) {
        let ni = os.ni;
        match os.eng.tty_cable(ni, &self.port) {
            Some(li) => self.link = Some(li),
            None => {
                os.print_err(&self.prog.no_file(&self.path));
                os.exit(1);
                return;
            }
        }
        self.attach(os, true);
    }
    fn on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        if let SvcEvent::Console(ConsoleEv::Line(l)) = ev {
            match self.far {
                Some((dev, s)) => {
                    let (ni, id) = (os.ni, os.id);
                    os.eng.tty_input(dev, s, ni, id, &l);
                }
                // the line is the Enter that wakes the device's console, if it answers now
                None => self.attach(os, false),
            }
        }
    }
    fn stop(&mut self, os: &mut OsApi) {
        // Ctrl+C, the host off: the session at the far end stays, without its terminal
        if let Some((dev, s)) = self.far.take() {
            let (ni, id) = (os.ni, os.id);
            os.eng.tty_detach(dev, s, ni, id);
        }
    }
    fn metrics(&self) -> Vec<Metric> {
        Vec::new()
    }
    fn status(&self) -> String {
        format!("{} {} {}", self.prog.name(), self.path, self.baud)
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

impl Engine {
    /// "screen DEV [BAUD]", "picocom [-b BAUD] DEV", "minicom -D DEV [-b BAUD] [-o]" in the shell
    pub(crate) fn sh_serial_term(&mut self, ni: usize, cmd: &str, args: &[&str]) -> Result<(), String> {
        let prog = match cmd {
            "picocom" => Prog::Picocom,
            "minicom" => Prog::Minicom,
            _ => Prog::Screen,
        };
        let (mut path, mut baud, mut bad) = (None, None, false);
        let mut i = 0;
        while i < args.len() {
            let a = args[i];
            match (prog, a) {
                (Prog::Picocom | Prog::Minicom, "-b" | "--baud") => {
                    baud = args.get(i + 1).copied();
                    bad |= baud.is_none();
                    i += 1;
                }
                (Prog::Minicom, "-D" | "--device") => {
                    path = args.get(i + 1).copied();
                    bad |= path.is_none();
                    i += 1;
                }
                (Prog::Minicom, "-o") => {}
                (_, x) if x.starts_with('-') => bad = true,
                (Prog::Screen, x) if path.is_some() && baud.is_none() => baud = Some(x),
                (Prog::Picocom | Prog::Screen, x) if path.is_none() => path = Some(x),
                _ => bad = true,
            }
            i += 1;
        }
        let baud = match baud {
            None => Some(9600),
            Some(b) => b.parse::<u32>().ok().filter(|&b| b > 0),
        };
        let (Some(path), Some(baud), false) = (path, baud, bad) else {
            self.err_out(ni, prog.usage());
            return Err(String::from(prog.usage().lines().next().unwrap_or("")));
        };
        // /dev/ttyUSBn is the host's port usbN; nothing else is a serial line here
        let port = path.strip_prefix("/dev/ttyUSB").filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit())).map(|n| format!("usb{}", n));
        if port.as_deref().is_none_or(|p| self.tty_cable(ni, p).is_none()) {
            let t = prog.no_file(path);
            self.err_out(ni, &t);
            return Err(String::from(t.trim_end()));
        }
        let c = ConsoleClient { prog, path: String::from(path), port: port.unwrap_or_default(), baud, link: None, far: None };
        self.run_program(ni, prog.name(), Box::new(c), true)
    }

    /// console cable slot `li` has its carrier: plugged in and not cut (the power of its ends
    /// does not matter: the adapter in the cable is the host's)
    pub(crate) fn console_carrier(&self, li: usize) -> bool {
        match self.links.get(li) {
            Some(Some(l)) => l.cable == CableKind::Console && !l.down && !self.faults.link(li).is_some_and(|o| o.cut > 0),
            _ => false,
        }
    }

    /// the console cable on port `port` ("usb0") of node `ni`: its slot (None: no such port, no
    /// cable, another kind of cable, unplugged)
    pub(crate) fn tty_cable(&self, ni: usize, port: &str) -> Option<usize> {
        let n = self.nodes.get(ni)?.as_ref()?;
        let li = n.ports.iter().find(|p| p.spec.name.eq_ignore_ascii_case(port))?.link?;
        self.console_carrier(li).then_some(li)
    }

    // the program of a terminal still runs
    fn tty_alive(&self, dev: u32, svc: u32) -> bool {
        self.node_index(dev).and_then(|pc| self.os(pc)).is_some_and(|o| o.running(svc))
    }

    /// the program `svc` of node `ni` takes console cable `li`: the cable session at the far end
    /// (made, or the one that waits) -> (device, session)
    pub(crate) fn tty_attach(&mut self, ni: usize, svc: u32, li: usize) -> Result<(u32, u32), TtyErr> {
        if !self.console_carrier(li) {
            return Err(TtyErr::NoCable);
        }
        let Some(Some(l)) = self.links.get(li) else {
            return Err(TtyErr::NoCable);
        };
        let k = if l.ends[0].0 == ni { 1 } else { 0 };
        let (far, port) = (l.ends[k].0, l.ends[1 - k].1);
        let on = self.nodes.get(far).is_some_and(|n| n.as_ref().is_some_and(|n| n.power)) && self.cdev(far).is_some_and(|c| c.style != CliStyle::None);
        if !on {
            return Err(TtyErr::Off);
        }
        let (pc, fdev) = (self.dev_of(ni), self.dev_of(far));
        let tty = Tty { dev: pc, port, svc, link: li, inq: VecDeque::new(), woken: false, fresh: false };
        let found = self.cdev(far).and_then(|c| c.sessions.iter().find(|i| i.kind == SesKind::Cable && !i.ending).map(|i| (i.id, i.tty.as_ref().map(|t| (t.dev, t.svc)))));
        match found {
            Some((_, Some((d, x)))) if self.tty_alive(d, x) => Err(TtyErr::Busy),
            Some((s, _)) => {
                // the session waited: its prompt again (at once, outside the program's call)
                if let Some(i) = self.ses_info_mut(far, s) {
                    i.tty = Some(Tty { fresh: true, woken: true, ..tty });
                }
                let now = self.now;
                self.schedule(now, Ev::Ses { node: far, ses: s, epoch: W_TTY });
                Ok((fdev, s))
            }
            None => {
                let s = self.ses_new(far, SesKind::Cable, None, Some(0)).ok_or(TtyErr::Off)?;
                if let Some(i) = self.ses_info_mut(far, s) {
                    i.tty = Some(tty);
                    // a console shows the console's messages
                    i.monitor = true;
                }
                self.ses_start_later(far, s);
                Ok((fdev, s))
            }
        }
    }

    /// a line typed into the program `svc` of node `ni` for session `s` of device `dev`: it waits
    /// for the session, which takes it at the same moment (outside the program's call)
    pub(crate) fn tty_input(&mut self, dev: u32, s: u32, ni: usize, svc: u32, line: &str) {
        let pc = self.dev_of(ni);
        let Some(far) = self.node_index(dev) else {
            return;
        };
        let Some(t) = self.ses_info_mut(far, s).and_then(|i| i.tty.as_mut()).filter(|t| t.dev == pc && t.svc == svc) else {
            return;
        };
        t.inq.push_back(String::from(line));
        if t.woken {
            return;
        }
        t.woken = true;
        let now = self.now;
        self.schedule(now, Ev::Ses { node: far, ses: s, epoch: W_TTY });
    }

    /// W_TTY with the cable session in place: its prompt again for a program that found it, then
    /// the lines typed there
    pub(crate) fn tty_wake(&mut self, ni: usize) {
        let fresh = match self.ses_here_mut(ni).and_then(|i| i.tty.as_mut()) {
            Some(t) => {
                t.woken = false;
                core::mem::take(&mut t.fresh)
            }
            None => return,
        };
        if fresh {
            if let Some(i) = self.ses_here_mut(ni) {
                i.prompted = false;
            }
            self.cli_state(ni);
        }
        loop {
            if self.ses_here(ni).is_none_or(|i| i.ending) {
                return;
            }
            let Some(line) = self.ses_here_mut(ni).and_then(|i| i.tty.as_mut()).and_then(|t| t.inq.pop_front()) else {
                return;
            };
            self.ses_took_line(ni);
            self.cli_line_at(ni, &line);
        }
    }

    /// output of cable session `s` of node `ni`: to its program, if one has the line (else it is
    /// lost, as on a console nobody watches)
    pub(crate) fn tty_out(&mut self, ni: usize, s: u32, text: &str) {
        let Some((dev, svc)) = self.ses_info(ni, s).and_then(|i| i.tty.as_ref()).map(|t| (t.dev, t.svc)) else {
            return;
        };
        let Some(pc) = self.node_index(dev) else {
            return;
        };
        if self.os(pc).is_some_and(|o| o.running(svc)) {
            self.prog_print(pc, svc, text, false);
        }
    }

    /// the program `svc` of node `ni` lets go of session `s` of device `dev` (it ended, the cable
    /// went): the session stays, without its terminal
    pub(crate) fn tty_detach(&mut self, dev: u32, s: u32, ni: usize, svc: u32) {
        let pc = self.dev_of(ni);
        let Some(far) = self.node_index(dev) else {
            return;
        };
        if let Some(i) = self.ses_info_mut(far, s)
            && i.tty.as_ref().is_some_and(|t| t.dev == pc && t.svc == svc)
        {
            i.tty = None;
        }
    }

    /// a cable session of node `ni` went (cli/ses.rs ses_drop: the device went off): its program
    /// hears it
    pub(crate) fn tty_dropped(&mut self, ni: usize, t: Tty) {
        let off = !self.nodes.get(ni).is_some_and(|n| n.as_ref().is_some_and(|n| n.power));
        let Some(pc) = self.node_index(t.dev) else {
            return;
        };
        self.svc_call(pc, t.svc, None, |svc, os| {
            if let Some(c) = svc.as_any_mut().downcast_mut::<ConsoleClient>() {
                c.far_gone(os, off);
            }
        });
    }

    /// console cable slot `li` lost its carrier (link.rs: refresh_link): the programs on it end
    pub(crate) fn tty_unplugged(&mut self, li: usize) {
        let Some(Some(l)) = self.links.get(li) else {
            return;
        };
        let ends = l.ends;
        for (n, _) in ends {
            let ids: Vec<u32> = self
                .os(n)
                .map(|o| o.svcs.iter().filter(|s| s.svc.as_ref().and_then(|x| x.as_any().downcast_ref::<ConsoleClient>()).is_some_and(|c| c.link == Some(li))).map(|s| s.id).collect())
                .unwrap_or_default();
            for id in ids {
                self.svc_call(n, id, None, |svc, os| {
                    if let Some(c) = svc.as_any_mut().downcast_mut::<ConsoleClient>() {
                        c.cable_gone(os);
                    }
                });
            }
        }
    }

    /// the session in place is a cable session
    pub(crate) fn tty_here(&self, ni: usize) -> bool {
        self.ses_here(ni).is_some_and(|i| i.kind == SesKind::Cable)
    }

    /// "PC1 usb0": where a cable session's terminal is
    pub(crate) fn tty_where(&self, t: &Tty) -> String {
        self.end_name((t.dev, t.port))
    }

    /// the far end of a console program waits for a line typed without echo (a password)
    pub(crate) fn tty_hides(&self, far: Option<(u32, u32)>) -> bool {
        far.is_some_and(|(dev, s)| self.cli_ses_secret(dev, s))
    }

    /// exit / logout in a cable session of an IOS-like device: as at the console, the line's
    /// login again or "press Enter" (the session stays on the cable)
    pub(crate) fn tty_exit(&mut self, ni: usize) {
        let via = self.ses_req(ni);
        let user = self.cdev(ni).map(|c| c.user.clone()).unwrap_or_default();
        if !user.is_empty() {
            self.aaa_acct(ni, via, &user, false);
        }
        if self.line_login_plan(ni, "con 0").is_some() {
            self.set_mode(ni, Mode::User);
            self.out(ni, "\nConsole session closed.\n\n");
            self.line_login(ni, "con 0", via);
            return;
        }
        let host = self.host(ni);
        self.out(ni, &format!("\n{}: session closed.\n\nConsole ready, press Enter to log in.\n\n", host));
        self.set_mode(ni, Mode::Out);
        if let Some(c) = self.cdev_mut(ni) {
            c.priv_level = 1;
            c.user.clear();
        }
    }

    /// "clear line console 0": the cable sessions log out as the console does (telnetd.rs)
    pub(crate) fn tty_cleared(&mut self, ni: usize) {
        let ids: Vec<u32> = self.cdev(ni).map(|c| c.sessions.iter().filter(|i| i.kind == SesKind::Cable && !i.ending).map(|i| i.id).collect()).unwrap_or_default();
        for s in ids {
            self.in_ses(ni, s, |e| {
                if e.cdev(ni).is_some_and(|c| c.job.is_some()) {
                    e.cli_interrupt_at(ni);
                }
                e.out(ni, "\n% This line was cleared from another session.\n");
                e.set_mode(ni, Mode::User);
                if e.line_login_plan(ni, "con 0").is_some() {
                    let via = e.ses_req(ni);
                    e.line_login(ni, "con 0", via);
                } else {
                    e.set_mode(ni, Mode::Out);
                    e.out(ni, "\nConsole ready, press Enter to log in.\n\n");
                }
                if let Some(i) = e.ses_here_mut(ni) {
                    i.prompted = false;
                }
                e.cli_state(ni);
            });
        }
    }
}
