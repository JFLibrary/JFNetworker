// dbrepl -- replication of the database service (db.rs, SA5): a replica follows its primary over a TCP
// connection of the lab's network, writes the log it receives to its own disk and applies it serially on its
// own CPU; a commit on the primary waits for nobody, for replicas to receive, to flush or to apply its record;
// epochs, promotion, fencing and the base copy.  Our own frames, no real product's protocol.  A child module of
// db.rs (it works on the database's own state); without "replication" in the config nothing here runs.
//
//   config  "replication": {"role": "primary" | "replica", "primary": "10.0.2.30", "port": 7101,
//           "mode": "async" | "semi" | "sync" | "apply", "sync_count": 1, "sync_timeout_ms": 10000,
//           "apply_us": 2, "apply_row_us": 10, "heartbeat_ms": 500, "timeout_ms": 3000, "retry_ms": 1000,
//           "fencing": true, "fence_wait_ms": 2000, "failover_ms": 0}
//           every member may be left out; "primary" makes the role "replica"; "port" is the log's port (the
//           database's port + 1); a replica may leave out "tables" and gets them with a base copy
//
//   Db::promote(os, lost)       a replica becomes primary (dbctl promote, Action::DbPromote through lab.rs)
//   Db::repl_view(now)          what the window and dbctl show (ReplView, FollowerView)
//   Db::repl_problem(now)       a replica that does not reach its primary (problems.rs)
//   Db::answered_after(lsn)     commits the primary answered beyond a position: what a failover loses
//                               (counted by the engine: Engine::db_lost_at, an observation)
//   Engine::db_lost_at(addr, lsn), Engine::db_at(addr)   the same through the address of the old primary
//
// The wire (TCP, port "port" on every member): a frame is a kind byte, a length (u32) and its body.  A log
// record travels as many bytes as it takes in the log (row_bytes + 64 for a row, 64 for a commit or an abort;
// more only when its values are longer): the bytes on the wire follow the log, so bandwidth, delay, loss and
// partitions count as they do for every other byte.  HELLO (the replica's epoch and position), WELCOME (where
// the stream starts, what the primary waits for), RECORD, ACK (received, flushed, applied), BEAT (the
// primary's position when nothing else goes), MEMBERS (the replicas of the primary), STATE (a member's
// epoch, role and history: a promotion, the check of a returning primary, and its answer), NOT (not a primary:
// follow this address), COPY / BULK / ROW / END (a base copy).
//
// The primary ships what its disk has (the flushed log), never more: a crash of the primary cannot take back
// what a replica holds.  async answers after the local flush; semi after `sync_count` replicas received the
// record; sync after they flushed it; apply after they applied it (only that makes read-your-writes hold on a
// replica).  Locks go at the local flush, so a commit that waits for replicas does not hold its rows.  A
// waiting commit gives up after sync_timeout_ms: the primary falls back to async, says so in its status and
// log, answers what waited, and goes back once enough replicas have caught up (0: it waits for ever).
//
// A replica receives records, writes them to its log (group writes on its disk) and applies what its disk has,
// one job after the other on its CPU: apply_us per record and apply_row_us per row record, a page of the row
// through its own pool (a miss reads its disk), at most 64 records or res::QUANTUM of CPU per job.  The rows of
// a transaction show at its commit record, together; an abort record drops them.  It answers reads from what it
// applied, refuses writes ("read_only", 503, with the primary's address) and has its own pool, cold at first.
//
// Lag.  lag_bytes: the primary's position as last heard minus what the replica applied; lag_ms: now minus the
// commit time (on the primary) of the oldest commit not applied yet, 0 when there is none; lag_p50_us /
// lag_p99_us: from a commit on the primary to its apply here, per commit.  The primary's figure per replica
// comes from the acknowledgements: its own flushed position against what the replica applied.
//
// Stale reads.  With replication a commit's answer names its position ("lsn").  A read may carry "min_lsn"
// (and "wait_ms"): a replica that applied less counts a stale read and answers at once with "stale": true, or
// with wait_ms > 0 waits that long for its apply first (read_waits) and only then answers stale.
//
// The base copy.  A replica without a matching copy (no or other tables, a position the primary's log tail no
// longer holds, a log that went another way after a promotion) gets one: the touched rows travel as real bytes
// (a log record's size each), the rest of the data (heap and indexes, `bulk`) is modelled: its first 4 MiB
// travel for real and measure the path, the rest takes the time that rate gives, without loading the link,
// and the replica's disk writes all of it.  The primary reads the bulk from its disk.  The replica answers no
// read until it applied the log up to where the copy ended (consistent).
//
// Epochs.  Every primary has an epoch and a history of where each epoch began in the log.  A promotion (only a
// replica, after it flushed and applied what it received) takes the highest epoch it has seen plus one, logs
// an abort for every transaction left open and tells every member it knows (its old primary and the replicas
// of the member list) again and again until each answered.  A replica follows the member with the higher
// epoch.  A primary that learns of a higher epoch stops taking writes when "fencing" is on (it becomes a replica
// of the new primary, and its log, which went another way, takes a base copy); without fencing both take
// writes, and each counts what it took since the fork when they meet (split_commits).  A primary that starts
// again after a crash asks its members first (fence_wait_ms) and refuses writes until then.  failover_ms > 0
// on a replica: it promotes itself after so long without a word from its primary (set it on one replica; a
// partition then shows split brain, which fencing ends when the sides meet again; there is no witness).
//
// What is not replicated: CREATE INDEX and DROP INDEX, the pool's contents.  A transaction longer than the
// log tail cannot be rebuilt after a replica's crash (its rows are lost there).

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use agent_core::jmap::Value;
use base::metrics::Window;
use jfn_model::msg;
use jfn_model::msg::Msg;
use netpkt::{IpAddr, Ipv4};

use super::{Db, DTimer, WalKind, WalRec, REC_HEAD};
use crate::dbsql::Stmt;
use crate::dbstore::{cost_of, walk, Access, Pool, Table, TableSpec, Val};
use crate::http::ReqIn;
use crate::httpsvc::decimal;
use crate::kernel::{Engine, Ns, MS, SEC, US};
use crate::os::{Metric, OsApi, SvcEvent};
use crate::tcp::SockOpts;
use crate::trace::Note;

/// the high bit of a transaction applied from the log: never the id of a reader here
const APPLIED: u64 = 1 << 62;
/// the transaction of a base copy's rows
const BASE_TXN: u64 = APPLIED | (1 << 61);
/// a frame's head: its kind and the length of its body
const HEAD: usize = 5;
const FRAME_MAX: usize = 8 << 20;
/// the bytes of a base copy's bulk that travel for real and measure the path
const PROBE: u64 = 4 << 20;
const BULK_FRAME: u64 = 64 << 10;
/// what a connection keeps ready for TCP
const OUT_LOW: usize = 64 << 10;
/// answered commits kept for counting what a failover loses
const ANSWERED_MAX: usize = 1 << 16;
/// a replica says how far it is at least this often while something changes
const STATUS_EVERY: Ns = 100 * MS;
/// one apply job: at most so many records
const APPLY_BATCH: usize = 64;
/// a replica without its primary for so long is a problem (problems.rs)
const LOST_PROBLEM: Ns = 5 * SEC;

const F_HELLO: u8 = b'H';
const F_WELCOME: u8 = b'W';
const F_REC: u8 = b'R';
const F_ACK: u8 = b'A';
const F_BEAT: u8 = b'K';
const F_MEMBERS: u8 = b'M';
const F_STATE: u8 = b'P';
const F_NOT: u8 = b'N';
const F_COPY: u8 = b'S';
const F_BULK: u8 = b'Z';
const F_ROW: u8 = b'T';
const F_END: u8 = b'E';

/// what a member is now
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    Primary,
    Replica,
}

/// what a commit on the primary waits for
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Async,
    Semi,
    Sync,
    Apply,
}

impl Mode {
    pub fn name(self) -> &'static str {
        match self {
            Mode::Async => "async",
            Mode::Semi => "semi",
            Mode::Sync => "sync",
            Mode::Apply => "apply",
        }
    }
    fn parse(s: &str) -> Option<Mode> {
        Some(match s {
            "async" => Mode::Async,
            "semi" => Mode::Semi,
            "sync" => Mode::Sync,
            "apply" => Mode::Apply,
            _ => return None,
        })
    }
    /// 0 nobody, 1 received, 2 flushed, 3 applied
    fn level(self) -> u8 {
        match self {
            Mode::Async => 0,
            Mode::Semi => 1,
            Mode::Sync => 2,
            Mode::Apply => 3,
        }
    }
}

/// its settings
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplCfg {
    pub role: Role,
    pub primary: Option<Ipv4>,
    /// the log's port on every member
    pub port: u16,
    pub mode: Mode,
    pub sync_count: u32,
    /// 0: a commit waits for ever
    pub sync_timeout: Ns,
    pub apply_cpu: Ns,
    pub apply_row_cpu: Ns,
    pub heartbeat: Ns,
    pub timeout: Ns,
    pub retry: Ns,
    pub fencing: bool,
    pub fence_wait: Ns,
    /// 0: no automatic failover
    pub failover: Ns,
}

impl ReplCfg {
    /// from the config's "replication" (the database's port for the default of its own)
    pub fn of(c: &Value, db_port: u16) -> Result<ReplCfg, Msg> {
        if !matches!(c, Value::Object(_)) {
            return Err(msg!("\u{201e}replication\u{201c} ist ein Objekt."));
        }
        let ms = |k: &str, d: u64| c.get(k).and_then(|v| decimal(v, MS)).unwrap_or(d * MS);
        let us = |k: &str, d: Ns| c.get(k).and_then(|v| decimal(v, US)).unwrap_or(d);
        let primary = match c.get("primary").and_then(|v| v.as_str()) {
            Some(a) => Some(Ipv4::parse(a.trim()).ok_or_else(|| msg!("\u{201e}replication.primary\u{201c}: \u{201e}{}\u{201c} ist keine IPv4-Adresse.", a))?),
            None => None,
        };
        let role = match c.get("role").and_then(|v| v.as_str()) {
            Some("primary") => Role::Primary,
            Some("replica") => Role::Replica,
            None if primary.is_some() => Role::Replica,
            None => Role::Primary,
            Some(x) => return Err(msg!("\u{201e}replication.role\u{201c} kennt {} nicht (primary, replica).", x)),
        };
        if role == Role::Replica && primary.is_none() {
            return Err(msg!("Ein Replikat braucht \u{201e}replication.primary\u{201c}: die Adresse seines Primary."));
        }
        let mode = match c.get("mode").and_then(|v| v.as_str()) {
            Some(m) => Mode::parse(m).ok_or_else(|| msg!("\u{201e}replication.mode\u{201c} kennt {} nicht (async, semi, sync, apply).", m))?,
            None => Mode::Async,
        };
        let port = match c.get("port").and_then(|v| v.as_u64()) {
            Some(p @ 1..=65535) => p as u16,
            Some(p) => return Err(msg!("Port {} gibt es nicht.", p)),
            None => db_port.wrapping_add(1).max(1),
        };
        if port == db_port {
            return Err(msg!("Das Log braucht einen eigenen Port, nicht {}.", port));
        }
        let heartbeat = ms("heartbeat_ms", 500).max(10 * MS);
        Ok(ReplCfg {
            role,
            primary,
            port,
            mode,
            sync_count: c.get("sync_count").and_then(|v| v.as_u64()).unwrap_or(1).clamp(1, 16) as u32,
            sync_timeout: ms("sync_timeout_ms", 10_000),
            apply_cpu: us("apply_us", 2 * US),
            apply_row_cpu: us("apply_row_us", 10 * US),
            heartbeat,
            timeout: ms("timeout_ms", 3000).max(heartbeat * 2),
            retry: ms("retry_ms", 1000).max(10 * MS),
            fencing: c.get("fencing").and_then(|v| v.as_bool()).unwrap_or(true),
            fence_wait: ms("fence_wait_ms", 2000),
            failover: ms("failover_ms", 0),
        })
    }
    /// what a running database can take without a restart: all but role, primary and port
    pub fn live(&self, other: &ReplCfg) -> bool {
        self.role == other.role && self.primary == other.primary && self.port == other.port
    }
    /// its settings in the units of its config (svcform.rs)
    pub fn value(&self) -> Value {
        use crate::svcform::{ms_of, us_of, word};
        let mut o = agent_core::jmap::empty_object();
        o.insert("role", word(if self.role == Role::Replica { "replica" } else { "primary" }));
        if let Some(p) = self.primary {
            o.insert("primary", Value::string(format!("{}", p)));
        }
        o.insert("port", Value::u64(self.port as u64));
        o.insert("mode", word(self.mode.name()));
        o.insert("sync_count", Value::u64(self.sync_count as u64));
        o.insert("sync_timeout_ms", ms_of(self.sync_timeout));
        o.insert("apply_us", us_of(self.apply_cpu));
        o.insert("apply_row_us", us_of(self.apply_row_cpu));
        o.insert("heartbeat_ms", ms_of(self.heartbeat));
        o.insert("timeout_ms", ms_of(self.timeout));
        o.insert("retry_ms", ms_of(self.retry));
        o.insert("fencing", Value::Bool(self.fencing));
        o.insert("fence_wait_ms", ms_of(self.fence_wait));
        o.insert("failover_ms", ms_of(self.failover));
        o
    }
}

