// pdutest -- checks of the simulation mode's PDUs in jfn_sim (part of --selftest): UDP
// datagrams with their own TTL, size and pattern, a port nobody listens on (ICMP back), no
// route, TCP connections to an echo service and to a closed port, a name from the hosts
// file and one nobody knows, sending again and again until stopped, and the driver's Seek
//
// The LAN: PC1 (10.0.0.1), PC2 (10.0.0.2) and SRV (10.0.0.10, TCP echo on port 7) on a
// switch; ARP is learned on the way.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

use agent_core::jmap::Value;
use jfn_model::project::{PortRef, Project, ServiceDef};
use jfn_sim::pdu::payload;
use jfn_sim::{Action, Cmd, Engine, Inline, PduProto, PduSender, PduSpec, MS, SEC};
use netpkt::ipv4::{Ipv4View, PROTO_ICMP, PROTO_UDP};
use netpkt::udp::UdpView;
use netpkt::{Ipv4, Prefix4};

use crate::selftest::T;

const BUDGET: usize = 2_000_000;

fn pr(dev: u32, port: &str) -> PortRef {
    PortRef { dev, port: String::from(port) }
}

fn ip(s: &str) -> Ipv4 {
    Ipv4::parse(s).unwrap_or(Ipv4::UNSPECIFIED)
}

/// PC1, PC2 and SRV (TCP echo on 7) on a switch: (engine, pc1, pc2, srv, ok)
fn lan() -> (Engine, u32, u32, u32, bool) {
    let mut p = Project::new("pdu", 5);
    let pc1 = p.add_device("pc", 0, 0).unwrap_or(0);
    let pc2 = p.add_device("pc", 200, 0).unwrap_or(0);
    let srv = p.add_device("server-1u", 300, 100).unwrap_or(0);
    let sw = p.add_device("jf-s24p", 100, 100).unwrap_or(0);
    let _ = p.connect(&pr(pc1, "eth0"), &pr(sw, "gi1/0/1"), None);
    let _ = p.connect(&pr(pc2, "eth0"), &pr(sw, "gi1/0/2"), None);
    let _ = p.connect(&pr(srv, "eth0"), &pr(sw, "gi1/0/3"), None);
    let mut e = Engine::from_project(&p).unwrap_or_else(|_| Engine::new(0));
    let addr = |dev: u32, a: &str| Action::IfAddr { dev, iface: String::from("eth0"), prefix: Prefix4::parse(a).unwrap_or(Prefix4 { addr: Ipv4::UNSPECIFIED, len: 0 }), add: true };
    let echo = ServiceDef { kind: String::from("tcp-echo"), name: String::from("echo"), disabled: false, config: agent_core::json::parse(b"{\"port\": 7}").unwrap_or(Value::Null) };
    let ok = [e.apply(addr(pc1, "10.0.0.1/24")), e.apply(addr(pc2, "10.0.0.2/24")), e.apply(addr(srv, "10.0.0.10/24")), e.apply(Action::Service { dev: srv, def: echo })];
    (e, pc1, pc2, srv, ok.iter().all(|r| r.is_ok()))
}

fn spec(proto: PduProto, dst: &str, dport: u16, size: u32, ttl: Option<u8>, count: u32, interval: u64) -> PduSpec {
    PduSpec { proto, dst: String::from(dst), sport: 0, dport, size, ttl, count, interval }
}

/// a PDU service on `dev` -> it was added
fn add(e: &mut Engine, dev: u32, name: &str, s: PduSpec) -> bool {
    e.add_service(dev, name, Box::new(PduSender::new(s)), true).is_ok()
}

fn sender<'a>(e: &'a Engine, dev: u32, name: &str) -> Option<&'a PduSender> {
    e.service::<PduSender>(dev, name)
}

