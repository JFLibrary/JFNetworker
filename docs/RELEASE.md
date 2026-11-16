# Releases von JFNetworker

Stand 2026-11-16: Plan, Probelauf der Pakete (Abschnitt „Probelauf“) und der Workflow
`.github/workflows/release.yml` mit `tools/release/` sind da. Ein erster Lauf auf GitHub (ohne Tag,
ohne Schlüssel) war grün; veröffentlicht ist noch nichts.

## Ziel

Ein Tag `vX.Y.Z` auf `main` genügt: GitHub Actions baut alle Pakete aus genau diesem Stand, signiert
sie, prüft sie und legt einen **Entwurf** des Releases auf GitHub an. Der Entwurf wird von Hand
durchgesehen und veröffentlicht. Danach zeigt networker.jfreund.org die Downloads.

Grundsätze:

- **Bauen und signieren in der CI**, nicht lokal: jeder Release kommt aus einem sauberen Runner mit
  `Cargo.lock` und lässt sich einem Commit und einem Log zuordnen. Lokal bleibt nur der Notweg.
- **Nichts geht automatisch nach draußen**: Die Schlüssel liegen in einem GitHub-Environment
  `release`, das auf die Freigabe wartet; der Release bleibt ein Entwurf, bis er veröffentlicht wird.
- **Jede Datei ist prüfbar**: `SHA256SUMS` mit GPG-Signatur und eine Herkunfts-Attestierung von GitHub
  (`gh attestation verify DATEI --repo JFLibrary/JFNetworker`), auch für die unsignierte `.exe`.

## Was ein Release enthält

| Datei | System | Signatur | GitHub | Website |
|---|---|---|---|---|
| `JFNetworker_Setup.exe` | Windows x64 | vorerst keine (kein Zertifikat) | ja | nein |
| `jfnetworker_X.Y.Z_amd64.deb` | Debian, Ubuntu | über `SHA256SUMS.asc` (apt prüft nur Paketquellen) | ja | ja |
| `JFNetworker-X.Y.Z-netlab-x86_64.AppImage` | Linux x64 | GPG (Typ-2-Signatur, `appimagetool --sign`-Format) | ja | ja |
| `JFNetworker-X.Y.Z-universal.dmg` | macOS 11+, Apple Silicon und Intel | Developer ID, notarisiert, Ticket geheftet | ja | ja |
| `jfn-sim-X.Y.Z-<system>.zip` / `.tar.gz` | Windows, Linux, macOS | über `SHA256SUMS.asc` | ja | nein |
| `SHA256SUMS`, `SHA256SUMS.asc` | – | GPG | ja | ja |

`jfn-sim` (der Runner ohne Fenster) kommt in eigene Archive, solange die Pakete nur das Programm
selbst tragen (Lücke 2).

## Der Ablauf

1. **Vorbereiten** (auf `main`): `CHANGELOG.md` unter `[Unreleased]` schreiben, dann
   `python tools/release/release.py prepare X.Y.Z` (macht daraus `## [X.Y.Z] - Datum` und hebt die
   Version in `PACKAGE`, `Cargo.toml`, `Cargo.lock`); volle Suite auf Windows und Linux wie bei jedem Push.
2. **Tag** `vX.Y.Z` setzen und pushen.
3. **`.github/workflows/release.yml`** (bei Tags `v*`; „Run workflow“ ist ein Probelauf ohne Release):
   - `check`: `release.py check vX.Y.Z`: Tag = `PACKAGE` `version` = `Cargo.toml` `version`, der Abschnitt im CHANGELOG existiert.
   - `windows` (windows-latest): `cargo jflib package --formats setup`; jfn-sim als `.zip`.
   - `linux` (ubuntu-latest): `cargo jflib package --formats deb,appimage`; jfn-sim als `.tar.gz`;
     AppImage mit GPG signiert.
   - `macos` (macos-latest, Apple Silicon): `cargo jflib package --formats dmg` mit
     `jfgui_MAC_IDENTITY` und `jfgui_MAC_NOTARY`; jfn-sim als universelles `.tar.gz`.
   - **Rauchtest je Paket**: Setup im Testmodus des Installers (`ASMINST_SHELL_DIR`, `--script
     tools/release/inst.uis`, `--dir`: keine Registry, kein Startmenü) installieren, `--selftest --only=l3`,
     deinstallieren, der Ordner muss leer sein (Windows); `.deb` mit apt installieren, starten, entfernen,
     AppImage mit `--appimage-extract-and-run --selftest --only=l3` (Linux); `.dmg` einhängen,
     `spctl -a` muss „accepted, source=Notarized Developer ID“ sagen, Selbsttest nativ und unter Rosetta (macOS).
   - `publish`: `SHA256SUMS` + GPG-Signatur, `actions/attest-build-provenance` für jede Datei,
     Entwurf auf GitHub mit dem CHANGELOG-Abschnitt (`release.py notes`: Download-Tabelle, fester
     Hinweis zur unsignierten `.exe` mit SmartScreen, Prüfsumme und Attestierung).
   - Alle Jobs mit Schlüsseln laufen im Environment `release`: ohne Secrets bauen sie unsigniert und
     sagen im Log, was fehlt; mit dir als nötigem Prüfer wartet jeder Lauf auf deine Freigabe.