/// its timers (DTimer::Repl)
#[derive(Clone, Copy, Debug)]
pub enum RT {
    /// the next try to reach the primary
    Retry,
    /// the apply job up to this position is done
    Applied(u64),
    /// heartbeats out, silence in, the automatic failover
    Beat,
    /// a replica's status is due
    Status,
    /// a commit waited this long for its replicas (its position)
    SyncOut(u64),
    /// the modelled part of a base copy is over
    CopyDone,
    /// a returning primary asked its members long enough
    FenceDone,
    /// a new primary tells its members again
    Announce,
}

/// what a connection is for
#[derive(Clone, Debug)]
enum Kind {
    /// came in, has not said what it wants yet
    New,
    /// a replica this primary streams to
    Follower(Follower),
    /// this replica's connection to its primary
    Up { welcomed: bool },
    /// this member tells another its state and waits for the answer
    Tell,
}

#[derive(Clone, Debug)]
struct Conn {
    kind: Kind,
    peer: Ipv4,
    inbuf: Vec<u8>,
    out: Vec<u8>,
    /// it closes once `out` is sent
    closing: bool,
    heard: Ns,
    sent_at: Ns,
}

impl Conn {
    fn new(kind: Kind, peer: Ipv4, now: Ns) -> Conn {
        Conn { kind, peer, inbuf: Vec::new(), out: Vec::new(), closing: false, heard: now, sent_at: now }
    }
}

/// a replica as its primary sees it
#[derive(Clone, Debug, Default)]
struct Follower {
    /// the last record handed to the connection
    sent: u64,
    recv: u64,
    flushed: u64,
    applied: u64,
    copy: Option<CopySend>,
}

/// a base copy on its way out
#[derive(Clone, Debug)]
struct CopySend {
    started: bool,
    probe_left: u64,
    table: usize,
    key: u64,
    snap: u64,
}

/// a base copy coming in
#[derive(Clone, Debug, Default)]
struct CopyRecv {
    bulk: u64,
    probe: u64,
    got: u64,
    snap: u64,
    t_half: Option<Ns>,
    t_end: Option<Ns>,
    /// the modelled rest ends then (None: END has not come)
    done_at: Option<Ns>,
    /// the rows are consistent once the log is applied up to here (after END)
    consistent_at: Option<u64>,
}

/// a commit's answer waiting for replicas
#[derive(Clone, Debug)]
struct SyncWait {
    lsn: u64,
    sid: u64,
    body: String,
    start: Ns,
    timer: Option<u64>,
}

/// a read waiting for the apply to reach a position
#[derive(Clone, Debug)]
struct Parked {
    req: ReqIn,
    min: u64,
    until: Ns,
}

/// what replication counted
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReplStats {
    pub records_out: u64,
    pub bytes_out: u64,
    pub records_in: u64,
    pub bytes_in: u64,
    pub applied_records: u64,
    pub applied_commits: u64,
    pub stale_reads: u64,
    pub read_waits: u64,
    pub sync_waits: u64,
    pub sync_fallbacks: u64,
    pub copies: u64,
    pub copy_bytes: u64,
    pub promotions: u64,
    pub failover_lost: u64,
    /// split brain: commits this primary took since the fork that the other one does not have, and the other's
    pub split_commits: u64,
    pub rival_commits: u64,
    /// commits a fenced primary had taken since the fork: they are gone
    pub fenced_lost: u64,
    pub refused_writes: u64,
}

/// a member's state as STATE carries it
#[derive(Clone, Debug, Default)]
struct State {
    epoch: u64,
    primary: bool,
    lsn: u64,
    /// who it follows (a replica)
    follows: Option<Ipv4>,
    /// the commits it took since the receiver's fork (an answer)
    commits: u64,
    history: Vec<(u64, u64)>,
}

/// the replication of a database
#[derive(Clone, Debug)]
pub struct Repl {
    pub cfg: ReplCfg,
    pub role: Role,
    pub epoch: u64,
    /// where each epoch began in the log: (epoch, its first position)
    pub history: Vec<(u64, u64)>,
    /// a primary that saw a higher epoch with fencing on (a replica now)
    pub fenced: bool,
    /// a returning primary asks its members until then
    checking: Option<Ns>,
    listener: Option<u32>,
    conns: BTreeMap<u32, Conn>,
    /// the members it knows: a primary's replicas, a replica's fellows (from MEMBERS)
    pub members: Vec<Ipv4>,
    /// the last position each member said it received (counting what a failover loses)
    member_recv: BTreeMap<u32, u64>,
    /// a replica: whom it follows now (its `epoch` is that of its log: the primary's it streams from)
    pub up_addr: Option<Ipv4>,
    upstream: Option<u32>,
    /// since when it has no primary (problems.rs)
    lost_since: Option<Ns>,
    retry_armed: bool,
    beat_armed: bool,
    status_armed: bool,
    /// a replica: what it applied, the primary's position as last heard
    pub applied: u64,
    known: u64,
    /// rows of transactions whose commit has not come yet
    pending: BTreeMap<u64, Vec<WalRec>>,
    /// the apply job on the CPU: up to this position
    applying: Option<u64>,
    /// what the primary waits for (from WELCOME)
    level: u8,
    acked: (u64, u64, u64),
    acked_at: Ns,
    /// the primary's commit time of the oldest commit not applied yet; it said it fell behind
    behind_since: Option<Ns>,
    behind: bool,
    copy: Option<CopyRecv>,
    /// it asks for a base copy (none yet, or one broke off)
    need_copy: bool,
    /// the copy's tables and seed (a replica takes the primary's)
    adopted: Option<Value>,
    adopted_seed: Option<u64>,
    /// a primary: commits it answered, by position, that some member may not have
    answered: VecDeque<u64>,
    answered_dropped: u64,
    /// commits answered in this epoch
    epoch_commits: u64,
    sync_wait: VecDeque<SyncWait>,
    pub fallback: bool,
    /// a new primary still owes these its announcement; the old primary
    owed: Vec<Ipv4>,
    announce_armed: bool,
    old_primary: Option<Ipv4>,
    pub promoting: bool,
    /// split brain: the other primary
    pub rival: Option<Ipv4>,
    /// the highest transaction id seen in the log (a promoted replica's ids start above it)
    max_txn: u64,
    max_epoch_seen: u64,
    pub stats: ReplStats,
    lag: Window,
    parked: Vec<Parked>,
}

impl Repl {
    pub fn new(cfg: ReplCfg) -> Repl {
        Repl {
            role: cfg.role,
            up_addr: cfg.primary,
            need_copy: false,
            epoch: 1,
            history: alloc::vec![(1, 0)],
            fenced: false,
            checking: None,
            listener: None,
            conns: BTreeMap::new(),
            members: Vec::new(),
            member_recv: BTreeMap::new(),
            upstream: None,
            lost_since: None,
            retry_armed: false,
            beat_armed: false,
            status_armed: false,
            applied: 0,
            known: 0,
            pending: BTreeMap::new(),
            applying: None,
            level: 0,
            acked: (0, 0, 0),
            acked_at: 0,
            behind_since: None,
            behind: false,
            copy: None,
            adopted: None,
            adopted_seed: None,
            answered: VecDeque::new(),
            answered_dropped: 0,
            epoch_commits: 0,
            sync_wait: VecDeque::new(),
            fallback: false,
            owed: Vec::new(),
            announce_armed: false,
            old_primary: None,
            promoting: false,
            rival: None,
            max_txn: 0,
            max_epoch_seen: 1,
            stats: ReplStats::default(),
            lag: Window::new(300, SEC),
            parked: Vec::new(),
            cfg,
        }
    }
    /// a replica without tables of its own: it asks its primary for a copy
    pub fn need_copy_at_start(&mut self) {
        self.need_copy = true;
    }
    /// the event is one of its sockets'
    pub fn owns(&self, ev: &SvcEvent) -> bool {
        match *ev {
            SvcEvent::TcpAccept { listener, .. } => Some(listener) == self.listener,
            SvcEvent::TcpConnected { sock, .. } | SvcEvent::TcpData { sock, .. } | SvcEvent::TcpWritable { sock } | SvcEvent::TcpEof { sock, .. } | SvcEvent::TcpClosed { sock, .. } => self.conns.contains_key(&sock),
            _ => false,
        }
    }
    fn followers(&self) -> impl Iterator<Item = (&u32, &Conn, &Follower)> {
        self.conns.iter().filter_map(|(s, c)| match &c.kind {
            Kind::Follower(f) => Some((s, c, f)),
            _ => None,
        })
    }
    fn welcomed(&self) -> bool {
        self.upstream.and_then(|s| self.conns.get(&s)).is_some_and(|c| matches!(c.kind, Kind::Up { welcomed: true }))
    }
    pub fn footprint(&self) -> usize {
        512 + self.conns.values().map(|c| 128 + c.inbuf.capacity() + c.out.capacity()).sum::<usize>() + self.pending.values().map(|v| 64 + v.len() * 96).sum::<usize>() + self.answered.len() * 8 + self.sync_wait.iter().map(|w| 64 + w.body.len()).sum::<usize>() + self.lag.bytes() + self.parked.iter().map(|p| 256 + p.req.body.len()).sum::<usize>()
    }
}

/// a replica as the window shows it (from its primary)
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FollowerView {
    pub addr: Ipv4,
    pub received: u64,
    pub flushed: u64,
    pub applied: u64,
    pub lag_bytes: u64,
    pub lag_ms: u64,
    pub copying: bool,
}

/// replication as the window and dbctl show it
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplView {
    pub role: Role,
    pub epoch: u64,
    pub mode: Mode,
    pub sync_count: u32,
    pub port: u16,
    pub fallback: bool,
    pub fenced: bool,
    pub promoting: bool,
    pub checking: bool,
    /// a replica: its primary, connected, copying (percent of the bulk), consistent
    pub primary: Option<Ipv4>,
    pub connected: bool,
    pub copying: Option<u64>,
    pub applied: u64,
    pub lag_ms: u64,
    pub lag_bytes: u64,
    pub lag_p50: Option<Ns>,
    pub lag_p99: Option<Ns>,
    pub followers: Vec<FollowerView>,
    pub members: Vec<Ipv4>,
    pub rival: Option<Ipv4>,
    pub history: Vec<(u64, u64)>,
    pub stats: ReplStats,
}

// ---------------------------------------------------------------- the wire

struct Wr(Vec<u8>);

impl Wr {
    fn new(kind: u8) -> Wr {
        let mut v = Vec::with_capacity(64);
        v.push(kind);
        v.extend_from_slice(&[0; 4]);
        Wr(v)
    }
    fn u8(&mut self, x: u8) -> &mut Wr {
        self.0.push(x);
        self
    }
    fn u16(&mut self, x: u16) -> &mut Wr {
        self.0.extend_from_slice(&x.to_be_bytes());
        self
    }
    fn u32(&mut self, x: u32) -> &mut Wr {
        self.0.extend_from_slice(&x.to_be_bytes());
        self
    }
    fn u64(&mut self, x: u64) -> &mut Wr {
        self.0.extend_from_slice(&x.to_be_bytes());
        self
    }
    fn bytes(&mut self, b: &[u8]) -> &mut Wr {
        self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self
    }
    fn val(&mut self, v: &Val) -> &mut Wr {
        match v {
            Val::Int(n) => {
                self.u8(0);
                self.u64(*n as u64);
            }
            Val::Text(t) => {
                let b = &t.as_bytes()[..t.len().min(u16::MAX as usize)];
                self.u8(1);
                self.u16(b.len() as u16);
                self.0.extend_from_slice(b);
            }
        }
        self
    }
    /// the frame, at least `size` bytes long with its head (the padding is zeros)
    fn done(mut self, size: u64) -> Vec<u8> {
        let want = (size as usize).min(FRAME_MAX);
        if self.0.len() < want {
            self.0.resize(want, 0);
        }
        let n = (self.0.len() - HEAD) as u32;
        self.0[1..HEAD].copy_from_slice(&n.to_be_bytes());
        self.0
    }
}

struct Rd<'a> {
    b: &'a [u8],
    i: usize,
}

