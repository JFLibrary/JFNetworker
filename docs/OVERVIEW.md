# JFNetworker im Überblick

*Entwicklerdoku von JFNetworker auf Deutsch; die Übersicht auf Englisch steht in der [README](../README.md). Teile: [Überblick](OVERVIEW.md) · [Bedienung](USAGE.md) · [Bauen und testen](BUILDING.md) · [Aufbau](REFERENCE.md) · [Performance](PERFORMANCE.md) · [Fahrplan](ROADMAP.md) · [Marken](TRADEMARKS.md)*


Ein Netzwerksimulator und Labor für verteilte Systeme, gebaut als Alternative zu Cisco Packet
Tracer: Geräte und Kabel auf einer Zeichenfläche, echte Protokolle Byte für Byte, eine
IOS-artige Befehlszeile für Router und Switches, eine Linux-artige Shell für Hosts, ein
Simulationsmodus Schritt für Schritt mit Paketansicht, und später Dienste, mit denen sich
horizontal skalierte Systeme aufbauen lassen (Loadbalancer, geshardete Datenbanken, Caches,
Queues, Lastgeneratoren, eigene Skripte).

**Stand (2026-11-09): Die Phasen 1 bis 6 sind fertig, Phase 7 läuft** (siehe [Fahrplan](ROADMAP.md#fahrplan)). Seit dem
2026-11-09 hat JFNetworker eine eigene Repo; bis dahin lag es in [JFLib](https://github.com/JFLibrary/JFLib) unter
`apps/JFNetworker`, JFLib kommt jetzt von crates.io ([Bauen und testen](BUILDING.md#bauen-und-testen)).

- **Topologie-Editor:** 49 herstellerneutrale Gerätemodelle aus der Palette ziehen, klicken oder
  per Tastatur platzieren; verbinden mit Port-Auswahl und automatischem oder vorher gewähltem
  Kabeltyp (Kupfer gerade und gekreuzt, Glasfaser Multimode und Singlemode, seriell mit
  gewähltem Ende, Konsole, Funk, WAN, Telefon, Koax, DAC, AOC; Kategorien Cat5e bis Cat8 und OM3
  bis OS2), Kabel mit LEDs, Wegpunkten, Verlauf und Farbe, Notizen, Rückgängig für alles, Projekte
  als `.jfnet`.
- **Simulator im Editor:** läuft in Echtzeit mit und folgt jeder Änderung. Switching mit VLANs,
  IPv4 mit ARP, Routing (auch L3-Switches), ICMP, Ping und Traceroute, UDP und TCP (RFC 9293),
  DHCP mit Relay, DNS, Syslog, NTP, HTTP mit Lastgenerator; die Probleme-Liste zeigt doppelte
  Adressen, fehlende Gateways, Schleifen und anderes.
- **Konsolen:** IOS-artig auf Routern und Switches (Modi, `?`, Tab, `show`-Befehle, `ip dhcp
  pool`, `copy run start` als Rückgängig-Schritt), eine Linux-Shell auf Hosts (`ip`, `ping`,
  `traceroute`, `dhclient`, `curl`, `nc`, `ss`, `top`), als Tab, eigenes Fenster oder im
  Gerätefenster mit Formularen und Desktop.
- **Simulationsmodus:** Pakete als Umschläge auf den Kabeln, Schritt vor und zurück entlang der
  Aufzeichnung, Ereignisliste mit Zeitleiste, Paket-Tab mit Schichten, Feldern, Bytes und
  Prüfsummen, eigene PDUs, Export als pcapng.
- **Labor für verteilte Systeme:** Loadbalancer, geshardeter KV-Speicher mit Replikaten und
  Quoren, Cache, Message Queue mit Workern und Fehlerinjektion mit Marken auf der Zeitleiste,
  im Fenster mit dem Bereich Metriken, Dienst-Pillen und Sparklines auf der Zeichenfläche und
  Fehlern aus dem Inspektor (siehe [Das Labor im Fenster](REFERENCE.md#das-labor-im-fenster)), dazu über
  die Shell (`lbctl`, `kvctl` …) und Szenarien (siehe
  [Labor für verteilte Systeme](REFERENCE.md#labor-für-verteilte-systeme)).
- **Politur (U5):** Deutsch und Englisch, Einstellungen, automatische Sicherung mit
  Wiederherstellung, vollständige Tastaturbedienung, Geräte und Kabel für Screenreader,
  geprüfter Kontrast, Tastenkürzel-Übersicht, App-Symbol und Paketierung (Dateityp `.jfnet`);
  flüssig auch mit 500 Geräten, 800 Kabeln und 300 Paketen in Bewegung (unter 33 ms pro Bild in
  1080p, siehe [Performance](PERFORMANCE.md#performance)).
- **Skripte in Hosts (SK4):** eigene Programme in der Skriptsprache `.jfs` laufen als Dienste:
  UDP, TCP, HTTP-Server und -Client, DNS, Timer, Disk und Metriken, ihre Anweisungen kosten
  Rechenzeit wie auf einer CPU mit einem Thread; `script` in der Shell zeigt und steuert sie
  (siehe [Skripte in Hosts](REFERENCE.md#skripte-in-hosts)).
- **Labore und Aufgaben (U6b):** Datei › Labor öffnen … bringt vier Labore mit (ein VLAN mit
  DHCP-Relay im Campus, ein dritter Webserver hinter dem Loadbalancer, ein KV-Speicher, der
  einen Ausfall übersteht, ein Cache vor einem langsamen Server); das Panel „Aufgabe“ zeigt
  Einleitung, Ziele als Checkliste, die der Simulator laufend prüft, Hinweise und Fortschritt;
  die Einstellungen der Dienste lassen sich im Gerätefenster als JSON bearbeiten (siehe
  [Labore und Aufgaben](REFERENCE.md#labore-und-aufgaben)).
- **IDE (IDE1, vormals Skript-Editor U7):** ein Programm des Desktops eines Hosts, maximiert
  in seinem Fenster: ein Explorer über die Dateien des Geräts und die noch eingebetteten Skripte, Tabs mit Entwürfen,
  die Sprachdienste von SK6 auch über Importe hinweg, Suchen und Ersetzen in Dateien, darunter
  Ausgabe, Terminal, Probleme, Debugger und Tests; eine Datei läuft als Prozess (`jfs DATEI`) oder
  als Dienst, der auf die Datei verweist (`"file"`), mit Haltepunkten, Schritten, Aufrufliste,
  Variablen und Ausdrücken; die `test_…`-Funktionen einer Datei laufen mit `jfs --test` (siehe
  [IDE](REFERENCE.md#ide)).
- **Spanning Tree (R4):** STP (IEEE 802.1D) und RSTP (802.1w) auf den IOS-artigen Switches, je
  Switch eingeschaltet: Wahl der Root-Bridge, Rollen und Zustände, Proposal und Agreement,
  Edge-Ports, BPDU Guard, `show spanning-tree`, bernsteinfarbene LEDs an blockierten Ports
  (siehe [Spanning Tree](REFERENCE.md#spanning-tree)).
- **Routing mit RIP (R2):** Router und L3-Switches sprechen RIPv2 nach RFC 2453 (Version 1 nach
  RFC 1058, wo eingestellt): `router rip` mit `network`, `passive-interface`,
  `default-information originate`, `redistribute`, `timers basic`, `distance` und
  `maximum-paths`; periodische und ausgelöste Updates, Split Horizon mit Poisoned Reverse,
  Timeout, Garbage Collection und Holddown, gleich gute Wege; `show ip rip database`, `show ip
  protocols`, `debug ip rip` (siehe [Routing mit RIP](REFERENCE.md#routing-mit-rip)).
- **NAT und Zugriffslisten (R1):** Router, L3-Switches und die Firewall übersetzen Adressen
  (PAT, Pools, statische Adressen und Port-Weiterleitungen, ICMP-Fehler nach RFC 5508) und
  filtern mit Standard- und erweiterten Zugriffslisten, nummeriert und benannt, mit Zählern und
  den Befehlen des Vorbilds (siehe [NAT und Zugriffslisten](REFERENCE.md#nat-und-zugriffslisten)).
- **IPv6 (R5):** auf denselben Interfaces wie IPv4, aus bis ein Gerät dafür eingerichtet wird:
  Link-Local-Adressen aus der MAC, Duplikaterkennung, Router Advertisements und SLAAC mit
  Lebensdauern, der Neighbor Cache mit allen Zuständen, statische Routen, Weiterleiten mit Hop
  Limit, Packet Too Big und die Fragmente des Absenders, ICMPv6 mit den Grenzen des Vorbilds;
  UDP und TCP über beide Familien (ein Socket auf `::` nimmt beide), der Resolver fragt A und
  AAAA und ordnet sie nach RFC 6724, DNS, HTTP, die TCP-Testdienste und Skripte über IPv6;
  `ipv6 …`, `show ipv6 interface|route|neighbors`, `ping ipv6`, `traceroute ipv6`, `debug ipv6
  icmp|nd`, `ip host`, `ipv6 host`, `ip name-server` und unter Linux `ip -6`, `ping6`,
  `traceroute6`, `sysctl net.ipv6.*`, `curl`, `nc` und `ss` mit `-4`/`-6`, `dig`, `host` und
  `nslookup` (siehe [IPv6](REFERENCE.md#ipv6)).
- **Routing mit OSPF (R3):** Router, L3-Switches und die Firewall sprechen OSPF Version 2 nach
  RFC 2328: `router ospf N` mit `network … area`, `router-id`, `passive-interface`, `auto-cost
  reference-bandwidth`, `area … stub [no-summary]`, `redistribute`, `default-information
  originate`, `distance` und `maximum-paths`, an Interfaces Kosten, Priorität, Timer und
  Netztyp; Hellos, Wahl von DR und BDR, Nachbarn bis Full mit Datenbankabgleich, Flooding mit
  Bestätigungen und Wiederholungen, Altern der LSAs, SPF mit gleich guten Wegen, Bereiche über
  einen ABR, Stub-Bereiche und externe Routen (E1, E2); `show ip ospf
  [neighbor|interface|database]`, `debug ip ospf` (siehe [Routing mit OSPF](REFERENCE.md#routing-mit-ospf)).
- **Aufgaben mit Bewertung (A1):** Labore mit Kriterien, die Punkte bringen: Namen, Adressen und
  Zustände von Interfaces, VLANs und Modi von Switchports, statische Routen, Kabel, Einstellungen
  von Diensten, Zeilen der Running-Config (Abkürzungen zählen) und Verbindungsprüfungen; die
  Rückmeldung je Kriterium, nur als Punktzahl oder erst nach dem Abgeben, wahlweise mit
  Zeitgrenze; Werkzeuge › Aufgabe erstellen … macht aus einem fertigen Netz eine Aufgabe mit
  Ausgangsnetz; drei Beispiele (siehe [Aufgaben mit Bewertung](REFERENCE.md#aufgaben-mit-bewertung)).
- **WLAN (W1):** Access Points und der Heimrouter bieten Netze an (SSID, 2,4, 5 oder 6 GHz mit
  Kanal, offen, WPA2- oder WPA3-Personal, VLAN, Gastnetz, Client-Isolation), Laptops, Smartphones
  und andere Stationen melden sich ohne Kabel an: Suche, Authentifizierung, Assoziation und der
  4-Wege-Handshake mit echten EAPOL-Frames, Signal und Rate aus der Entfernung im Maßstab des
  Projekts, Roaming beim Verschieben, Verbindungsverlust außer Reichweite; Funklinien,
  Reichweiten und Signalbalken auf der Zeichenfläche, die Einstellungen im Inspektor, `iw` in der
  Shell, `show dot11 associations` am Access Point (siehe [WLAN](REFERENCE.md#wlan)).
- **Routing mit BGP (R6):** Router, L3-Switches und die Firewall sprechen BGP-4 nach RFC 4271
  über TCP-Port 179 (4-Byte-AS-Nummern nach RFC 6793, Route Refresh nach RFC 2918): `router bgp
  ASN` mit `bgp router-id`, `neighbor … remote-as|update-source|next-hop-self|ebgp-multihop|
  shutdown|description|weight`, `network … mask …`, `redistribute connected|static|rip|ospf`,
  `timers bgp`, `bgp default local-preference`; die Zustände von Idle bis Established mit
  ConnectRetry, Hold- und Keepalive-Timer und Kollisionen, eBGP mit TTL 1 und iBGP zwischen
  Loopbacks, die Entscheidung Schritt für Schritt, Schleifen im AS-Pfad, Fast External
  Fallover; `show ip bgp [summary|neighbors]`, `show ip route bgp`, `clear ip bgp`, `debug ip
  bgp` (siehe [Routing mit BGP](REFERENCE.md#routing-mit-bgp)).
- **Dateien, TFTP und FTP (X1a):** jedes Gerät hat Dateien, die mit dem Projekt gespeichert
  werden: `flash:` mit dem Systemabbild und `nvram:startup-config` auf den IOS-artigen Geräten,
  ein kleiner Verzeichnisbaum auf den Linux-Hosts; ein TFTP- und ein FTP-Server als Dienste,
  `copy running-config tftp:` und zurück, `copy … ftp:`, `dir`, `more`, `delete`, in der Shell
  `ls`, `cat`, `cp`, `rm`, `mkdir`, `echo … >`, `tftp` und `curl ftp://`; der Reiter Dateien im
  Gerätefenster mit Import und Export (siehe [Dateien, TFTP und FTP](REFERENCE.md#dateien-tftp-und-ftp)).
- **Zurückspulen (K1):** im Simulationsmodus zeigen die Geräte zu jedem Zeitpunkt der
  Aufzeichnung ihren Zustand von damals – LEDs und Kabel, Inspektor, Gerätefenster, Metriken
  und Konsolen, die `show`-Befehle aus diesem Zustand beantworten; der Simulator hält dafür
  Keyframes in einer Speichergrenze und spielt den Rest exakt nach. „Von hier fortsetzen“ macht
  einen solchen Zeitpunkt zur Gegenwart (siehe [Zurückspulen](REFERENCE.md#zurückspulen)).
- **Physische Ansicht (P1):** neben der logischen Zeichenfläche die Orte des Projekts --
  Standorte, Gebäude, Etagen, Räume, Racks und Tische -- als Hierarchie zum Durchwandern, ein
  Raum als Grundriss, ein Rack von vorn mit den Geräten in ihren Höheneinheiten; Geräte werden
  hineingezogen, jedes Kabel bekommt eine Länge (automatisch aus den Orten oder von Hand) mit der
  Grenze seines Mediums nach IEEE 802.3, ein zu langes kommt nicht hoch und ist überall zu sehen
  (siehe [Physische Ansicht](REFERENCE.md#physische-ansicht)).
- **Mail (X1c):** Mailserver mit SMTP (RFC 5321 mit SIZE, 8BITMIME, PIPELINING, erweiterten
  Statuscodes und AUTH auf dem Submission-Port 587) und POP3 (RFC 1939 mit UIDL und TOP):
  Postfächer für die Benutzer der eigenen Domänen in den Dateien des Geräts, Weiterleiten an
  andere Domänen über ihre MX-Einträge mit Warteschlange, Wiederholungen und
  Unzustellbarkeitsnachricht, Relay-Regeln mit einem Schalter „offener Relay“ zum Lernen; das
  Programm E-Mail im Desktop (Konto, Posteingang, Verfassen, Antworten, Senden, Abrufen), in der
  Shell `curl smtp://`, `curl pop3://` und `mail` (siehe [Mail](REFERENCE.md#mail-smtp-und-pop3)).
- **Gruppen und Hintergrundbild (G1):** Geräte, Notizen und Gruppen auf der logischen
  Zeichenfläche zu Gruppen zusammenfassen (Strg+G), verschachtelt; aufgeklappt ein beschrifteter
  Rahmen, zugeklappt ein Cluster-Symbol mit Zahl, Befunden und Dienst-Pillen, an dem die Kabel
  nach draußen als eine Linie je Gegenstelle und Art enden; ziehen, umbenennen, auflösen, alles
  rückgängig; darunter ein Hintergrundbild (ein Grundriss), in der Projektdatei eingebettet,
  verschiebbar, skalierbar, sperrbar (siehe [Gruppen und Hintergrundbild](REFERENCE.md#gruppen-und-hintergrundbild)).
- **SNMP (X1b):** ein SNMP-Agent mit den Versionen 1 und 2c auf den IOS-artigen Geräten
  (`snmp-server community`, `location`, `contact`, `host`, `enable traps`, `show snmp`, `debug
  snmp packets`) und als Dienst `snmpd` auf den Linux-Hosts; die MIB rechnet er bei jeder Anfrage
  aus dem Zustand des Geräts (system, Interfaces mit 64-Bit-Zählern, Adressen, Routen, die Zähler
  von SNMP), mit einer Community `rw` setzt er Kontakt, Standort, Namen und `ifAdminStatus`, er
  meldet coldStart, linkDown, linkUp und authenticationFailure als Traps oder Informs an den
  Dienst `snmptrapd`; in der Shell `snmpget`, `snmpgetnext`, `snmpwalk`, `snmpbulkwalk` und
  `snmpset`, im Desktop ein MIB-Browser (siehe [SNMP](REFERENCE.md#snmp)).
- **Logins, Telnet und AAA (X1d):** Anmeldung an den IOS-artigen Geräten mit `enable secret`,
  `username … secret`, Passwörtern auf den Leitungen und Rechtestufen, das Passwort beim
  Tippen unsichtbar; AAA mit RADIUS (`aaa new-model`, `radius server`, Anmeldung, `enable`,
  Rechte und Accounting; die lokalen Benutzer nur, wenn kein Server antwortet), der Dienst
  `radius-server` auf den Hosts, `telnet` als Client an beiden Konsolen (siehe
  [Logins, Telnet und AAA mit RADIUS](REFERENCE.md#logins-telnet-und-aaa-mit-radius)).
- **Shell mit Ordnern und Prozessen (V1):** ein Arbeitsverzeichnis mit `cd` und `pwd`, die
  klassischen Werkzeuge (`grep`, `find`, `sort`, `tree`, `less`, `man` und viele mehr), Pipes,
  Umleitungen, Listen, Variablen und Platzhalter für jeden Befehl, Tab für Pfade; Skripte laufen
  mit `jfs DATEI` als Prozesse, auch im Hintergrund (`&`, `jobs`, `ps`, `kill`); Ordner in
  `flash:` der IOS-artigen Geräte (siehe
  [Die Shell: Ordner, Werkzeuge, Prozesse](REFERENCE.md#die-shell-ordner-werkzeuge-prozesse)).
- **Module, Debugger und Tests für Skripte (SK7):** ein Skript lädt mit `import("pfad.jfs")`
  Dateien des Geräts; ein Skript-Dienst lässt sich in virtueller Zeit anhalten und schrittweise
  ausführen, mit Haltepunkten, Aufrufliste und Variablen (die Schnittstelle für die kommende
  IDE); `assert` und `assert_eq` für Tests (siehe [Skripte in Hosts](REFERENCE.md#skripte-in-hosts)).
- **Mini-Desktop (V2):** das Gerätefenster eines Hosts ist ein kleiner PC – Programme in eigenen
  Fenstern (Terminal, Dateimanager, Texteditor, IP-Konfiguration, Webbrowser, E-Mail,
  MIB-Browser, Systemmonitor, Einstellungen), Symbole, eine Taskleiste mit Startmenü und Uhr,
  F11 für das Vollbild (siehe [Desktop der Hosts](REFERENCE.md#desktop-der-hosts)).
- **Webbrowser (B1 bis B4):** echte kleine Websites aus dem simulierten Netz im Desktop eines
  Hosts: HTML, Markdown und Text von `http-server`-Routen, Dateien (`"root": "/var/www"`) und
  Skripten mit `http.serve`; Links, Bilder, Formulare mit GET und POST, Verlauf, eigene
  Fehlerseiten, Seitenskripte in `jfs`, Stylesheets aus `<link>`, Cookies, HTTPS mit echtem TLS 1.3
  und eine Entwickleransicht (siehe [Webbrowser](REFERENCE.md#webbrowser)).
- **Sitzungen und Telnet-Server (X1e):** mehrere Befehlszeilen je Gerät mit eigenem Modus,
  Verzeichnis und eigenen Jobs; „Neue Sitzung“ im Terminal-Panel; Telnet auf die
  vty-Leitungen der IOS-artigen Geräte (`transport input`, Anmeldung über Passwort, lokale
  Benutzer oder RADIUS, Sitzungslimit, Leerlaufzeit, `show users`, `clear line`, `terminal
  monitor`) und `telnetd` auf Linux-Hosts (`who`, `w`, `tty`, `ps` mit TTY) (siehe
  [Sitzungen und Telnet-Server](REFERENCE.md#sitzungen-und-telnet-server)).
- **Software-Architektur (A2):** Dienste als Einheiten auf Linux-Hosts (Unit-Dateien,
  `systemctl`, `journalctl -u`, Neustart-Regeln in virtueller Zeit), aus der IDE auf eine Gruppe
  von Hosts bereitstellen, einer nach dem anderen mit Gesundheitsprüfung und Abbruch beim ersten
  Fehler, ein Lasttest mit den Metriken daneben und das Labor `05-microservices` (siehe
  [Einheiten, Bereitstellen und Lasttest](REFERENCE.md#einheiten-bereitstellen-und-lasttest)).
- **SSH und SNMPv3 (X1f):** SSH Version 2 mit echtem Schlüsselaustausch und echter Verschlüsselung:
  `crypto key generate`, `ip ssh`, `transport input ssh` und `ssh -l` auf den IOS-artigen Geräten, der
  Dienst `sshd`, `ssh`, `scp` und `ssh-keygen` auf den Hosts mit `known_hosts` und `authorized_keys`;
  SNMPv3 mit Benutzern, Gruppen und Sichten, HMAC und AES, den Report-PDUs und `-v 3` in den Werkzeugen und
  im MIB-Browser (siehe [SSH](REFERENCE.md#ssh) und [SNMP](REFERENCE.md#snmp)).