4. **Durchsehen und veröffentlichen** auf GitHub.
5. **Website**: Die Download-Seite holt beim Deploy den neuesten Release über die GitHub-API
   (Dateinamen, Größen, Prüfsummen) und verlinkt die Dateien auf GitHub; `deploy.py` wie gehabt.

## Schlüssel (Secrets im Environment `release`)

| Secret | Wofür | Woher |
|---|---|---|
| `MAC_CERT_P12`, `MAC_CERT_PASSWORD` | Developer ID Application, Base64 | Export aus dem Schlüsselbund des Mac mini (vom Nutzer) |
| `NOTARY_KEY_P8`, `NOTARY_KEY_ID`, `NOTARY_ISSUER` | `notarytool` mit API-Key | App Store Connect › Benutzer und Zugriff › Integrationen (vom Nutzer) |
| `GPG_SIGNING_KEY`, `GPG_PASSPHRASE` | AppImage, `SHA256SUMS` | ein neuer Release-Schlüssel: Hauptschlüssel offline, nur der Signier-Unterschlüssel als Secret |

In der CI legt der macOS-Job einen Wegwerf-Schlüsselbund an, importiert das Zertifikat und speichert
das notarytool-Profil darin (`xcrun notarytool store-credentials`); danach wird er gelöscht. Der
öffentliche GPG-Schlüssel kommt auf die Website und in die README.

Später: Windows über einen Signier-Dienst in der Cloud (SignPath Foundation für Open Source oder
Azure Trusted Signing); `tools/codesign.py` / cargo-jflib bekommt dafür einen weiteren Signierer.
Dann darf die `.exe` auch auf die Website. Danach ein Update-Kanal (`--update`, `update.json` mit
Ed25519-Signatur), damit JFNetworker sich selbst aktualisiert.

## Was du einrichten musst (einmal)

1. **Environment** `release` anlegen (GitHub › Settings › Environments), dich als *Required
   reviewer*, Deployment-Branches/Tags auf `main` und `v*` beschränken.
2. **Developer ID** (Mac mini, Schlüsselbundverwaltung): „Developer ID Application: … (6RBCBNA6DJ)“
   samt privatem Schlüssel als `.p12` mit Passwort exportieren, `base64 -i cert.p12 | pbcopy` als
   `MAC_CERT_P12`, das Passwort als `MAC_CERT_PASSWORD`.
3. **Notarisierung**: App Store Connect › Benutzer und Zugriff › Integrationen › API-Schlüssel mit der
   Rolle *Developer*; den Inhalt der `.p8` als `NOTARY_KEY_P8`, Key-ID und Issuer-ID als
   `NOTARY_KEY_ID` und `NOTARY_ISSUER`.
4. **GPG-Release-Schlüssel**: ein Hauptschlüssel, der offline bleibt (z. B. auf einem USB-Stick), mit
   einem Signier-Unterschlüssel; nur den Unterschlüssel exportieren
   (`gpg --armor --export-secret-subkeys ID!`) als `GPG_SIGNING_KEY`, sein Passwort als
   `GPG_PASSPHRASE`. Den öffentlichen Schlüssel veröffentlichen wir auf der Website.
5. **Herausgeber** entscheiden (unten).

Ich fasse dabei keine Passwörter oder Schlüssel an; das Eintragen der Secrets machst du.

## Lücken

| | Lücke | Stand |
|---|---|---|
| 1 | `cargo jflib package` verlangt die Fassade `jflib` als Abhängigkeit („JFNetworker does not depend on the crate jflib“); cargo-jflib liest aus ihr nur die JFLib-Version für Installer und AppImage-Laufzeit, die Fassade zöge alle 52 JFLib-Crates in jeden Build | Übergang: `tools/release/facade.py` setzt sie im Workflow nur zum Paketieren ein (das Programm nutzt sie nicht, die Datei bleibt gleich). Richtig: cargo-jflib nimmt die Version auch von `jflib-rt` (JFLib, nächstes Release) |
| 2 | `jfn-sim` steht nicht in den Paketen: cargo-jflib 0.2.1 kennt keine weiteren Programme (`win_files`/`linux_files` liegen auf dem JFLib-Branch `cargo-jflib-files`) | Übergang: eigene Archive je System im Release |
| 3 | `publisher=JFLib` steht im Setup (Programme und Features) und als `Maintainer: JFLib <nobody@localhost>` im `.deb` | offen: deine Entscheidung; `homepage` zeigt jetzt auf networker.jfreund.org |
| 4 | kein `CHANGELOG.md` | erledigt: `CHANGELOG.md` mit dem Inhalt von 0.1.0 unter `[Unreleased]` |
| 5 | macOS-Warnung (ungenutzter Import in `src/simmodetest.rs`) | erledigt (`cargo check --target aarch64-apple-darwin` ohne Warnung) |
| 6 | `ci.yml` holte noch „JFLib beside this repository“ | erledigt |
| 7 | Download-Seite auf networker.jfreund.org | offen: erst den Inhalt abstimmen (Dateien, Hinweise, Systemvoraussetzungen), dann bauen |
| 8 | der Workflow lief noch nie auf GitHub | erledigt: Probelauf ohne Schlüssel grün (unten); mit den Secrets noch einmal, dann der Tag |
| 9 | Actions auf Node.js 20 (`cache@v4`, `upload-artifact@v4`, `download-artifact@v4`, `attest-build-provenance@v2`) | erledigt: `cache@v6`, `upload-artifact@v7`, `download-artifact@v8`, `attest-build-provenance@v4` |
| 10 | die macOS-Runner haben kein Rosetta: der Intel-Teil des .dmg lief dort nicht | erledigt: der Rauchtest installiert Rosetta vorher (`softwareupdate --install-rosetta`), der Intel-Teil besteht den Selbsttest (zweiter Lauf) |
| 11 | `ubuntu-latest` ist seit Oktober 2026 Ubuntu 26.04: eine neuere glibc hätte die Pakete für ältere Systeme unbrauchbar machen können | kein Problem: `JFNetworker-netlab` und `jfn-sim` verlangen nur `GLIBC_2.2.5` (JFLib bindet die alten Symbolversionen), die AppImage-Laufzeit ist statisch |

