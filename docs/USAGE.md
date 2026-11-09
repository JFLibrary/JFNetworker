# Bedienung

*Entwicklerdoku von JFNetworker auf Deutsch; die Übersicht auf Englisch steht in der [README](../README.md). Teile: [Überblick](OVERVIEW.md) · [Bedienung](USAGE.md) · [Bauen und testen](BUILDING.md) · [Aufbau](REFERENCE.md) · [Performance](PERFORMANCE.md) · [Fahrplan](ROADMAP.md) · [Marken](TRADEMARKS.md)*


- **Startseite:** das Fenster öffnet ohne Projekt auf der Startseite: **Neu** (ein leeres Projekt und
  fünf Vorlagen als Karten: Heimnetz, Büro mit VLANs, Campus, Rechenzentrum mit Leaf-Spine, Webdienst
  hinter Loadbalancer; jede ein laufendes Netz ohne Datei, Speichern fragt nach einem Namen),
  **Zuletzt** (die letzten Projekte mit Ordner und Änderungszeit, Öffnen …; eine fehlende Datei ist
  markiert und lässt sich aus der Liste nehmen, eine Datei außerhalb dessen, was das Programm lesen
  darf, öffnet den Dateidialog an ihrer Stelle) und **Lernen** (die Labore und Aufgaben mit
  Bewertung). Tab läuft durch die Karten, Eingabe öffnet; die Leiste einer automatischen Sicherung
  steht über der Seite und hat dann die Tastatur. Datei › Startseite führt zurück (mit der Frage nach
  ungespeicherten Änderungen), die Einstellung „Beim Start anzeigen“ schaltet die Seite ab.
- **Akademie** (Startseite › Lernen oder Datei › Akademie …): Kurse aus Dateien mit Lektionen zum Lesen,
  Laboren im Simulator, Quiz, Tests und Fehlersuche; ihr Fortschritt bleibt erhalten. Kurse lassen sich mit einem Klick
  von GitHub oder aus einem Ordner installieren, aktualisieren und entfernen (siehe [Akademie](REFERENCE.md#akademie)).
- **Projektinfo** (Datei › Projektinfo …): Beschreibung und Autor des Projekts, gespeichert in der
  Datei, sichtbar oben in den Eigenschaften des Projekts; ein Rückgängig-Schritt.
- **Geräte platzieren:** eine Kachel der Palette auf die Zeichenfläche ziehen, oder anklicken und
  dann auf die Fläche klicken (Umschalt: mehrere), oder per Tastatur: Strg+1 springt in die
  Gerätesuche, Tab zu den Kacheln einer Kategorie, die Pfeiltasten wählen, Eingabe setzt das
  Gerät in die Mitte der Ansicht.
- **Verbinden (C):** von Gerät zu Gerät ziehen oder beide anklicken; per Tastatur Strg+9 (die
  Topologie), Tab zum ersten Gerät, Eingabe, Tab zum zweiten, Eingabe – die Port-Auswahl öffnet
  sich unter dem zweiten Gerät, Eingabe verbindet das beste Paar. Umschalt verbindet sofort.
- **Kabel zuerst wählen:** der Bereich „Kabel“ oben in der Palette: Strg+1, Tab zum Bereich
  (Eingabe klappt ihn auf), Tab zu den Kacheln, die Pfeiltasten wählen, Eingabe setzt das Werkzeug
  Verbinden mit dieser Art; es bleibt bis Esc oder zu einem anderen Werkzeug, die Statusleiste
  nennt die Art.
- **Konsole:** Doppelklick oder Eingabe auf einem Gerät öffnet seine Konsole im Terminal-Bereich
  (Strg+3), mit Umschalt das Gerätefenster.
- **Beschriftung:** Doppelklick auf eine freie Stelle der Zeichenfläche (oder T: in der Mitte der
  Ansicht) öffnet dort ein schmales Eingabefeld. Eingabe oder ein Klick daneben übernimmt den Text
  (ein Schritt zum Rückgängigmachen), Umschalt+Eingabe beginnt eine neue Zeile, Esc verwirft; leer
  wird nichts gespeichert. Die Beschriftung ist nur Text, ohne Karte, in der Textfarbe des Themes;
  ein Rahmen zeigt sich beim Überfahren und wenn sie gewählt ist. Doppelklick oder F2 ändern sie an
  Ort und Stelle, sie verschiebt, löscht und speichert sich wie eine Notiz. Das Werkzeug N legt
  weiter die große Notiz an.
- **Simulation:** F5 Simulationsmodus, Umschalt+F5 Echtzeit, F6 Abspielen/Anhalten, F7 Schritt
  vor, Umschalt+F7 zurück; das Werkzeug P sendet eine einfache PDU (Quelle, dann Ziel – auch mit
  Tab und Eingabe). In der Vergangenheit zeigen die Geräte ihren Zustand von damals, „Von hier
  fortsetzen …“ (Simulation) macht dort weiter.
- **Tastatur:** Strg+1 … 8 zeigen die Bereiche und geben ihnen die Tastatur, Strg+9 die
  Topologie; dort wählt Tab die Geräte und Notizen, dann die Kabel (Eingabe auf einem Kabel: seine
  Eigenschaften). Werkzeugleiste und Palette sind Fokusgruppen (ein Tab-Halt, Pfeiltasten
  innen). F1 zeigt alle Tastenkürzel. F11 schaltet das aktive Gerätefenster (ohne eines das
  Hauptfenster) in den Vollbildmodus und zurück.
- **Einstellungen** (Werkzeuge › Einstellungen, Strg+,): Raster und Rasterweite, Einrasten,
  Minimap, Tempo beim Start, Bewegung (wie das System, reduziert, voll), Sprache (wie das
  System, Deutsch, English), automatisches Sichern (Sekunden, 0 aus), Startseite beim Start
  anzeigen. Sie liegen in
  `config://settings.ini`.
- **Automatische Sicherung:** ungespeicherte Änderungen landen alle 120 s (einstellbar) und
  sofort beim Ruhezustand oder Abmelden in `data://autosave/`; nach einem Absturz bietet die
  nächste Sitzung sie in einer Leiste über der Zeichenfläche an (Wiederherstellen, Verwerfen,
  Später). Jede laufende Sitzung hält eine Sperre, ein zweites Fenster bietet ihre Kopie nicht an;
  eine wiederhergestellte Kopie bleibt, bis die neue Sitzung ihre eigene geschrieben hat. Wer
  beim Beenden „Verwerfen“ wählt, verwirft auch die Kopie.
- **Barrierefreiheit:** Geräte und Kabel sind für Screenreader Listeneinträge („Kabel PC1 eth0 –
  SW1 Gi1/0/1, Kupfer, gerade, aktiv“), die Leinwand heißt „Topologie“; die Farben der
  Pakete und Kabel halten in beiden Themes und im Hochkontrast mindestens 3:1 gegen die Fläche
  (Selbsttest). Die meisten Kabelarten unterscheiden sich auch in der Linie (gekreuzt
  gestrichelt, Singlemode Strich-Punkt, Konsole und Funk gepunktet, seriell mit Blitz, WAN doppelt,
  Telefon Strich-Punkt-Punkt, Koax als Röhre, DAC mit Querstrichen, AOC dick gestrichelt), Pakete nennen
  Inhalt und Weg für Screenreader und in der Ereignisliste; „reduzierte Bewegung“ lässt Pakete
  springen statt gleiten.