impl Rd<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let s = self.b.get(self.i..self.i.checked_add(n)?)?;
        self.i += n;
        Some(s)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|s| s[0])
    }
    fn u16(&mut self) -> Option<u16> {
        self.take(2).map(|s| u16::from_be_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        self.take(4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn u64(&mut self) -> Option<u64> {
        let s = self.take(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(s);
        Some(u64::from_be_bytes(a))
    }
    fn bytes(&mut self) -> Option<&[u8]> {
        let n = self.u32()? as usize;
        self.take(n)
    }
    fn val(&mut self) -> Option<Val> {
        match self.u8()? {
            0 => Some(Val::Int(self.u64()? as i64)),
            _ => {
                let n = self.u16()? as usize;
                Some(Val::Text(String::from_utf8_lossy(self.take(n)?).into_owned()))
            }
        }
    }
}

/// the complete frames at the front of a buffer, taken out of it
fn frames(buf: &mut Vec<u8>) -> Vec<(u8, Vec<u8>)> {
    let mut out = Vec::new();
    let mut at = 0;
    while buf.len() >= at + HEAD {
        let n = u32::from_be_bytes([buf[at + 1], buf[at + 2], buf[at + 3], buf[at + 4]]) as usize;
        if n > FRAME_MAX || buf.len() < at + HEAD + n {
            break;
        }
        out.push((buf[at], buf[at + HEAD..at + HEAD + n].to_vec()));
        at += HEAD + n;
    }
    buf.drain(..at);
    out
}

fn rec_frame(r: &WalRec, size: u64) -> Vec<u8> {
    let mut w = Wr::new(F_REC);
    w.u64(r.lsn).u64(r.txn).u64(r.at);
    match &r.kind {
        WalKind::Row { table, key, vals, gone } => {
            w.u8(0).u16(*table).u64(*key).u8(*gone as u8).u16(vals.len() as u16);
            for (c, v) in vals {
                w.u16(*c).val(v);
            }
        }
        WalKind::Commit => {
            w.u8(1);
        }
        WalKind::Abort => {
            w.u8(2);
        }
    }
    w.done(size)
}

fn rec_of(b: &[u8]) -> Option<WalRec> {
    let mut r = Rd { b, i: 0 };
    let (lsn, txn, at) = (r.u64()?, r.u64()?, r.u64()?);
    let kind = match r.u8()? {
        0 => {
            let (table, key, gone, n) = (r.u16()?, r.u64()?, r.u8()? != 0, r.u16()?);
            let mut vals = Vec::with_capacity(n as usize);
            for _ in 0..n {
                vals.push((r.u16()?, r.val()?));
            }
            WalKind::Row { table, key, vals, gone }
        }
        1 => WalKind::Commit,
        _ => WalKind::Abort,
    };
    Some(WalRec { lsn, txn, at, kind })
}

fn state_frame(kind: u8, s: &State) -> Vec<u8> {
    let mut w = Wr::new(kind);
    w.u64(s.epoch).u8(s.primary as u8).u64(s.lsn).u32(s.follows.map_or(0, |a| a.to_u32())).u64(s.commits).u8(s.history.len().min(32) as u8);
    for (e, at) in s.history.iter().rev().take(32).rev() {
        w.u64(*e).u64(*at);
    }
    w.done(0)
}

fn state_of(b: &[u8]) -> Option<State> {
    let mut r = Rd { b, i: 0 };
    let mut s = State { epoch: r.u64()?, primary: r.u8()? != 0, lsn: r.u64()?, ..State::default() };
    let f = r.u32()?;
    s.follows = (f != 0).then(|| Ipv4::from_u32(f));
    s.commits = r.u64()?;
    for _ in 0..r.u8()? {
        s.history.push((r.u64()?, r.u64()?));
    }
    Some(s)
}

/// a statement that would change something
fn writes(s: &Stmt) -> bool {
    match s {
        Stmt::Insert { .. } | Stmt::Update { .. } | Stmt::Delete { .. } | Stmt::CreateIndex { .. } | Stmt::DropIndex { .. } => true,
        Stmt::Select(x) => x.for_update,
        Stmt::Explain(_) | Stmt::Begin(_) | Stmt::Commit | Stmt::Rollback => false,
    }
}

/// a commit's answer with its position
fn with_lsn(body: String, lsn: u64) -> String {
    match body.rfind('}') {
        Some(i) => format!("{},\"lsn\":{}{}", &body[..i], lsn, &body[i..]),
        None => body,
    }
}

/// where my log left the other's: the start of the first epoch above mine in its history
fn fork_of(history: &[(u64, u64)], my_epoch: u64) -> Option<u64> {
    history.iter().find(|(e, _)| *e > my_epoch).map(|(_, at)| *at)
}

// ---------------------------------------------------------------- the database's side

impl Db {
    /// the log's port of this database (replication on)
    fn rport(&self) -> u16 {
        self.repl.as_ref().map_or(0, |r| r.cfg.port)
    }
    fn opts() -> SockOpts {
        SockOpts { nodelay: true, ..SockOpts::default() }
    }
    /// what tells two copies of the tables apart: their definitions with the offsets the seed gave
    fn spec_hash(&self) -> u64 {
        let mut s = String::new();
        for t in &self.tables {
            s.push_str(&format!("{:?}", t.spec));
        }
        base::hashring::hash64(s.as_bytes())
    }
    fn note_on(os: &mut OsApi, n: Note) {
        if os.recording() {
            os.note(n);
        }
    }

    /// starts with the service: the log's port, a replica connects, a returning primary asks first
    pub(super) fn repl_start(&mut self, os: &mut OsApi, restart: bool) {
        let port = self.rport();
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        match os.tcp_listen(port, 64, Db::opts()) {
            Ok(l) => r.listener = Some(l),
            Err(e) => os.log(3, "db", &format!("Replikation: Port {} geht nicht: {}", port, e)),
        }
        r.lost_since = Some(now);
        self.repl_rebuild();
        let Some(r) = self.repl.as_mut() else { return };
        match r.role {
            Role::Replica => self.repl_connect(os),
            Role::Primary => {
                if restart && r.cfg.fencing && !r.members.is_empty() {
                    r.checking = Some(now + r.cfg.fence_wait);
                    r.owed = r.members.clone();
                    let at = now + r.cfg.fence_wait;
                    self.wheel.add(at, DTimer::Repl(RT::FenceDone));
                    os.log(5, "db", "Replikation: fragt die Mitglieder nach einer neueren Epoche, bevor sie schreibt");
                    self.repl_tell_owed(os);
                }
                self.repl_close_orphans(os);
            }
        }
        self.repl_beat_arm(os);
    }

    /// stops with the service (a kill): connections and what waited go; what the log has stays
    pub(super) fn repl_stop(&mut self) {
        let Some(r) = self.repl.as_mut() else { return };
        r.listener = None;
        r.conns.clear();
        r.upstream = None;
        r.retry_armed = false;
        r.beat_armed = false;
        r.status_armed = false;
        r.announce_armed = false;
        r.applying = None;
        r.pending.clear();
        r.sync_wait.clear();
        r.parked.clear();
        r.checking = None;
        r.promoting = false;
        if r.copy.take().is_some() {
            r.need_copy = true;
        }
        // what it applied is what its disk keeps
        r.applied = r.applied.min(self.flushed);
    }

    /// after a start: the rows of transactions still open in the log it applied
    fn repl_rebuild(&mut self) {
        let Some(r) = self.repl.as_mut() else { return };
        r.pending.clear();
        if r.role != Role::Replica {
            return;
        }
        for rec in self.wal_tail.iter().filter(|x| x.lsn <= r.applied) {
            match rec.kind {
                WalKind::Row { .. } => r.pending.entry(rec.txn).or_default().push(rec.clone()),
                WalKind::Commit | WalKind::Abort => {
                    r.pending.remove(&rec.txn);
                }
            }
        }
    }

    /// a primary that starts again: transactions a crash left open get their abort in the log (their rows may
    /// have reached a replica)
    fn repl_close_orphans(&mut self, os: &mut OsApi) {
        let mut open: BTreeMap<u64, ()> = BTreeMap::new();
        for rec in &self.wal_tail {
            match rec.kind {
                WalKind::Row { .. } => {
                    open.insert(rec.txn, ());
                }
                _ => {
                    open.remove(&rec.txn);
                }
            }
        }
        if open.is_empty() {
            return;
        }
        for txn in open.keys() {
            self.lsn += REC_HEAD;
            let lsn = self.lsn;
            self.stats.wal_bytes += REC_HEAD;
            self.wal_rec(WalRec { lsn, txn: *txn, at: 0, kind: WalKind::Abort });
        }
        if self.flushing.is_none() {
            self.start_flush(os);
        }
    }

    /// a transaction that wrote ends badly: its abort goes into the log (replication only)
    pub(super) fn repl_log_abort(&mut self, txn: u64) {
        if self.repl.as_ref().is_none_or(|r| r.role != Role::Primary || r.fenced) {
            return;
        }
        self.lsn += REC_HEAD;
        let lsn = self.lsn;
        self.stats.wal_bytes += REC_HEAD;
        self.wal_rec(WalRec { lsn, txn, at: 0, kind: WalKind::Abort });
    }

    fn repl_beat_arm(&mut self, os: &mut OsApi) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        if !r.beat_armed {
            r.beat_armed = true;
            let at = now + r.cfg.heartbeat;
            self.wheel.add(at, DTimer::Repl(RT::Beat));
        }
    }

    // ------------------------------------------------ connections

    fn repl_connect(&mut self, os: &mut OsApi) {
        let port = self.rport();
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        if r.role != Role::Replica || r.upstream.is_some() || r.promoting {
            return;
        }
        let Some(addr) = r.up_addr else { return };
        match os.tcp_connect(IpAddr::V4(addr), port, Db::opts()) {
            Ok(s) => {
                r.upstream = Some(s);
                r.conns.insert(s, Conn::new(Kind::Up { welcomed: false }, addr, now));
            }
            Err(_) => self.repl_retry(os),
        }
    }

    fn repl_retry(&mut self, os: &mut OsApi) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        if !r.retry_armed {
            r.retry_armed = true;
            let at = now + r.cfg.retry;
            self.wheel.add(at, DTimer::Repl(RT::Retry));
        }
    }

    /// hands what a connection has ready to TCP; closes it when it is to close and all went
    fn repl_flush_out(&mut self, os: &mut OsApi, sock: u32) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        let Some(c) = r.conns.get_mut(&sock) else { return };
        if !c.out.is_empty() {
            let n = os.tcp_send(sock, &c.out);
            if n > 0 {
                c.out.drain(..n);
                c.sent_at = now;
                r.stats.bytes_out += n as u64;
            }
        }
        if c.closing && c.out.is_empty() {
            r.conns.remove(&sock);
            os.tcp_close(sock);
        }
    }

    fn repl_send(&mut self, os: &mut OsApi, sock: u32, frame: Vec<u8>) {
        if let Some(c) = self.repl.as_mut().and_then(|r| r.conns.get_mut(&sock)) {
            c.out.extend_from_slice(&frame);
        }
        self.repl_flush_out(os, sock);
    }

    fn repl_close(&mut self, os: &mut OsApi, sock: u32) {
        if let Some(c) = self.repl.as_mut().and_then(|r| r.conns.get_mut(&sock)) {
            c.closing = true;
        }
        self.repl_flush_out(os, sock);
    }

    /// a connection is over: what it was for
    fn repl_gone(&mut self, os: &mut OsApi, sock: u32) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        let Some(c) = r.conns.remove(&sock) else { return };
        match c.kind {
            Kind::Up { .. } => {
                r.upstream = None;
                if r.lost_since.is_none() {
                    r.lost_since = Some(now);
                }
                if r.role == Role::Replica && !r.promoting {
                    os.log(4, "db", &format!("Replikation: die Verbindung zum Primary {} ist weg", c.peer));
                    self.repl_retry(os);
                }
            }
            Kind::Follower(_) => {
                Db::note_on(os, Note::DbReplica { peer: c.peer, up: false });
            }
            _ => {}
        }
    }

    /// an event of one of its sockets
    pub(super) fn repl_on(&mut self, ev: SvcEvent, os: &mut OsApi) {
        let now = os.now();
        match ev {
            SvcEvent::TcpAccept { listener, .. } => {
                while let Some(s) = os.tcp_accept(listener) {
                    let peer = match os.tcp_info(s).map(|t| t.remote.0) {
                        Some(IpAddr::V4(a)) => a,
                        _ => Ipv4::UNSPECIFIED,
                    };
                    if let Some(r) = self.repl.as_mut() {
                        r.conns.insert(s, Conn::new(Kind::New, peer, now));
                    }
                }
            }
            SvcEvent::TcpConnected { sock, .. } => {
                let Some(r) = self.repl.as_ref() else { return };
                let Some(c) = r.conns.get(&sock) else { return };
                match c.kind {
                    Kind::Up { .. } => {
                        let flags = (r.need_copy || self.tables.is_empty()) as u8;
                        let mut w = Wr::new(F_HELLO);
                        w.u64(r.epoch).u64(self.lsn).u64(self.spec_hash()).u8(flags);
                        self.repl_send(os, sock, w.done(0));
                    }
                    Kind::Tell => {
                        let s = self.repl_state(0);
                        self.repl_send(os, sock, state_frame(F_STATE, &s));
                    }
                    _ => {}
                }
            }
            SvcEvent::TcpData { sock, .. } | SvcEvent::TcpEof { sock, .. } => {
                let data = os.tcp_recv(sock, usize::MAX);
                let eof = matches!(ev, SvcEvent::TcpEof { .. });
                let list = {
                    let Some(r) = self.repl.as_mut() else { return };
                    let Some(c) = r.conns.get_mut(&sock) else { return };
                    c.inbuf.extend_from_slice(&data);
                    c.heard = now;
                    if matches!(c.kind, Kind::Up { .. }) {
                        r.lost_since = None;
                    }
                    frames(&mut c.inbuf)
                };
                let mut got = false;
                for (k, body) in list {
                    got |= self.repl_frame(os, sock, k, &body);
                }
                if got {
                    self.repl_received(os);
                }
                if eof {
                    self.repl_close(os, sock);
                }
            }
            SvcEvent::TcpWritable { sock } => {
                let follower = self.repl.as_ref().and_then(|r| r.conns.get(&sock)).is_some_and(|c| matches!(c.kind, Kind::Follower(_)));
                if follower {
                    self.repl_fill(os, sock);
                } else {
                    self.repl_flush_out(os, sock);
                }
            }
            SvcEvent::TcpClosed { sock, .. } => {
                self.repl_gone(os, sock);
                os.tcp_close(sock);
            }
            _ => {}
        }
    }

    /// one frame; true when it brought log records (a replica)
    fn repl_frame(&mut self, os: &mut OsApi, sock: u32, k: u8, b: &[u8]) -> bool {
        let kind = match self.repl.as_ref().and_then(|r| r.conns.get(&sock)) {
            Some(c) => c.kind.clone(),
            None => return false,
        };
        let peer = self.repl.as_ref().and_then(|r| r.conns.get(&sock)).map_or(Ipv4::UNSPECIFIED, |c| c.peer);
        let mut r = Rd { b, i: 0 };
        match (kind, k) {
            (Kind::New, F_HELLO) => {
                let (Some(epoch), Some(pos), Some(hash), Some(flags)) = (r.u64(), r.u64(), r.u64(), r.u8()) else { return false };
                self.repl_hello(os, sock, peer, epoch, pos, hash, flags);
            }
            (Kind::New, F_STATE) => {
                let Some(s) = state_of(b) else { return false };
                let fork = fork_of(&s.history, self.repl.as_ref().map_or(0, |x| x.epoch));
                self.repl_state_in(os, peer, &s);
                let mine = self.repl_state(fork.map_or(0, |f| self.repl_commits_after(f)));
                self.repl_send(os, sock, state_frame(F_STATE, &mine));
                self.repl_close(os, sock);
            }
            (Kind::Tell, F_STATE) => {
                if let Some(s) = state_of(b) {
                    self.repl_state_in(os, peer, &s);
                    if let Some(x) = self.repl.as_mut() {
                        x.owed.retain(|a| *a != peer);
                        // the other primary counted what it took since our fork (split brain), this one what
                        // it took in its epoch
                        if s.primary && s.epoch < x.epoch && x.role == Role::Primary {
                            x.stats.rival_commits = s.commits;
                            x.stats.split_commits = x.epoch_commits;
                        }
                    }
                    self.repl_fence_check_done(os);
                }
                self.repl_close(os, sock);
            }
            (Kind::Follower(_), F_ACK) => {
                let (Some(recv), Some(fl), Some(ap)) = (r.u64(), r.u64(), r.u64()) else { return false };
                if let Some(x) = self.repl.as_mut() {
                    if let Some(Conn { kind: Kind::Follower(f), .. }) = x.conns.get_mut(&sock) {
                        f.recv = f.recv.max(recv);
                        f.flushed = f.flushed.max(fl);
                        f.applied = f.applied.max(ap);
                    }
                    let e = x.member_recv.entry(peer.to_u32()).or_insert(0);
                    *e = (*e).max(recv);
                }
                self.repl_prune_answered();
                self.repl_sync_check(os);
            }
            (Kind::Up { .. }, F_WELCOME) => {
                let (Some(epoch), Some(_start), Some(level), Some(copy)) = (r.u64(), r.u64(), r.u8(), r.u8()) else { return false };
                let mut history = Vec::new();
                for _ in 0..r.u8().unwrap_or(0) {
                    let (Some(e), Some(at)) = (r.u64(), r.u64()) else { break };
                    history.push((e, at));
                }
                if let Some(x) = self.repl.as_mut() {
                    // the primary's history is its log's: a promotion of this replica goes on from it
                    if !history.is_empty() {
                        x.history = history;
                    }
                    // its log goes on in the primary's epoch from here (after a copy, or where both logs agree)
                    x.epoch = epoch;
                    x.max_epoch_seen = x.max_epoch_seen.max(epoch);
                    x.level = level;
                    x.retry_armed = false;
                    if let Some(c) = x.conns.get_mut(&sock) {
                        c.kind = Kind::Up { welcomed: true };
                    }
                    os.log(5, "db", &format!("Replikation: folgt {} ab Position {}", peer, self.lsn));
                }
                if copy == 0 {
                    let lsn = self.lsn;
                    Db::note_on(os, Note::DbReplConnected { primary: peer, from: lsn });
                }
                self.repl_status(os, true);
            }
            (Kind::Up { .. }, F_REC) => {
                let Some(rec) = rec_of(b) else { return false };
                return self.repl_record(rec, b.len() as u64 + HEAD as u64);
            }
            (Kind::Up { .. }, F_BEAT) => {
                let (Some(pos), Some(epoch), Some(at)) = (r.u64(), r.u64(), r.u64()) else { return false };
                let lsn = self.lsn;
                if let Some(x) = self.repl.as_mut() {
                    x.known = x.known.max(pos);
                    x.max_epoch_seen = x.max_epoch_seen.max(epoch);
                    if x.known > lsn && x.behind_since.is_none() {
                        x.behind_since = Some(at);
                    }
                }
            }
            (Kind::Up { .. }, F_MEMBERS) => {
                let Some(n) = r.u16() else { return false };
                let mut list = Vec::new();
                for _ in 0..n {
                    let Some(a) = r.u32() else { break };
                    list.push(Ipv4::from_u32(a));
                }
                if let Some(x) = self.repl.as_mut() {
                    x.members = list;
                }
            }
            (Kind::Up { .. }, F_NOT) => {
                let (Some(epoch), Some(a)) = (r.u64(), r.u32()) else { return false };
                if let Some(x) = self.repl.as_mut() {
                    x.max_epoch_seen = x.max_epoch_seen.max(epoch);
                    let to = Ipv4::from_u32(a);
                    if a != 0 && !os.is_mine(to) && x.up_addr != Some(to) {
                        os.log(5, "db", &format!("Replikation: {} ist kein Primary, folgt jetzt {}", peer, to));
                        x.up_addr = Some(to);
                    }
                }
                self.repl_close(os, sock);
            }
            (Kind::Up { .. }, F_COPY) => self.repl_copy_start(os, b),
            (Kind::Up { .. }, F_BULK) => {
                let now = os.now();
                if let Some(c) = self.repl.as_mut().and_then(|x| x.copy.as_mut()) {
                    c.got += b.len() as u64 + HEAD as u64;
                    if c.t_half.is_none() && c.got * 2 >= c.probe {
                        c.t_half = Some(now);
                    }
                    if c.got >= c.probe {
                        c.t_end = Some(now);
                    }
                }
            }
            (Kind::Up { .. }, F_ROW) => self.repl_copy_row(b),
            (Kind::Up { .. }, F_END) => {
                let Some(at) = r.u64() else { return false };
                self.repl_copy_end(os, at);
            }
            _ => {}
        }
        false
    }

    // ------------------------------------------------ the primary

    #[allow(clippy::too_many_arguments)]
    fn repl_hello(&mut self, os: &mut OsApi, sock: u32, peer: Ipv4, epoch: u64, pos: u64, hash: u64, flags: u8) {
        let now = os.now();
        let hash_now = self.spec_hash();
        let (flushed, floor) = (self.flushed, self.tail_floor);
        let Some(r) = self.repl.as_mut() else { return };
        if r.role != Role::Primary || r.fenced || r.checking.is_some() || epoch > r.epoch {
            // not the one to follow: who is
            let to = if r.role == Role::Replica { r.up_addr.map_or(0, |a| a.to_u32()) } else { 0 };
            let mut w = Wr::new(F_NOT);
            w.u64(r.epoch).u32(to);
            self.repl_send(os, sock, w.done(0));
            self.repl_close(os, sock);
            return;
        }
        let diverged = fork_of(&r.history, epoch).is_some_and(|f| pos > f);
        let copy = flags & 1 != 0 || hash != hash_now || pos > flushed || pos < floor || diverged;
        let mut f = Follower { sent: pos, recv: pos, flushed: pos, applied: pos, copy: None };
        if copy {
            f.copy = Some(CopySend { started: false, probe_left: 0, table: 0, key: 0, snap: flushed });
            f.sent = flushed;
        }
        if let Some(c) = r.conns.get_mut(&sock) {
            c.kind = Kind::Follower(f);
            c.heard = now;
        }
        if !r.members.contains(&peer) {
            r.members.push(peer);
        }
        let e = r.member_recv.entry(peer.to_u32()).or_insert(0);
        *e = (*e).max(pos);
        let mut w = Wr::new(F_WELCOME);
        w.u64(r.epoch).u64(if copy { flushed } else { pos }).u8(r.cfg.mode.level()).u8(copy as u8).u8(r.history.len().min(32) as u8);
        for (e, at) in r.history.iter().rev().take(32).rev() {
            w.u64(*e).u64(*at);
        }
        self.repl_send(os, sock, w.done(0));
        self.repl_members_out(os);
        Db::note_on(os, Note::DbReplica { peer, up: true });
        if copy {
            os.log(5, "db", &format!("Replikation: {} bekommt eine Kopie der Daten", peer));
        } else {
            os.log(5, "db", &format!("Replikation: {} folgt ab Position {}", peer, pos));
        }
        self.repl_fill(os, sock);
    }

    /// the replicas it streams to learn who else follows
    fn repl_members_out(&mut self, os: &mut OsApi) {
        let Some(r) = self.repl.as_ref() else { return };
        let list: Vec<Ipv4> = r.followers().map(|(_, c, _)| c.peer).collect();
        let socks: Vec<u32> = r.followers().map(|(s, _, _)| *s).collect();
        let mut w = Wr::new(F_MEMBERS);
        w.u16(list.len() as u16);
        for a in &list {
            w.u32(a.to_u32());
        }
        let f = w.done(0);
        for s in socks {
            self.repl_send(os, s, f.clone());
        }
    }

    /// what the database says of itself in STATE
    fn repl_state(&self, commits: u64) -> State {
        let Some(r) = self.repl.as_ref() else { return State::default() };
        State { epoch: r.epoch, primary: r.role == Role::Primary && !r.fenced, lsn: self.lsn, follows: if r.role == Role::Replica { r.up_addr } else { None }, commits, history: r.history.clone() }
    }

    /// the commits it answered beyond a position (what a failover loses)
    pub fn answered_after(&self, lsn: u64) -> u64 {
        self.repl.as_ref().map_or(0, |r| r.answered.iter().filter(|x| **x > lsn).count() as u64)
    }
    fn repl_commits_after(&self, fork: u64) -> u64 {
        self.answered_after(fork)
    }

    /// another member's state: follow it, step down, or count a split brain
    fn repl_state_in(&mut self, os: &mut OsApi, from: Ipv4, s: &State) {
        let Some(r) = self.repl.as_mut() else { return };
        r.max_epoch_seen = r.max_epoch_seen.max(s.epoch);
        if s.epoch <= r.epoch {
            if s.primary && s.epoch < r.epoch && r.role == Role::Primary && !r.fenced && r.rival != Some(from) {
                r.rival = Some(from);
                os.log(3, "db", &format!("Split Brain: {} nimmt in einer alten Epoche ({}) weiter Schreibzugriffe an", from, s.epoch));
                Db::note_on(os, Note::DbSplitBrain { other: from });
            }
            return;
        }
        // a newer epoch: its primary
        let newer = if s.primary { Some(from) } else { s.follows };
        let Some(newer) = newer.filter(|a| !os.is_mine(*a)) else { return };
        match r.role {
            Role::Replica => {
                if r.promoting {
                    return;
                }
                // (its own epoch stays that of its log until the new primary welcomes it)
                if r.up_addr != Some(newer) {
                    r.up_addr = Some(newer);
                    os.log(5, "db", &format!("Replikation: folgt dem neuen Primary {} (Epoche {})", newer, s.epoch));
                    Db::note_on(os, Note::DbReplFollow { primary: newer, epoch: s.epoch });
                    if let Some(up) = self.repl.as_mut().and_then(|x| x.upstream.take()) {
                        if let Some(x) = self.repl.as_mut() {
                            x.conns.remove(&up);
                        }
                        os.tcp_abort(up);
                    }
                    self.repl_connect(os);
                }
            }
            Role::Primary => {
                let my_epoch = r.epoch;
                let fork = fork_of(&s.history, my_epoch).unwrap_or(s.lsn);
                let mine = self.answered_after(fork);
                let Some(r) = self.repl.as_mut() else { return };
                if r.cfg.fencing {
                    r.stats.fenced_lost += mine;
                    os.log(3, "db", &format!("Abgegrenzt: {} ist Primary in Epoche {}; {} Commits seit der Abzweigung gehen verloren", newer, s.epoch, mine));
                    self.repl_step_down(os, newer, s.epoch);
                } else if r.rival != Some(newer) {
                    r.rival = Some(newer);
                    r.stats.split_commits = mine;
                    os.log(3, "db", &format!("Split Brain: {} ist Primary in Epoche {}, diese Datenbank nimmt ohne Fencing weiter Schreibzugriffe an; {} Commits seit der Abzweigung", newer, s.epoch, mine));
                    Db::note_on(os, Note::DbSplitBrain { other: newer });
                }
            }
        }
    }

    /// a primary that learned of a newer one (fencing): no more writes, it follows the new one
    fn repl_step_down(&mut self, os: &mut OsApi, newer: Ipv4, epoch: u64) {
        self.drop_work(os, "fenced", "Ein neuerer Primary ist da: diese Datenbank nimmt keine Schreibzugriffe mehr an.");
        let Some(r) = self.repl.as_mut() else { return };
        let waits: Vec<SyncWait> = r.sync_wait.drain(..).collect();
        r.fenced = true;
        r.role = Role::Replica;
        r.max_epoch_seen = r.max_epoch_seen.max(epoch);
        r.up_addr = Some(newer);
        r.fallback = false;
        r.checking = None;
        r.owed.clear();
        r.applied = self.lsn;
        let socks: Vec<u32> = r.conns.iter().filter(|(_, c)| matches!(c.kind, Kind::Follower(_))).map(|(s, _)| *s).collect();
        for w in waits {
            if let Some(t) = w.timer {
                self.wheel.cancel(t);
            }
            self.reply(os, w.sid, 503, Db::fail_body("fenced", "Der Commit ist lokal geschehen, aber kein Replikat hat ihn bestätigt, und ein neuerer Primary ist da: er gilt nicht.", 0, false), Some(w.start));
        }
        for s in socks {
            if let Some(x) = self.repl.as_mut() {
                x.conns.remove(&s);
            }
            os.tcp_abort(s);
        }
        Db::note_on(os, Note::DbFenced { epoch });
        self.repl_connect(os);
    }

    /// every statement and transaction goes, with this error to whoever waits (a step down, a base copy)
    fn drop_work(&mut self, os: &mut OsApi, code: &str, text: &str) {
        let ids: Vec<u64> = self.execs.keys().copied().collect();
        for id in ids {
            if let Some(mut e) = self.execs.remove(&id) {
                if let Some(s) = e.snap_held.take() {
                    self.snap_drop(s);
                }
                self.refuse(os, e.sid, 503, code, text, 0, true, Some(e.start));
            }
        }
        let txns: Vec<u64> = self.txns.keys().copied().collect();
        let quiet = self.repl.as_mut().map(|r| core::mem::replace(&mut r.fenced, true));
        for t in txns {
            self.abort(os, t);
        }
        if let (Some(r), Some(q)) = (self.repl.as_mut(), quiet) {
            r.fenced = q;
        }
        self.ready.clear();
        self.pending.clear();
        let parked: Vec<Parked> = self.repl.as_mut().map_or(Vec::new(), |r| r.parked.drain(..).collect());
        for p in parked {
            self.refuse(os, p.req.id, 503, code, text, 0, true, Some(p.req.at));
        }
    }

    /// after a flush (from `was` to the flushed position): a primary ships, a replica says so and applies
    pub(super) fn repl_flushed(&mut self, os: &mut OsApi, was: u64) {
        let Some(r) = self.repl.as_ref() else { return };
        if r.role == Role::Primary && !r.fenced {
            // the commit records just flushed get their time: the lag counts from here
            let now = os.now();
            let from = self.wal_tail.partition_point(|x| x.lsn <= was);
            let flushed = self.flushed;
            for rec in self.wal_tail.iter_mut().skip(from) {
                if rec.lsn > flushed {
                    break;
                }
                if rec.at == 0 && matches!(rec.kind, WalKind::Commit) {
                    rec.at = now;
                }
            }
            let socks: Vec<u32> = self.repl.as_ref().map_or(Vec::new(), |r| r.followers().map(|(s, _, _)| *s).collect());
            for s in socks {
                self.repl_fill(os, s);
            }
            self.repl_sync_check(os);
        } else if r.role == Role::Replica {
            let lvl = r.level;
            self.repl_status(os, lvl >= 2);
            self.repl_apply_kick(os);
            if self.flushing.is_none() && self.lsn > self.flushed {
                self.start_flush(os);
            }
        }
    }

    /// hands a follower what is due: its copy, then the flushed records
    fn repl_fill(&mut self, os: &mut OsApi, sock: u32) {
        // (fills up to OUT_LOW and hands it to TCP, again while TCP takes all: only a full send buffer brings
        // TcpWritable, which calls this again)
        let mut budget = 1 << 16;
        loop {
            budget -= 1;
            if budget == 0 {
                break;
            }
            let (flushed, floor) = (self.flushed, self.tail_floor);
            let Some(r) = self.repl.as_ref() else { return };
            let Some(c) = r.conns.get(&sock) else { return };
            if c.closing {
                break;
            }
            if c.out.len() >= OUT_LOW {
                self.repl_flush_out(os, sock);
                if self.repl.as_ref().and_then(|r| r.conns.get(&sock)).is_none_or(|c| !c.out.is_empty()) {
                    return;
                }
                continue;
            }
            let Kind::Follower(f) = &c.kind else { return };
            if let Some(cp) = f.copy.clone() {
                let frame = self.repl_copy_next(os, sock, cp);
                match frame {
                    Some(fr) => {
                        if let Some(c) = self.repl.as_mut().and_then(|x| x.conns.get_mut(&sock)) {
                            c.out.extend_from_slice(&fr);
                        }
                        continue;
                    }
                    None => continue,
                }
            }
            if f.sent < floor {
                // it fell out of the log tail: a copy
                let snap = flushed;
                if let Some(Conn { kind: Kind::Follower(f), peer, .. }) = self.repl.as_mut().and_then(|x| x.conns.get_mut(&sock)) {
                    f.copy = Some(CopySend { started: false, probe_left: 0, table: 0, key: 0, snap });
                    f.sent = snap;
                    let p = *peer;
                    os.log(4, "db", &format!("Replikation: {} ist aus dem Log-Rest gefallen und bekommt eine Kopie", p));
                }
                continue;
            }
            let i = self.wal_tail.partition_point(|x| x.lsn <= f.sent);
            let Some(rec) = self.wal_tail.get(i) else { break };
            if rec.lsn > flushed {
                break;
            }
            let size = match &rec.kind {
                WalKind::Row { table, .. } => self.tables.get(*table as usize).map_or(REC_HEAD, |t| t.spec.row_bytes + REC_HEAD),
                _ => REC_HEAD,
            };
            let fr = rec_frame(rec, size);
            let lsn = rec.lsn;
            if let Some(x) = self.repl.as_mut() {
                x.stats.records_out += 1;
                if let Some(Conn { kind: Kind::Follower(f), out, .. }) = x.conns.get_mut(&sock) {
                    f.sent = lsn;
                    out.extend_from_slice(&fr);
                }
            }
        }
        self.repl_flush_out(os, sock);
    }

    /// the next frame of a base copy (None: it moved on without one)
    fn repl_copy_next(&mut self, os: &mut OsApi, sock: u32, mut cp: CopySend) -> Option<Vec<u8>> {
        let l = self.cfg.layout;
        let bulk: u64 = self.tables.iter().map(|t| t.disk_bytes(&l)).sum();
        let out = if !cp.started {
            cp.started = true;
            cp.probe_left = bulk.min(PROBE);
            // the primary reads its data for the copy (its disk is busy with it)
            let _ = os.disk_read(bulk);
            let specs = self.repl.as_ref().and_then(|r| r.adopted.clone()).or_else(|| self.tables_cfg.clone()).unwrap_or(Value::Null);
            let text = agent_core::json::stringify(&specs).unwrap_or_default();
            let rows: u64 = self.tables.iter().map(|t| t.rows.len() as u64).sum();
            let mut w = Wr::new(F_COPY);
            w.u64(cp.snap).u64(self.repl.as_ref().map_or(1, |r| r.epoch)).u64(self.repl.as_ref().and_then(|r| r.adopted_seed).unwrap_or(self.cfg.seed)).u64(bulk).u64(rows).bytes(text.as_bytes());
            if let Some(r) = self.repl.as_mut() {
                r.stats.copies += 1;
            }
            Some(w.done(0))
        } else if cp.probe_left > 0 {
            let n = cp.probe_left.min(BULK_FRAME);
            cp.probe_left -= n;
            Some(Wr::new(F_BULK).done(n))
        } else {
            // the touched rows, table by table in key order, as their newest committed version on the disk
            let mut found = None;
            while cp.table < self.tables.len() {
                let t = &self.tables[cp.table];
                if let Some((k, row)) = t.rows.range(cp.key..).next() {
                    cp.key = *k + 1;
                    let flushed = self.flushed;
                    if let Some(v) = row.vers.iter().rev().find(|v| v.ts != 0 && v.lsn <= flushed) {
                        let mut w = Wr::new(F_ROW);
                        w.u16(cp.table as u16).u64(*k).u8(v.deleted as u8).u64(v.lsn).u16(v.vals.len() as u16);
                        for (c, x) in &v.vals {
                            w.u16(*c).val(x);
                        }
                        found = Some(w.done(t.spec.row_bytes + REC_HEAD));
                        break;
                    }
                    continue;
                }
                cp.table += 1;
                cp.key = 0;
            }
            match found {
                Some(f) => Some(f),
                None => {
                    // the end: consistent once the log is applied up to where the primary is now
                    let mut w = Wr::new(F_END);
                    w.u64(self.flushed);
                    if let Some(Conn { kind: Kind::Follower(f), .. }) = self.repl.as_mut().and_then(|x| x.conns.get_mut(&sock)) {
                        f.copy = None;
                    }
                    if let Some(c) = self.repl.as_mut().and_then(|x| x.conns.get_mut(&sock)) {
                        c.out.extend_from_slice(&w.done(0));
                    }
                    return None;
                }
            }
        };
        if let Some(Conn { kind: Kind::Follower(f), .. }) = self.repl.as_mut().and_then(|x| x.conns.get_mut(&sock)) {
            f.copy = Some(cp);
        }
        if let (Some(r), Some(o)) = (self.repl.as_mut(), out.as_ref()) {
            r.stats.copy_bytes += o.len() as u64;
        }
        out
    }

    /// commits that every member received need no counting any more
    fn repl_prune_answered(&mut self) {
        let Some(r) = self.repl.as_mut() else { return };
        let min = r.member_recv.values().copied().min().unwrap_or(0);
        while r.answered.front().is_some_and(|x| *x <= min) {
            r.answered.pop_front();
        }
    }

    /// a commit's answer: now, or when enough replicas have its record
    pub(super) fn repl_answer_commit(&mut self, os: &mut OsApi, sid: u64, body: String, start: Ns, lsn: u64) {
        let now = os.now();
        let body = with_lsn(body, lsn);
        let Some(r) = self.repl.as_mut() else { return };
        if r.role == Role::Primary && !r.fenced && r.cfg.mode != Mode::Async && !r.fallback {
            r.stats.sync_waits += 1;
            let timer = (r.cfg.sync_timeout > 0).then(|| self.wheel.add(now + r.cfg.sync_timeout, DTimer::Repl(RT::SyncOut(lsn))));
            r.sync_wait.push_back(SyncWait { lsn, sid, body, start, timer });
            self.repl_sync_check(os);
            return;
        }
        self.repl_answered(lsn);
        self.reply(os, sid, 200, body, Some(start));
    }

    fn repl_answered(&mut self, lsn: u64) {
        let Some(r) = self.repl.as_mut() else { return };
        r.epoch_commits += 1;
        if r.answered.len() >= ANSWERED_MAX {
            r.answered.pop_front();
            r.answered_dropped += 1;
        }
        r.answered.push_back(lsn);
    }

    /// the position `sync_count` replicas have reached at the level the mode waits for
    fn repl_pos_k(&self) -> u64 {
        let Some(r) = self.repl.as_ref() else { return 0 };
        let lvl = r.cfg.mode.level();
        let mut ps: Vec<u64> = r
            .followers()
            .filter(|(_, _, f)| f.copy.is_none())
            .map(|(_, _, f)| match lvl {
                1 => f.recv,
                2 => f.flushed,
                _ => f.applied,
            })
            .collect();
        ps.sort_unstable_by(|a, b| b.cmp(a));
        ps.get(r.cfg.sync_count.max(1) as usize - 1).copied().unwrap_or(0)
    }

    /// answers the commits whose records enough replicas have; ends a fallback once they caught up
    fn repl_sync_check(&mut self, os: &mut OsApi) {
        let pos = self.repl_pos_k();
        let flushed = self.flushed;
        loop {
            let Some(r) = self.repl.as_mut() else { return };
            if !r.sync_wait.front().is_some_and(|w| w.lsn <= pos) {
                break;
            }
            let Some(w) = r.sync_wait.pop_front() else { break };
            if let Some(t) = w.timer {
                self.wheel.cancel(t);
            }
            self.repl_answered(w.lsn);
            self.reply(os, w.sid, 200, w.body, Some(w.start));
        }
        let Some(r) = self.repl.as_mut() else { return };
        if r.fallback && pos >= flushed {
            r.fallback = false;
            let mode = r.cfg.mode.name();
            os.log(5, "db", &format!("Replikation: die Replikate haben aufgeholt, wieder {}", mode));
            Db::note_on(os, Note::DbSyncFallback { on: false });
        }
    }

    /// a commit waited too long: async from now on, until the replicas caught up
    fn repl_sync_out(&mut self, os: &mut OsApi, lsn: u64) {
        let Some(r) = self.repl.as_mut() else { return };
        if !r.sync_wait.front().is_some_and(|w| w.lsn <= lsn) {
            return;
        }
        r.fallback = true;
        r.stats.sync_fallbacks += 1;
        let waits: Vec<SyncWait> = r.sync_wait.drain(..).collect();
        let (mode, ms) = (r.cfg.mode.name(), r.cfg.sync_timeout / MS);
        os.log(4, "db", &format!("Replikation: kein Replikat hat nach {} ms bestätigt, Commits gehen jetzt async statt {}", ms, mode));
        Db::note_on(os, Note::DbSyncFallback { on: true });
        for w in waits {
            if let Some(t) = w.timer {
                self.wheel.cancel(t);
            }
            self.repl_answered(w.lsn);
            self.reply(os, w.sid, 200, w.body, Some(w.start));
        }
    }

    // ------------------------------------------------ the replica

    /// a log record came: into its log (duplicates are skipped)
    fn repl_record(&mut self, rec: WalRec, bytes: u64) -> bool {
        if rec.lsn <= self.lsn {
            return false;
        }
        let Some(r) = self.repl.as_mut() else { return false };
        if r.copy.as_ref().is_some_and(|c| c.consistent_at.is_none() && c.done_at.is_none() && rec.lsn <= c.snap) {
            return false;
        }
        r.stats.records_in += 1;
        r.stats.bytes_in += bytes;
        r.known = r.known.max(rec.lsn);
        if rec.txn & APPLIED == 0 {
            r.max_txn = r.max_txn.max(rec.txn);
        }
        if matches!(rec.kind, WalKind::Commit) && r.behind_since.is_none() {
            r.behind_since = Some(rec.at);
        }
        self.stats.wal_bytes += rec.lsn - self.lsn;
        self.lsn = rec.lsn;
        self.wal_rec(rec);
        true
    }

    /// after records came: it says so (semi waits for that) and writes them
    fn repl_received(&mut self, os: &mut OsApi) {
        let lvl = self.repl.as_ref().map_or(0, |r| r.level);
        self.repl_status(os, lvl >= 1);
        if self.flushing.is_none() && self.lsn > self.flushed {
            self.start_flush(os);
        }
    }

    /// tells the primary how far it is: at once, or at most every STATUS_EVERY while something changed, and once
    /// a heartbeat when nothing did
    fn repl_status(&mut self, os: &mut OsApi, now_too: bool) {
        let now = os.now();
        let (lsn, flushed) = (self.lsn, self.flushed);
        let Some(r) = self.repl.as_mut() else { return };
        let Some(up) = r.upstream else { return };
        if !r.welcomed() {
            return;
        }
        let pos = (lsn, flushed, r.applied);
        // (unchanged, it still says so once a heartbeat: the primary hears that it is there)
        if pos == r.acked && now < r.acked_at + r.cfg.heartbeat {
            return;
        }
        if !now_too && now < r.acked_at + STATUS_EVERY {
            if !r.status_armed {
                r.status_armed = true;
                let at = r.acked_at + STATUS_EVERY;
                self.wheel.add(at, DTimer::Repl(RT::Status));
            }
            return;
        }
        r.acked = pos;
        r.acked_at = now;
        let mut w = Wr::new(F_ACK);
        w.u64(pos.0).u64(pos.1).u64(pos.2);
        self.repl_send(os, up, w.done(0));
    }

    /// the applier: the next job of records its disk has, one after the other on its CPU
    pub(super) fn repl_apply_kick(&mut self, os: &mut OsApi) {
        let now = os.now();
        loop {
            let Some(r) = self.repl.as_ref() else { return };
            if r.role != Role::Replica || r.applying.is_some() || self.recovering.is_some() {
                return;
            }
            if r.copy.as_ref().is_some_and(|c| c.done_at.is_none_or(|d| d > now) && c.consistent_at.is_none()) {
                return;
            }
            let (apply_cpu, row_cpu) = (r.cfg.apply_cpu, r.cfg.apply_row_cpu);
            let from = self.wal_tail.partition_point(|x| x.lsn <= r.applied);
            let mut cpu: Ns = 0;
            let mut last = None;
            let mut keys: Vec<(u16, u64)> = Vec::new();
            for rec in self.wal_tail.iter().skip(from).take(APPLY_BATCH) {
                if rec.lsn > self.flushed || cpu >= crate::res::QUANTUM {
                    break;
                }
                cpu += apply_cpu;
                if let WalKind::Row { table, key, .. } = rec.kind {
                    cpu += row_cpu;
                    keys.push((table, key));
                }
                last = Some(rec.lsn);
            }
            let Some(last) = last else { break };
            // the rows' pages through its own pool: a miss reads its disk
            let l = self.cfg.layout;
            let mut disk = now;
            for (t, k) in keys {
                let Some(tb) = self.tables.get(t as usize) else { continue };
                let access = Access::Key { lo: k, hi: k };
                let cost = cost_of(tb, &l, &access, 1, 1);
                let touch = walk(&mut self.pool, tb, &l, &access, &cost, &[k], true);
                self.recent_pool.add(now, touch.hits, touch.misses());
                for _ in 0..touch.misses() {
                    disk = os.disk_read(l.page);
                }
            }
            if !self.metered {
                cpu = 0;
            }
            let at = os.cpu(cpu).max(disk);
            if at > now {
                if let Some(r) = self.repl.as_mut() {
                    r.applying = Some(last);
                }
                self.wheel.add(at, DTimer::Repl(RT::Applied(last)));
                return;
            }
            self.repl_apply_to(os, last);
        }
        self.repl_caught_up(os);
    }

    /// the records up to this position take effect
    fn repl_apply_to(&mut self, os: &mut OsApi, to: u64) {
        let now = os.now();
        let Some(r) = self.repl.as_ref() else { return };
        let from = self.wal_tail.partition_point(|x| x.lsn <= r.applied);
        let recs: Vec<WalRec> = self.wal_tail.iter().skip(from).take_while(|x| x.lsn <= to).cloned().collect();
        let mut commits = false;
        for rec in recs {
            let Some(r) = self.repl.as_mut() else { return };
            r.applied = rec.lsn;
            r.stats.applied_records += 1;
            match rec.kind {
                WalKind::Row { .. } => r.pending.entry(rec.txn).or_default().push(rec),
                WalKind::Abort => {
                    r.pending.remove(&rec.txn);
                }
                WalKind::Commit => {
                    let rows = r.pending.remove(&rec.txn).unwrap_or_default();
                    r.stats.applied_commits += 1;
                    if rec.at > 0 {
                        r.lag.record(now, now.saturating_sub(rec.at));
                    }
                    self.repl_commit_rows(rows, rec.txn, rec.lsn);
                    commits = true;
                }
            }
        }
        if commits {
            self.bound();
            if !self.ckpt_armed {
                self.ckpt_armed = true;
                let at = now + self.cfg.checkpoint;
                self.wheel.add(at, DTimer::Checkpoint);
            }
        }
        // the oldest commit not applied yet
        let applied = self.repl.as_ref().map_or(0, |r| r.applied);
        let next = self.wal_tail.iter().skip(self.wal_tail.partition_point(|x| x.lsn <= applied)).find(|x| matches!(x.kind, WalKind::Commit)).map(|x| x.at);
        let lsn = self.lsn;
        if let Some(r) = self.repl.as_mut() {
            r.behind_since = match next {
                Some(t) => Some(t),
                None if r.known > lsn => r.behind_since,
                None => None,
            };
        }
        if commits {
            let lvl = self.repl.as_ref().map_or(0, |r| r.level);
            self.repl_status(os, lvl >= 3);
            self.repl_unpark(os);
        }
    }

    /// a transaction's rows show, together, with one commit stamp
    fn repl_commit_rows(&mut self, rows: Vec<WalRec>, txn: u64, lsn: u64) {
        let t = txn | APPLIED;
        let mut keys = Vec::with_capacity(rows.len());
        for rec in rows {
            if let WalKind::Row { table, key, vals, gone } = rec.kind
                && let Some(tb) = self.tables.get_mut(table as usize)
            {
                tb.write(key, t, rec.lsn, vals, gone);
                if key > tb.next_key {
                    tb.next_key = key;
                }
                keys.push((table, key));
            }
        }
        self.clock += 1;
        let ts = self.clock;
        let (horizon, durable) = (self.horizon(), self.flushed);
        for (ti, k) in keys {
            if let Some(tb) = self.tables.get_mut(ti as usize) {
                tb.commit_row(k, t, ts);
                if let Some(v) = tb.rows.get_mut(&k).and_then(|r| r.vers.last_mut())
                    && v.ts == ts
                {
                    v.lsn = lsn;
                }
                tb.prune_row(k, horizon, durable);
                if tb.rows.get(&k).is_some_and(|r| r.vers.len() >= 2) {
                    self.old.insert((ti, k));
                } else {
                    self.old.remove(&(ti, k));
                }
            }
        }
    }

    /// the applier has nothing left: a promotion may end, a copy becomes consistent
    fn repl_caught_up(&mut self, os: &mut OsApi) {
        let lsn = self.lsn;
        let flushed = self.flushed;
        let Some(r) = self.repl.as_mut() else { return };
        if let Some(c) = &r.copy
            && let Some(at) = c.consistent_at
            && c.done_at.is_none()
            && r.applied >= at
        {
            r.copy = None;
            r.need_copy = false;
            os.log(5, "db", &format!("Replikation: die Kopie ist vollständig, Position {}", r.applied));
            let at = r.applied;
            Db::note_on(os, Note::DbReplCaughtUp { lsn: at });
        }
        let Some(r) = self.repl.as_ref() else { return };
        if r.promoting && r.applied >= lsn && flushed >= lsn && r.applying.is_none() {
            self.repl_promoted(os);
        }
    }

    /// reads that waited for a position the apply reached, and those whose wait is over
    fn repl_unpark(&mut self, os: &mut OsApi) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        if r.parked.is_empty() {
            return;
        }
        let applied = r.applied;
        let mut go = Vec::new();
        let mut keep = Vec::new();
        for p in r.parked.drain(..) {
            if p.min <= applied || p.until <= now {
                go.push(p);
            } else {
                keep.push(p);
            }
        }
        r.parked = keep;
        for p in go {
            let stale = p.min > applied;
            if stale {
                if let Some(r) = self.repl.as_mut() {
                    r.stats.stale_reads += 1;
                }
            }
            self.query_on(os, p.req, stale);
        }
    }

    // ------------------------------------------------ the base copy (replica)

    fn repl_copy_start(&mut self, os: &mut OsApi, b: &[u8]) {
        let mut r = Rd { b, i: 0 };
        let (Some(snap), Some(epoch), Some(seed), Some(bulk), Some(_rows)) = (r.u64(), r.u64(), r.u64(), r.u64(), r.u64()) else { return };
        let Some(text) = r.bytes() else { return };
        let specs = agent_core::json::parse(text).unwrap_or(Value::Null);
        let mut list: Vec<TableSpec> = Vec::new();
        for t in specs.as_array().unwrap_or(&[]) {
            match super::table_of(t) {
                Ok(s) => list.push(s),
                Err(_) => return,
            }
        }
        self.drop_work(os, "copying", "Das Replikat bekommt gerade eine Kopie der Daten: bitte gleich noch einmal.");
        // its tables anew, from the primary's definitions and seed
        let l = self.cfg.layout;
        if self.reserved {
            let had: u64 = self.tables.iter().map(|t| t.disk_bytes(&l)).sum();
            os.disk_free(had);
            self.reserved = false;
        }
        self.tables = list.into_iter().enumerate().map(|(i, s)| Table::new(s, i as u16, seed)).collect();
        let need: u64 = self.tables.iter().map(|t| t.disk_bytes(&l)).sum();
        if os.disk_reserve(need) {
            self.reserved = true;
            self.no_disk = None;
        } else {
            self.no_disk = Some((need, os.disk().map_or(0, |d| d.capacity)));
        }
        self.pool = Pool::new(self.cfg.pool_bytes / l.page);
        for t in &self.tables {
            self.pool.set_heap(t.no, l.heap_pages(t.spec.rows, t.spec.row_bytes));
        }
        self.snaps.clear();
        self.old.clear();
        self.locks.clear();
        self.clock += 1;
        os.disk_free(self.wal_disk);
        self.wal_disk = 0;
        self.lsn = snap;
        self.flushed = snap;
        self.ckpt = snap;
        self.wal_tail.clear();
        self.tail_floor = snap;
        let Some(x) = self.repl.as_mut() else { return };
        x.adopted = Some(specs);
        x.adopted_seed = Some(seed);
        x.epoch = x.epoch.max(epoch);
        x.applied = snap;
        x.known = snap;
        x.pending.clear();
        x.behind_since = None;
        x.stats.copies += 1;
        let probe = bulk.min(PROBE);
        x.copy = Some(CopyRecv { bulk, probe, got: 0, snap, t_half: None, t_end: if probe == 0 { Some(os.now()) } else { None }, done_at: None, consistent_at: None });
        os.log(5, "db", &format!("Replikation: bekommt eine Kopie: {} Daten ab Position {}", crate::disk::size_text(bulk), snap));
        Db::note_on(os, Note::DbReplCopy { bytes: bulk });
    }

    fn repl_copy_row(&mut self, b: &[u8]) {
        let mut r = Rd { b, i: 0 };
        let (Some(t), Some(key), Some(gone), Some(lsn), Some(n)) = (r.u16(), r.u64(), r.u8(), r.u64(), r.u16()) else { return };
        let mut vals = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let (Some(c), Some(v)) = (r.u16(), r.val()) else { return };
            vals.push((c, v));
        }
        let ts = self.clock;
        let Some(tb) = self.tables.get_mut(t as usize) else { return };
        tb.write(key, BASE_TXN, lsn, vals, gone != 0);
        tb.commit_row(key, BASE_TXN, ts);
        if key > tb.next_key {
            tb.next_key = key;
        }
        if let Some(x) = self.repl.as_mut() {
            x.stats.copy_bytes += b.len() as u64 + HEAD as u64;
        }
    }

    /// the real part of the copy is in: the rest takes the time the probe measured, and the disk writes all
    fn repl_copy_end(&mut self, os: &mut OsApi, consistent_at: u64) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        let Some(c) = r.copy.as_mut() else { return };
        let rest = c.bulk.saturating_sub(c.probe);
        let wait = match (c.t_half, c.t_end) {
            (Some(h), Some(e)) if e > h && rest > 0 => {
                let half = c.probe - c.probe / 2;
                (rest as u128 * (e - h) as u128 / half.max(1) as u128) as Ns
            }
            _ => 0,
        };
        let disk = os.disk_write(c.bulk, 0).unwrap_or(now);
        let done = (now + wait).max(disk);
        c.done_at = Some(done);
        c.consistent_at = Some(consistent_at);
        self.wheel.add(done, DTimer::Repl(RT::CopyDone));
    }

    // ------------------------------------------------ promotion, members

    /// a replica becomes primary (dbctl promote, Action::DbPromote): after it flushed and applied what it
    /// received; `lost`: the commits the old primary answered beyond that, as the engine counted them
    pub fn promote(&mut self, os: &mut OsApi, lost: Option<u64>) -> Result<(), Msg> {
        let Some(r) = self.repl.as_mut() else {
            return Err(msg!("Diese Datenbank hat keine Replikation."));
        };
        if r.role == Role::Primary {
            return Err(msg!("Diese Datenbank ist schon Primary."));
        }
        if r.promoting {
            return Err(msg!("Diese Datenbank wird schon Primary."));
        }
        if r.copy.is_some() || r.need_copy || self.tables.is_empty() {
            return Err(msg!("Das Replikat hat noch keine vollständige Kopie der Daten: es kann nicht Primary werden."));
        }
        r.promoting = true;
        r.old_primary = r.up_addr;
        if let Some(n) = lost {
            r.stats.failover_lost += n;
        }
        if let Some(up) = r.upstream.take() {
            r.conns.remove(&up);
            os.tcp_abort(up);
        }
        os.log(5, "db", "Replikation: wird Primary, wendet zuerst an, was es schon hat");
        if self.flushing.is_none() && self.lsn > self.flushed {
            self.start_flush(os);
        }
        self.repl_apply_kick(os);
        self.repl_caught_up(os);
        Ok(())
    }

    /// the promotion is done: a new epoch, the open transactions end, the members hear of it
    fn repl_promoted(&mut self, os: &mut OsApi) {
        let start = self.lsn;
        let open: Vec<u64> = self.repl.as_ref().map_or(Vec::new(), |r| r.pending.keys().copied().collect());
        for txn in open {
            self.lsn += REC_HEAD;
            let lsn = self.lsn;
            self.stats.wal_bytes += REC_HEAD;
            self.wal_rec(WalRec { lsn, txn, at: 0, kind: WalKind::Abort });
        }
        let Some(r) = self.repl.as_mut() else { return };
        r.pending.clear();
        r.promoting = false;
        r.epoch = r.max_epoch_seen.max(r.epoch) + 1;
        r.max_epoch_seen = r.epoch;
        r.history.push((r.epoch, start));
        r.role = Role::Primary;
        r.fenced = false;
        r.fallback = false;
        r.rival = None;
        r.epoch_commits = 0;
        r.answered.clear();
        r.member_recv.clear();
        r.stats.promotions += 1;
        let epoch = r.epoch;
        let mut owed: Vec<Ipv4> = r.members.iter().copied().filter(|a| !os.is_mine(*a)).collect();
        if let Some(o) = r.old_primary
            && !owed.contains(&o)
        {
            owed.push(o);
        }
        r.members = owed.clone();
        r.owed = owed;
        self.next_txn = self.next_txn.max(r.max_txn) + 1;
        os.log(5, "db", &format!("Replikation: ist jetzt Primary in Epoche {} ab Position {}", epoch, start));
        Db::note_on(os, Note::DbPromoted { epoch });
        if self.flushing.is_none() && self.lsn > self.flushed {
            self.start_flush(os);
        }
        self.repl_tell_owed(os);
        self.repl_announce_arm(os);
    }

    fn repl_announce_arm(&mut self, os: &mut OsApi) {
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        if !r.announce_armed && !r.owed.is_empty() {
            r.announce_armed = true;
            let at = now + r.cfg.heartbeat * 4;
            self.wheel.add(at, DTimer::Repl(RT::Announce));
        }
    }

    /// tells every member it still owes its state (a connection each, unless one is on its way)
    fn repl_tell_owed(&mut self, os: &mut OsApi) {
        let port = self.rport();
        let now = os.now();
        let Some(r) = self.repl.as_mut() else { return };
        let busy: Vec<Ipv4> = r.conns.values().filter(|c| matches!(c.kind, Kind::Tell)).map(|c| c.peer).collect();
        let list: Vec<Ipv4> = r.owed.iter().copied().filter(|a| !busy.contains(a)).collect();
        for a in list {
            if let Ok(s) = os.tcp_connect(IpAddr::V4(a), port, Db::opts()) {
                r.conns.insert(s, Conn::new(Kind::Tell, a, now));
            }
        }
    }

    /// a returning primary heard from every member (or waited long enough)
    fn repl_fence_check_done(&mut self, os: &mut OsApi) {
        let Some(r) = self.repl.as_mut() else { return };
        if r.checking.is_some() && (r.owed.is_empty() || r.role != Role::Primary) {
            r.checking = None;
            if r.role == Role::Primary {
                os.log(5, "db", "Replikation: keine neuere Epoche gefunden, nimmt Schreibzugriffe an");
            }
        }
    }

    // ------------------------------------------------ timers

    pub(super) fn repl_timer(&mut self, os: &mut OsApi, t: RT) {
        let now = os.now();
        match t {
            RT::Retry => {
                if let Some(r) = self.repl.as_mut() {
                    r.retry_armed = false;
                    if r.role == Role::Replica && r.upstream.is_some() && !r.welcomed() {
                        // the connection is still on its way: it gets its own end
                        return;
                    }
                }
                self.repl_connect(os);
            }
            RT::Applied(to) => {
                if let Some(r) = self.repl.as_mut() {
                    if r.applying != Some(to) {
                        return;
                    }
                    r.applying = None;
                }
                self.repl_apply_to(os, to);
                self.repl_apply_kick(os);
            }
            RT::Status => {
                if let Some(r) = self.repl.as_mut() {
                    r.status_armed = false;
                }
                self.repl_status(os, true);
            }
            RT::SyncOut(lsn) => self.repl_sync_out(os, lsn),
            RT::CopyDone => {
                if let Some(c) = self.repl.as_mut().and_then(|r| r.copy.as_mut())
                    && c.done_at.is_some_and(|d| d <= now)
                {
                    c.done_at = None;
                }
                self.repl_apply_kick(os);
            }
            RT::FenceDone => {
                if let Some(r) = self.repl.as_mut() {
                    r.owed.clear();
                }
                self.repl_fence_check_done(os);
            }
            RT::Announce => {
                if let Some(r) = self.repl.as_mut() {
                    r.announce_armed = false;
                    if r.role != Role::Primary || r.fenced {
                        r.owed.clear();
                        return;
                    }
                }
                self.repl_tell_owed(os);
                self.repl_announce_arm(os);
            }
            RT::Beat => self.repl_beat(os),
        }
    }

    /// every heartbeat: a primary's beat to replicas it sent nothing lately, silent connections end, a replica
    /// alone too long promotes itself (failover_ms)
    fn repl_beat(&mut self, os: &mut OsApi) {
        let now = os.now();
        let flushed = self.flushed;
        let Some(r) = self.repl.as_mut() else { return };
        r.beat_armed = false;
        let (hb, timeout) = (r.cfg.heartbeat, r.cfg.timeout);
        let mut beat = Vec::new();
        let mut dead = Vec::new();
        for (s, c) in &r.conns {
            if now.saturating_sub(c.heard) >= timeout && !matches!(c.kind, Kind::Tell) {
                dead.push(*s);
            } else if matches!(c.kind, Kind::Follower(_)) && now.saturating_sub(c.sent_at) >= hb {
                beat.push(*s);
            }
        }
        let epoch = r.epoch;
        for s in dead {
            self.repl_gone(os, s);
            os.tcp_abort(s);
        }
        for s in beat {
            let mut w = Wr::new(F_BEAT);
            w.u64(flushed).u64(epoch).u64(now);
            self.repl_send(os, s, w.done(0));
        }
        // a replica's own status, whether it fell behind, its failover
        self.repl_status(os, false);
        if let Some(r) = self.repl.as_mut()
            && r.role == Role::Replica
        {
            let lag = r.behind_since.map_or(0, |t| now.saturating_sub(t) / MS);
            if !r.behind && lag > 1000 {
                r.behind = true;
                Db::note_on(os, Note::DbReplBehind { lag_ms: lag });
            } else if r.behind && lag < 100 {
                r.behind = false;
                let at = r.applied;
                Db::note_on(os, Note::DbReplCaughtUp { lsn: at });
            }
        }
        let Some(r) = self.repl.as_mut() else { return };
        if r.role == Role::Replica && !r.promoting && r.cfg.failover > 0 && !r.need_copy && r.copy.is_none() && r.lost_since.is_some_and(|t| now.saturating_sub(t) >= r.cfg.failover) {
            os.log(4, "db", &format!("Replikation: seit {} ms nichts vom Primary, wird selbst Primary", r.cfg.failover / MS));
            let _ = self.promote(os, None);
        }
        // the watch on the primary's silence starts when the connection does
        if let Some(r) = self.repl.as_mut()
            && r.role == Role::Replica
            && r.upstream.is_some()
            && r.lost_since.is_none()
            && r.upstream.and_then(|s| r.conns.get(&s)).is_some_and(|c| now.saturating_sub(c.heard) >= timeout.min(r.cfg.failover.max(hb)))
        {
            r.lost_since = r.upstream.and_then(|s| r.conns.get(&s)).map(|c| c.heard);
        }
        self.repl_beat_arm(os);
    }

    // ------------------------------------------------ reads, writes, answers

    /// writes are refused here: (code, text)
    pub(super) fn repl_no_writes(&self, s: &Stmt) -> Option<(&'static str, String)> {
        let r = self.repl.as_ref()?;
        if !writes(s) {
            return None;
        }
        if r.promoting {
            return Some(("promoting", String::from("Das Replikat wird gerade Primary: bitte gleich noch einmal.")));
        }
        if r.checking.is_some() {
            return Some(("fence_check", String::from("Die Datenbank fragt nach einem Neustart erst ihre Mitglieder, ob es einen neueren Primary gibt.")));
        }
        if r.role == Role::Replica {
            let p = r.up_addr.map_or(String::new(), |a| format!(" Schreibzugriffe gehen an den Primary {}.", a));
            if r.fenced {
                return Some(("fenced", format!("Ein neuerer Primary ist da: diese Datenbank nimmt keine Schreibzugriffe mehr an.{}", p)));
            }
            return Some(("read_only", format!("Ein Replikat nimmt nur Lesezugriffe an.{}", p)));
        }
        None
    }

    /// the gate of a read on a replica: no copy yet, a copy on its way, a position not reached (Err: answered
    /// or parked); Ok(stale)
    pub(super) fn repl_gate(&mut self, os: &mut OsApi, r: ReqIn) -> Result<(ReqIn, bool), ()> {
        let now = os.now();
        let Some(x) = self.repl.as_mut() else { return Ok((r, false)) };
        if x.role != Role::Replica && !x.promoting {
            return Ok((r, false));
        }
        if self.tables.is_empty() || x.copy.is_some() || x.need_copy {
            self.stats.refused += 1;
            self.refuse(os, r.id, 503, "copying", "Das Replikat hat noch keine vollständige Kopie der Daten.", 0, true, Some(r.at));
            return Err(());
        }
        // a position the read needs
        let Some((min, wait)) = min_lsn_of(&r) else { return Ok((r, false)) };
        if min <= x.applied {
            return Ok((r, false));
        }
        if wait > 0 {
            x.stats.read_waits += 1;
            x.parked.push(Parked { req: r, min, until: now + wait });
            self.wheel.add(now + wait, DTimer::Repl(RT::Status));
            return Err(());
        }
        x.stats.stale_reads += 1;
        Ok((r, true))
    }

    /// GET /_db/health and /_db/health/replica with replication
    pub(super) fn repl_health(&self, os: &mut OsApi, r: &ReqIn) -> Option<(u16, String)> {
        let x = self.repl.as_ref()?;
        let now = os.now();
        let v = self.repl_view(now)?;
        let path = r.head.path();
        if path == "/_db/health/replica" {
            let max = crate::kv::query_value(&r.head.target, "max_lag_ms").and_then(|s| s.parse::<u64>().ok());
            let why = if x.role != Role::Replica || x.promoting {
                Some("kein Replikat")
            } else if v.copying.is_some() {
                Some("kopiert die Daten")
            } else if !v.connected {
                Some("ohne Verbindung zum Primary")
            } else if self.recovering.is_some() {
                Some("liest sein Log")
            } else if max.is_some_and(|m| v.lag_ms > m) {
                Some("zu weit zurück")
            } else {
                None
            };
            return Some(match why {
                None => (200, format!("{{\"ok\":true,\"lag_ms\":{},\"lag_bytes\":{},\"applied_lsn\":{}}}\n", v.lag_ms, v.lag_bytes, v.applied)),
                Some(w) => (503, format!("{{\"ok\":false,\"why\":\"{}\",\"lag_ms\":{},\"lag_bytes\":{}}}\n", w, v.lag_ms, v.lag_bytes)),
            });
        }
        let status = if self.recovering.is_some() {
            "recovering"
        } else if v.copying.is_some() {
            "copying"
        } else if v.promoting {
            "promoting"
        } else if v.fenced {
            "fenced"
        } else {
            "ok"
        };
        let role = if v.role == Role::Primary { "primary" } else { "replica" };
        let mut b = format!("{{\"status\":\"{}\",\"role\":\"{}\",\"epoch\":{},\"lsn\":{},\"flushed_lsn\":{},\"checkpoint_lsn\":{}", status, role, v.epoch, self.lsn, self.flushed, self.ckpt);
        if v.role == Role::Primary {
            b.push_str(&format!(",\"mode\":\"{}\",\"sync_count\":{},\"fallback\":{},\"replicas\":[", v.mode.name(), v.sync_count, v.fallback));
            for (i, f) in v.followers.iter().enumerate() {
                if i > 0 {
                    b.push(',');
                }
                b.push_str(&format!("{{\"addr\":\"{}\",\"received\":{},\"flushed\":{},\"applied\":{},\"lag_ms\":{},\"lag_bytes\":{},\"copying\":{}}}", f.addr, f.received, f.flushed, f.applied, f.lag_ms, f.lag_bytes, f.copying));
            }
            b.push(']');
        } else {
            b.push_str(&format!(",\"applied_lsn\":{},\"primary\":\"{}\",\"connected\":{},\"lag_ms\":{},\"lag_bytes\":{}", v.applied, v.primary.unwrap_or(Ipv4::UNSPECIFIED), v.connected, v.lag_ms, v.lag_bytes));
        }
        b.push_str(&format!(",\"recoveries\":{},\"recovery_ms\":{}}}\n", self.stats.recoveries, self.stats.recovery_ms));
        Some((200, b))
    }

    /// replication as the window and dbctl show it
    pub fn repl_view(&self, now: Ns) -> Option<ReplView> {
        let r = self.repl.as_ref()?;
        let commit_at = |after: u64| self.wal_tail.iter().skip(self.wal_tail.partition_point(|x| x.lsn <= after)).find(|x| matches!(x.kind, WalKind::Commit) && x.lsn <= self.flushed).map(|x| x.at);
        let followers = r
            .followers()
            .map(|(_, c, f)| {
                let lag_ms = if f.applied < self.flushed { commit_at(f.applied).filter(|t| *t > 0).map_or(0, |t| now.saturating_sub(t) / MS) } else { 0 };
                FollowerView { addr: c.peer, received: f.recv, flushed: f.flushed, applied: f.applied, lag_bytes: self.flushed.saturating_sub(f.applied), lag_ms, copying: f.copy.is_some() }
            })
            .collect::<Vec<_>>();
        let (lag_ms, lag_bytes) = match r.role {
            Role::Replica => (r.behind_since.map_or(0, |t| now.saturating_sub(t) / MS), r.known.max(self.lsn).saturating_sub(r.applied)),
            Role::Primary => (followers.iter().map(|f| f.lag_ms).max().unwrap_or(0), followers.iter().map(|f| f.lag_bytes).max().unwrap_or(0)),
        };
        let copying = r.copy.as_ref().map(|c| if c.done_at.is_none() && c.consistent_at.is_some() { 100 } else { (c.got.min(c.probe) * 100 / c.probe.max(1)).min(99) });
        Some(ReplView {
            role: r.role,
            epoch: r.epoch,
            mode: r.cfg.mode,
            sync_count: r.cfg.sync_count,
            port: r.cfg.port,
            fallback: r.fallback,
            fenced: r.fenced,
            promoting: r.promoting,
            checking: r.checking.is_some(),
            primary: if r.role == Role::Replica { r.up_addr } else { None },
            connected: r.welcomed(),
            copying: copying.or(if r.need_copy && r.role == Role::Replica { Some(0) } else { None }),
            applied: if r.role == Role::Replica { r.applied } else { self.lsn },
            lag_ms,
            lag_bytes,
            lag_p50: r.lag.p50(now),
            lag_p99: r.lag.p99(now),
            followers,
            members: r.members.clone(),
            rival: r.rival,
            history: r.history.clone(),
            stats: r.stats,
        })
    }

    /// a replica that does not reach its primary (problems.rs)
    pub fn repl_problem(&self, now: Ns) -> Option<Msg> {
        let r = self.repl.as_ref()?;
        if r.role != Role::Replica || r.promoting || !self.serve.listening() {
            return None;
        }
        let since = r.lost_since?;
        if now.saturating_sub(since) < LOST_PROBLEM {
            return None;
        }
        Some(msg!("Das Replikat erreicht seinen Primary {} nicht.", format!("{}", r.up_addr.unwrap_or(Ipv4::UNSPECIFIED))))
    }

    /// the numbers of replication (Service::metrics_at)
    pub(super) fn repl_metrics(&self, now: Ns, out: &mut Vec<Metric>) {
        let Some(v) = self.repl_view(now) else { return };
        let s = v.stats;
        let i = |x: u64| x as i64;
        out.push(Metric::new("role", if v.role == Role::Primary { 1 } else { 2 }));
        out.push(Metric::new("epoch", i(v.epoch)));
        out.push(Metric::new("replicas", v.followers.iter().filter(|f| !f.copying).count() as i64));
        out.push(Metric::new("lag_ms", i(v.lag_ms)));
        out.push(Metric::new("lag_bytes", i(v.lag_bytes)));
        out.push(Metric::new("applied_lsn", i(v.applied)));
        out.push(Metric::new("applied_records", i(s.applied_records)));
        out.push(Metric::new("applied_commits", i(s.applied_commits)));
        out.push(Metric::new("stale_reads", i(s.stale_reads)));
        out.push(Metric::new("read_waits", i(s.read_waits)));
        out.push(Metric::new("sync_waits", i(s.sync_waits)));
        out.push(Metric::new("sync_waiting", self.repl.as_ref().map_or(0, |r| r.sync_wait.len() as i64)));
        out.push(Metric::new("sync_fallbacks", i(s.sync_fallbacks)));
        out.push(Metric::new("repl_bytes_out", i(s.bytes_out)));
        out.push(Metric::new("repl_bytes_in", i(s.bytes_in)));
        out.push(Metric::new("copies", i(s.copies)));
        out.push(Metric::new("failover_lost", i(s.failover_lost)));
        out.push(Metric::new("split_commits", i(s.split_commits)));
        out.push(Metric::new("fenced_lost", i(s.fenced_lost)));
        out.push(Metric::new("fenced", v.fenced as i64));
        out.push(Metric::new("lag_p50_us", (v.lag_p50.unwrap_or(0) / US) as i64));
        out.push(Metric::new("lag_p99_us", (v.lag_p99.unwrap_or(0) / US) as i64));
    }

    /// its status line with replication
    pub(super) fn repl_status_msg(&self) -> Option<Msg> {
        let v = self.repl_view(self.seen)?;
        let port = self.cfg.port;
        Some(match v.role {
            Role::Replica if v.promoting => msg!("Port {}: wird Primary, wendet zuerst an, was es hat", port),
            Role::Replica if v.copying.is_some() => msg!("Port {}: Replikat, bekommt eine Kopie der Daten ({} %)", port, v.copying.unwrap_or(0)),
            Role::Replica if !v.connected => msg!("Port {}: Replikat, erreicht seinen Primary {} nicht", port, format!("{}", v.primary.unwrap_or(Ipv4::UNSPECIFIED))),
            Role::Replica if v.fenced => msg!("Port {}: abgegrenzt (ein neuerer Primary ist da), folgt {}, Rückstand {} ms", port, format!("{}", v.primary.unwrap_or(Ipv4::UNSPECIFIED)), v.lag_ms),
            Role::Replica => msg!("Port {}: Replikat von {}, Rückstand {} ms ({} Bytes), {} Commits angewendet", port, format!("{}", v.primary.unwrap_or(Ipv4::UNSPECIFIED)), v.lag_ms, v.lag_bytes, v.stats.applied_commits),
            Role::Primary if v.checking => msg!("Port {}: Primary nach einem Neustart, fragt erst seine Mitglieder", port),
            Role::Primary if v.rival.is_some() => msg!("Port {}: Split Brain, auch {} nimmt Schreibzugriffe an", port, format!("{}", v.rival.unwrap_or(Ipv4::UNSPECIFIED))),
            Role::Primary if v.fallback => msg!("Port {}: Primary (Epoche {}), {} statt {}: kein Replikat hat rechtzeitig bestätigt", port, v.epoch, "async", v.mode.name()),
            Role::Primary => msg!("Port {}: Primary (Epoche {}, {}), {} Replikate, Rückstand bis {} ms", port, v.epoch, v.mode.name(), v.followers.iter().filter(|f| !f.copying).count(), v.lag_ms),
        })
    }

    /// replication's settings live (Service::reconfigure): all but role, primary and port
    pub(super) fn repl_reconfigure(&mut self, fresh: Option<&Repl>) -> Result<(), Msg> {
        match (self.repl.as_mut(), fresh) {
            (None, None) => Ok(()),
            (Some(r), Some(f)) if r.cfg.live(&f.cfg) => {
                r.cfg = f.cfg.clone();
                Ok(())
            }
            _ => Err(msg!("Rolle, Primary und Port der Replikation ändern sich nur mit einem Neustart.")),
        }
    }
}