## Probelauf (2026-11-16)

Lokal, ohne Signaturen, mit der Fassade nur für diesen Lauf (Lücke 1):

| Paket | Wo gebaut | Ergebnis |
|---|---|---|
| `JFNetworker-0.1.0-universal.dmg` | Mac mini, `cargo jflib package --formats dmg` (12 min) | 30,8 MB; arm64 + x86_64; Bundle-ID `org.jfreund.JFNetworker`, Version 0.1.0; ad hoc signiert, Gatekeeper lehnt ab (erwartet ohne Developer ID); Selbsttest `l3` nativ und unter Rosetta 163/163 |
| `JFNetworker_Setup.exe` | Windows, `cargo jflib package --formats setup,deb,appimage` (16 min mit dem Release-Build) | 19,5 MB, ein Style (netlab); im Testmodus des Installers installiert (Registry nur in `registry.txt`: `DisplayName=JFNetworker`, `DisplayVersion=0.1.0`, `Publisher=JFLib`), Selbsttest `l3` 163/163, deinstalliert ohne Rest |
| `jfnetworker_0.1.0_amd64.deb` | derselbe Lauf (das .deb macht cargo-jflib selbst, ohne WSL) | 15,3 MB; `/usr/lib/jfnetworker/JFNetworker-netlab`, `.desktop`-Datei, Lizenzen; das Programm daraus besteht den Selbsttest `l3` 163/163 (WSL); `Maintainer: JFLib <nobody@localhost>` (Lücke 3) |
| `JFNetworker-0.1.0-netlab-x86_64.AppImage` | derselbe Lauf | 19,6 MB; `--appimage-extract-and-run --selftest --only=l3` 163/163 (WSL) |

### Auf GitHub (2026-11-16, Lauf 43899801757, „Run workflow“ auf 765387e, ohne Secrets)

Alle fünf Jobs grün: `check` 5 s, Windows 21 min, Linux 12 min, macOS 28 min, `publish` 17 s.
Rauchtests: Setup im Testmodus installiert und restlos deinstalliert, `.deb` über apt installiert und
entfernt, AppImage entpackt, `.dmg` eingehängt (x86_64 + arm64); der Selbsttest `l3` 163/163 überall,
der Intel-Teil auf dem Mac nicht (kein Rosetta, Lücke 10). Jede Datei hat ihre Attestierung, und
die Logs nennen, was nicht signiert ist (Authenticode, GPG, das .dmg nur ad hoc). Die Dateien:
`JFNetworker_Setup.exe`, `jfnetworker_0.1.0_amd64.deb`, `JFNetworker-0.1.0-netlab-x86_64.AppImage`,
`JFNetworker-0.1.0-universal.dmg`, `jfn-sim-0.1.0-windows-x86_64.zip`, `jfn-sim-0.1.0-linux-x86_64.tar.gz`,
`jfn-sim-0.1.0-macos-universal.tar.gz`, `SHA256SUMS`.

### Zweiter Lauf (2026-11-16, Lauf 43949172206 auf ac2073c, ohne Secrets)

Alle fünf Jobs grün: Windows 25 min, Linux 9 min, macOS 19 min, `publish` 19 s. Keine Warnung mehr
über Node.js 20 (Lücke 9). Auf dem Mac besteht jetzt auch der Intel-Teil unter Rosetta den
Selbsttest `l3` mit 163/163 (Lücke 10). Die heruntergeladenen Dateien passen zu `SHA256SUMS`. Die
Linux-Programme brauchen nur `GLIBC_2.2.5`, obwohl sie auf Ubuntu 26.04 gebaut sind (Lücke 11).

## Entscheidungen, die noch offen sind

- Herausgeber im Setup und im `.deb` (`publisher`).
- Wann der Windows-Signier-Dienst beantragt wird.
- Ob die Linux-Pakete später auch über eine eigene apt-Quelle und Flatpak kommen.
