# Changelog

All notable changes to JFNetworker are recorded here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). `python tools/release/release.py
prepare X.Y.Z` turns `[Unreleased]` into a release and raises the version in `PACKAGE` and
`Cargo.toml`; [docs/RELEASE.md](docs/RELEASE.md) describes the whole release.

## [Unreleased]

### Changed

- RIP without `version` sends version 1 and accepts 1 and 2, as IOS does, and routes show IOS's
  hop counts (`[120/1]` for a network one hop away).
- `switchport mode dynamic auto` (the default) and `dynamic desirable` negotiate a trunk as IOS's
  DTP does: a port left at its default facing `switchport mode trunk` becomes a trunk.
- The running configuration writes banners as `banner motd ^C...^C`, as IOS does.
- A scenario's own `pcap FILE` and `series FILE` steps write only below the scenario's folder;
  other places need `--pcap` or `--series`.
- A project file with a number above 2147483647 or with the same device, cable or note number
  twice is refused when it is opened.
- "Jetzt installieren" first shows how many courses and how many bytes it would install; more than
  1 GiB at once is refused.
- Delete also removes a selected background picture.

### Fixed

- Linux shell: `>` empties its file before the command runs (`sort f > f`), a line ending in `|`
  is a syntax error, assignments are neither globbed nor split, `sleep` with a huge number no
  longer ends at once, `wait` waits for every background job, and `ping -w` and `ping -I` work.
- IOS banners with `#` or `^` in them, or over several lines, survive a reload.
- A queue's error rate counts rejected messages, a key-value store's no longer counts replica
  writes, TCP stays exact after 4 GiB, and experiments show page-ins and OOM kills as means over
  the seeds like the other figures.
- A script service takes at most 256 MiB of heap, all script services of a project 1 GiB.
- Wi-Fi reassociations no longer leave unused cable slots behind.
- A complex PDU is sent from the device chosen in its dialog, and the frame links of the packet
  panel no longer collide with its other controls from the 40th frame on.
- An unreadable `progress.json` of the Akademie is kept as a `.bak` copy instead of being
  overwritten, the installer checks a course with the same file limits as the app, and its
  clean-up waits for a running installation.
- A file larger than a project file may be is refused before it is read into memory.
- Closed device windows keep at most 8 desktops.

- An experiment with many long axes is refused at once instead of building every configuration
  (six axes of 50 values froze the Experiments panel).
- The check of a course before it is installed reads each file once, holds one lab at a time,
  refuses labs of more than 2000 devices and stops when the installation is cancelled.
- A file on a device holds at most 8 MiB ("File too large") and a device at most 32 MiB of files
  ("Disk quota exceeded"), whatever its disk: repeated `cat a a a > b` could exhaust the memory of
  the computer running the app.
- Writes to the same file in a row are one undo step, and a step keeps only the device's files;
  each write used to keep two copies of the whole device.
- Finished name lookups and ping or traceroute jobs no longer pile up during long runs; the last
  512 of each stay.

## [0.1.0] - 2026-11-19

The first release of JFNetworker, a network simulator and a lab for distributed systems.

### Added

- Networks on a canvas: about 50 device models (hosts, switches, routers, firewalls, access points,
  servers, modems, IoT devices) and 14 kinds of cable, the ports chosen when connecting, notes and
  labels, groups, and a physical view with buildings, rooms, racks and cable lengths.
- A simulator that sends real frames byte for byte: Ethernet and VLANs, STP and RSTP, ARP, IPv4 and
  IPv6, ICMP, UDP and TCP, DHCP, DNS, RIP, OSPF and BGP, NAT, ACLs and a stateful firewall with
  zones, Wi-Fi with WPA2/WPA3 and 802.1X, SSH, Telnet, SNMP, mail, HTTP and TLS 1.3.
- IOS-like command lines on routers and switches; on hosts a Linux-like shell with files, pipes
  and processes, and a small desktop with a web browser, an IDE for JFScript and a system monitor.
- The simulation mode: every frame step by step, the events, a timeline, the layers and bytes of a
  packet, own PDUs and a pcapng export.
- The Akademie: courses with lessons, labs, quizzes and fault hunts in German and English, installed
  from GitHub (Grundlagen, Fehlersuche), and tasks with an assessment for teachers.
- The architecture lab: web servers, a load balancer and a reverse proxy with rate limits, retries
  and circuit breakers, a key-value store, a cache, a queue, a SQL-shaped database with replication
  and a pooler, CPU and RAM per device, fault injection, metrics with percentiles and parameter
  sweeps that compare scaling up with scaling out.
- German and English, light and dark, the NetLab look; "Erklärung öffnen" opens the page of a
  device or a cable on networker.jfreund.org, Hilfe › Dokumentation the documentation.
- `jfn-sim`: scenarios, sweeps and courses on the command line, without a window.
