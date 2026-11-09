# Performance

*Entwicklerdoku von JFNetworker auf Deutsch; die Übersicht auf Englisch steht in der [README](../README.md). Teile: [Überblick](OVERVIEW.md) · [Bedienung](USAGE.md) · [Bauen und testen](BUILDING.md) · [Aufbau](REFERENCE.md) · [Performance](PERFORMANCE.md) · [Fahrplan](ROADMAP.md) · [Marken](TRADEMARKS.md)*


Große Projekte bleiben flüssig: `tests/perf.uis` verlangt mit 500 Geräten, 800 Kabeln und 300
Rahmen gleichzeitig auf den Kabeln in 1920 × 1080 höchstens 33 ms pro Bild (30 fps).

- **Testprojekt:** `JFNetworker --gen-topology 500` öffnet ohne Datei einen synthetischen
  Campus aus Katalogmodellen (`jfn_model::synth`): 7 Kern-Router im 100G-Ring, 19 Pods aus einem
  Router mit zwei Uplinks, zwei Access-Switches, PCs, Drucker, Kamera und Servern mit je einem
  Port an beiden Switches; statische Routen, Adressen aus 10.P.1.0/24 und 10.P.2.0/24. Kabel über
  einen Baum hinaus verbinden nur geroutete Interfaces oder die zwei Netze eines Servers, es gibt
  also keine Schleife auf Schicht 2 (ohne Spanning Tree). `jfn-sim gen 500 --out campus.jfnet`
  schreibt dasselbe Projekt als Datei; gleiche Größe, gleiches Projekt.
- **Verkehr:** `--traffic 300` startet 300 Pings auf einmal (`Engine::ping_many`), deren
  ARP-Anfragen zur Zeit 0 alle auf den Kabeln liegen; der Simulationsmodus zeigt sie als 300
  bewegte Rahmen (die Zahl steht im Semantik-Eintrag „Rahmen auf den Kabeln“).
- **Messung:** `bench N anim|drag|wheel … 33000` (ui/auto.rs) misst mit der echten Uhr die
  Durchläufe und das Neuzeichnen der schmutzigen Zellen pro Bild; der Vergleich von
  `--verify-dirty` läuft danach, ungezählt. Bestanden ist ein Lauf, dessen Median höchstens 33 ms
  beträgt, mit bis zu drei Läufen gegen Last auf dem Rechner.

Was es schnell macht:

- **Backing-Store der Leinwand** (`ui::diagram`): Raster, Kabel und Geräte werden einmal pro
  Version (Projekt, Ansicht, LEDs, Theme) in eine eigene Fläche gemalt; jedes schmutzige Rechteck
  kopiert nur noch seinen Teil heraus. Bewegte Rahmen, Hover und Auswahl malen die Szene nicht
  mehr neu.
- **Culling über den räumlichen Index:** der Painter fragt die Buckets des Widgets
  (`DgFrame::find`) und rechnet nur für sichtbare Kabel LEDs und Portnamen.
- **Detailstufe während Gesten:** solange Verschieben, Zoomen mit dem Rad oder das Ziehen von
  Geräten läuft (`DgFrame::moving`) und mehr als 150 Geräte und Kabel zu sehen sind, entfallen
  Namen, LEDs und Portnamen, Kabel sind schlichte 1-px-Linien (`dg_hairline`); danach zeichnet die
  Leinwand wieder in voller Qualität.
- **Rahmen:** ihre Beschriftungen („Rahmen N: A → B · Info“) werden einmal dekodiert und
  gemerkt, Rahmen außerhalb der Ansicht nicht gezeichnet, doppelte Kennungen (Nummer % 1024)
  nicht vergeben.
- **Szene:** `Scene::build` sucht Geräte, Modelle und Ports über Tabellen statt linear.
- **Engine:** jede Weiterleitung schlug die Routingtabelle neu zusammen; jetzt hält `IpStack`
  die Tabelle der letzten Suche mit dem, woraus sie entstand (Zustand und Adressen der Interfaces,
  statische Routen), und baut sie nur bei einer Änderung neu. Determinismus und Golden-Berichte
  bleiben unverändert.

Messwerte auf dem Entwicklungsrechner (Windows, Median von drei Läufen in ms, 1920 × 1080):

| Szene | vorher | nachher |
|---|---|---|
| Echtzeit, die 300 Pings laufen | 1,3–1,4 | 1,3–1,5 |
| Simulation, ganzer Campus (17 %), Rahmen bewegen sich | 7,7–8,4 | 3,7–4,6 |
| … Verschieben mit der mittleren Taste | 9,1–10,6 | 7,1–8,3 |
| … Zoom mit Strg + Rad | 9,2–10,7 | 7,2–8,1 |
| 100 % über zwei Pods, Rahmen bewegen sich | 2,3–2,6 | 2,2–2,3 |
| … Verschieben | 8,9–10,4 | 6,9–8,0 |
| … Zoom | 9,6–12,2 | 7,2–9,0 |
| … einen Server ziehen | 10,9–12,8 | 8,4–10,1 |
| Abspielen mit 8× | 28,6–35,0 | 3,3–3,4 |

Ein ganzes Fenster neu zeichnen dauert statt 17–21 ms noch 3–4 ms; 300 Pings mit gelerntem ARP
(9 460 Ereignisse) rechnet die Engine in 6–7 statt 14 ms. Was ein Bild jetzt noch kostet, sind
vor allem die Durchläufe der übrigen Oberfläche (Panels, zwei Durchläufe pro Bild während einer
Geste).
