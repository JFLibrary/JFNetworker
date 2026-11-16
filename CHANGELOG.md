# Changelog

All notable changes to JFNetworker are recorded here. The format follows
[Keep a Changelog 1.1.0](https://keepachangelog.com/en/1.1.0/), and versions follow
[Semantic Versioning 2.0.0](https://semver.org/spec/v2.0.0.html). `python tools/release/release.py
prepare X.Y.Z` turns `[Unreleased]` into a release and raises the version in `PACKAGE` and
`Cargo.toml`; [docs/RELEASE.md](docs/RELEASE.md) describes the whole release.

## [Unreleased]

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