/// the IPv4 packets a device sent: (TTL, protocol, payload)
fn sent_ip(e: &Engine, dev: u32) -> Vec<(u8, u8, Vec<u8>)> {
    e.trace
        .hops()
        .filter(|h| h.from.0 == dev && h.bytes.len() > 14 && h.bytes[12..14] == [0x08, 0x00])
        .filter_map(|h| Ipv4View::parse(&h.bytes[14..]).map(|v| (v.ttl(), v.proto(), v.payload().to_vec())))
        .collect()
}

pub fn run(t: &mut T) {
    udp(t);
    udp_no_route(t);
    tcp(t);
    names(t);
    periodic(t);
    seek(t);
    dialog_source(t);
    frame_links(t);
}

// UDP with a TTL and a size: three datagrams, PC2 answers each with port unreachable
fn udp(t: &mut T) {
    let (mut e, pc1, pc2, _, ok) = lan();
    t.check("udp: set-up", ok);
    t.eq("payload", payload(25), b"abcdefghijklmnopqrstuvwab".to_vec());
    t.check("udp: added", add(&mut e, pc1, "pdu-1", spec(PduProto::Udp, "10.0.0.2", 9, 32, Some(5), 3, 100 * MS)));
    let _ = e.run_until(SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-1");
    t.eq("udp: sent", s.map(|s| (s.sent, s.replies, s.failed)), Some((3, 0, 0)));
    t.check("udp: done", s.is_some_and(|s| s.done() && s.error.is_none()));
    let udp: Vec<(u8, Vec<u8>)> = sent_ip(&e, pc1).into_iter().filter(|p| p.1 == PROTO_UDP).map(|p| (p.0, p.2)).collect();
    t.eq("udp: datagrams with TTL 5", udp.iter().map(|p| p.0).collect::<Vec<_>>(), alloc::vec![5, 5, 5]);
    let u = udp.first().and_then(|p| UdpView::parse(&p.1).map(|v| (v.dst_port(), v.len(), v.payload().to_vec())));
    t.eq("udp: port, length, data", u, Some((9, 40, payload(32))));
    t.eq("udp: port unreachable from PC2", sent_ip(&e, pc2).iter().filter(|p| p.1 == PROTO_ICMP && p.2.first() == Some(&3)).count(), 3);
    // the device's own TTL without one (Linux 64)
    t.check("udp: default ttl added", add(&mut e, pc1, "pdu-2", spec(PduProto::Udp, "10.0.0.2", 9, 0, None, 1, MS)));
    let _ = e.run_until(2 * SEC, BUDGET);
    let last = sent_ip(&e, pc1).into_iter().filter(|p| p.1 == PROTO_UDP).last();
    t.eq("udp: default ttl", last.map(|p| (p.0, p.2.len())), Some((64, 8)));
}

// no route: nothing leaves, the sender says why
fn udp_no_route(t: &mut T) {
    let (mut e, pc1, _, _, _) = lan();
    t.check("no route: added", add(&mut e, pc1, "pdu-1", spec(PduProto::Udp, "192.168.7.7", 53, 10, None, 1, MS)));
    let _ = e.run_until(SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-1");
    t.eq("no route: counts", s.map(|s| (s.sent, s.failed)), Some((0, 1)));
    t.eq("no route: why", s.and_then(|s| s.error.clone()), Some(String::from("keine Route zum Ziel")));
    t.check("no route: done", s.is_some_and(|s| s.done()));
}

// TCP: two connections to the echo service end in order; a closed port refuses
fn tcp(t: &mut T) {
    let (mut e, pc1, _, srv, _) = lan();
    t.check("tcp: added", add(&mut e, pc1, "pdu-1", spec(PduProto::Tcp, "10.0.0.10", 7, 1000, None, 2, 50 * MS)));
    let _ = e.run_until(2 * SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-1");
    t.eq("tcp: connections", s.map(|s| (s.sent, s.replies, s.failed)), Some((2, 2, 0)));
    t.check("tcp: done", s.is_some_and(|s| s.done()));
    t.eq("tcp: echo served", (e.metric(srv, "echo", "connections"), e.metric(srv, "echo", "bytes_in"), e.metric(srv, "echo", "bytes_out")), (Some(2), Some(2000), Some(2000)));
    t.check("tcp: closed port added", add(&mut e, pc1, "pdu-2", spec(PduProto::Tcp, "10.0.0.2", 7, 10, None, 1, MS)));
    let _ = e.run_until(4 * SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-2");
    t.eq("tcp: refused", s.map(|s| (s.sent, s.replies, s.failed)), Some((1, 0, 1)));
    t.eq("tcp: why", s.and_then(|s| s.error.clone()), Some(String::from("abgelehnt (RST auf den SYN)")));
}

// a name from the hosts file is looked up first; one nobody knows ends the PDU
fn names(t: &mut T) {
    let (mut e, pc1, pc2, _, _) = lan();
    t.check("names: hosts entry", e.apply(Action::HostEntry { dev: pc1, name: String::from("pc2.lab"), ip: Some(ip("10.0.0.2")) }).is_ok());
    t.check("names: added", add(&mut e, pc1, "pdu-1", spec(PduProto::Udp, "pc2.lab", 9, 4, None, 1, MS)));
    let _ = e.run_until(SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-1");
    t.eq("names: address", s.and_then(|s| s.dst), Some(ip("10.0.0.2")));
    t.eq("names: sent", s.map(|s| s.sent), Some(1));
    t.eq("names: arrived", sent_ip(&e, pc2).iter().filter(|p| p.1 == PROTO_ICMP).count(), 1);
    t.check("names: unknown added", add(&mut e, pc1, "pdu-2", spec(PduProto::Tcp, "nowhere.lab", 7, 4, None, 1, MS)));
    let _ = e.run_until(30 * SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-2");
    t.check("names: unknown fails", s.is_some_and(|s| s.sent == 0 && s.done() && s.error.as_deref().is_some_and(|w| w.starts_with("„nowhere.lab“"))));
}

// again and again until stopped
fn periodic(t: &mut T) {
    let (mut e, pc1, _, _, _) = lan();
    t.check("periodic: added", add(&mut e, pc1, "pdu-1", spec(PduProto::Udp, "10.0.0.2", 9, 8, None, 0, 10 * MS)));
    let _ = e.run_until(105 * MS, BUDGET);
    t.eq("periodic: every 10 ms", sender(&e, pc1, "pdu-1").map(|s| s.sent), Some(11));
    t.check("periodic: still running", sender(&e, pc1, "pdu-1").is_some_and(|s| !s.done()));
    t.check("periodic: stop", e.set_service_running(pc1, "pdu-1", false).is_ok());
    let _ = e.run_until(SEC, BUDGET);
    let s = sender(&e, pc1, "pdu-1");
    t.eq("periodic: no more after the stop", s.map(|s| s.sent), Some(11));
    t.check("periodic: stopped", s.is_some_and(|s| s.stopped && s.done()));
}

// Seek: any moment of the recording; at or after the present the view is live again
fn seek(t: &mut T) {
    let (e, pc1, _, _, _) = lan();
    let mut d = Inline::new(e);
    let _ = d.send(Cmd::Simulation, 0);
    let _ = d.eng.ping(pc1, ip("10.0.0.2"), jfn_sim::PingOpts { count: 1, ..jfn_sim::PingOpts::default() });
    let _ = d.send(Cmd::RunFor(10 * MS), 0);
    let now = d.eng.now();
    let _ = d.send(Cmd::Seek(1000), 0);
    t.eq("seek: in the past", (d.view(), d.view_time()), (Some(1000), 1000));
    let _ = d.send(Cmd::Seek(now + 5), 0);
    t.eq("seek: the present", d.view(), None);
    let _ = d.send(Cmd::Seek(0), 0);
    let _ = d.send(Cmd::Live, 0);
    t.eq("seek: live", d.view_time(), now);
}

// review 2026-11-18: the complex PDU leaves from the device chosen in its dialog, also when the
// list of sources changes while the dialog is open (it held a line of the list, so a device that
// got its address by then made another one the source)
fn dialog_source(t: &mut T) {
    use crate::workspace::sim::pdu;
    let mut p = Project::new("pdu-dialog", 5);
    let pcs: Vec<u32> = (0..3).map(|i| p.add_device("pc", i * 150, 0).unwrap_or(0)).collect();
    let sw = p.add_device("jf-s24p", 150, 150).unwrap_or(0);
    for (i, &pc) in pcs.iter().enumerate() {
        let _ = p.connect(&pr(pc, "eth0"), &pr(sw, &alloc::format!("gi1/0/{}", i + 1)), None);
    }
    // PC2 and PC3 have addresses, PC1 none yet
    for (i, &pc) in pcs.iter().enumerate().skip(1) {
        if let Some(d) = p.device_mut(pc) {
            d.startup_config = alloc::format!("ip addr add 10.0.0.{}/24 dev eth0\n", i + 1);
        }
    }
    let (pc1, pc3) = (pcs[0], pcs[2]);
    let mut n = crate::app::Net::new();
    n.doc.replace(p, None);
    n.canvas.st.sel = [pc3 as u64].into_iter().collect();
    pdu::open_dialog(&mut n);
    t.eq("dialog: the selected device is the source", n.sim.pdu.f_src, Some(pc3));
    // PC1 gets its address while the dialog is open: it comes first in the list of sources
    let addr = move |dev: u32, a: &str, add: bool| Action::IfAddr { dev, iface: String::from("eth0"), prefix: Prefix4::parse(a).unwrap_or(Prefix4 { addr: Ipv4::UNSPECIFIED, len: 0 }), add };
    let a1 = addr(pc1, "10.0.0.1/24", true);
    t.check("dialog: PC1 gets an address", n.doc.sim.with_engine("test", move |e| e.apply(a1.clone()).is_ok()));
    n.sim.pdu.f_dst = String::from("10.0.0.2");
    pdu::submit(&mut n);
    t.eq("dialog: sent from the device chosen", n.sim.pdu.list.last().map(|x| x.spec.src), Some(pc3));
    // PC3 loses its address: the dialog says so and no other device sends instead
    let a3 = addr(pc3, "10.0.0.3/24", false);
    let _ = n.doc.sim.with_engine("test", move |e| e.apply(a3.clone()).is_ok());
    let before = n.sim.pdu.list.len();
    pdu::submit(&mut n);
    t.check("dialog: a source without an address is named", n.sim.pdu.list.len() == before && n.sim.pdu.f_err.contains("PC3"));
}

// review 2026-11-18: the links of the frames of a device's step in the Paket panel have ids of
// their own (from the 40th frame on they were the hex view's, from the 176th the PDUs panel's:
// a broadcast flooded by a 48-port switch)
fn frame_links(t: &mut T) {
    use crate::app::constants::{ID_PDU_ACT0, ID_PDU_SIMPLE, ID_PK_HEX, ID_PK_TREE, PK_LINKS};
    use crate::workspace::sim::packet::pdu_link_id;
    let ids: Vec<u32> = (0..512).filter_map(pdu_link_id).collect();
    let mut uniq = ids.clone();
    uniq.dedup();
    t.check("frame links: one id each", ids.len() == 512 && uniq.len() == 512);
    t.check("frame links: none of the simulation panels' ids", ids.iter().all(|&i| !(0x8000..=0x9FFF).contains(&i)) && [ID_PK_HEX, ID_PK_TREE, ID_PDU_SIMPLE, ID_PDU_ACT0].iter().all(|x| !ids.contains(x)));
    t.eq("frame links past the block: plain text", pdu_link_id(PK_LINKS), None);
}