/// the position a read needs and how long it may wait for it: "min_lsn", "wait_ms" of its JSON
fn min_lsn_of(r: &ReqIn) -> Option<(u64, Ns)> {
    let first = r.body.iter().copied().find(|b| !b.is_ascii_whitespace())?;
    if first != b'{' {
        return None;
    }
    let v = agent_core::json::parse(&r.body).ok()?;
    let min = v.get("min_lsn").and_then(|x| x.as_u64())?;
    let wait = v.get("wait_ms").and_then(|x| decimal(x, MS)).unwrap_or(0).min(60 * SEC);
    Some((min, wait))
}

// ---------------------------------------------------------------- the engine side

impl Engine {
    /// the device and name of the database with replication at this address
    pub fn db_at(&self, a: Ipv4) -> Option<(u32, alloc::string::String)> {
        for d in self.devices() {
            let mine = self.node_index(d).and_then(|ni| self.stack(ni)).is_some_and(|s| s.is_mine(a));
            if !mine {
                continue;
            }
            for v in self.services(d) {
                if self.service::<Db>(d, &v.name).is_some_and(|x| x.repl.is_some()) {
                    return Some((d, v.name.clone()));
                }
            }
        }
        None
    }
    /// what a failover to this replica loses: the commits the primary it followed answered beyond what it
    /// received (an observation: the engine looks at both)
    pub fn db_lost_at(&self, dev: u32, svc: &str) -> Option<u64> {
        let d = self.service::<Db>(dev, svc)?;
        let a = d.repl.as_ref()?.up_addr?;
        let (od, os) = self.db_at(a)?;
        Some(self.service::<Db>(od, &os)?.answered_after(d.lsn))
    }
    /// the replication of a database for the lab
    pub fn db_repl_view(&self, dev: u32, svc: &str) -> Option<ReplView> {
        self.service::<Db>(dev, svc)?.repl_view(self.now)
    }
}
