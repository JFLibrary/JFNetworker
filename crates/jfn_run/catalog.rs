// catalog -- `jfn-sim catalog`: the device models and cable kinds as JSON, for the documentation
//
//   catalog(args) -> exit code     JSON on stdout, or into the file after --out
//
// The website's docs (networker.jfreund.org/docs) take their facts from here instead of copying
// them by hand: every model with its category, symbol, command line, features, port groups and
// the hardware the simulator assumes; every cable kind with its grades and the reach the
// simulator allows per speed (phys::reach_graded).  Texts stay German, as the catalogue has them;
// the website's generator looks up the English rows in src/locales/*.tsv.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use jfn_model::cable::{CableGrade, CableKind};
use jfn_model::catalog::{self, CliStyle, Media, Model, Optic, Poe, K100G, K10G, K1G, K25G};

use crate::{complain, say};

/// a JSON string literal
fn js(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

fn media_id(m: Media) -> &'static str {
    match m {
        Media::Rj45 => "rj45",
        Media::Sfp => "sfp",
        Media::SfpPlus => "sfp+",
        Media::Sfp28 => "sfp28",
        Media::Qsfp28 => "qsfp28",
        Media::Serial => "serial",
        Media::Console => "console",
        Media::Usb => "usb",
        Media::RadioAp => "radio-ap",
        Media::RadioClient => "radio",
        Media::Wan => "wan",
        Media::Rj11 => "rj11",
        Media::Coax => "coax",
    }
}

fn poe_id(p: Poe) -> &'static str {
    match p {
        Poe::None => "",
        Poe::Pse => "pse",
        Poe::PseBt => "pse-bt",
        Poe::Pd => "pd",
    }
}

fn features(m: &Model) -> String {
    let names = [
        (catalog::F_HOST, "host"),
        (catalog::F_BRIDGE, "bridge"),
        (catalog::F_ROUTER, "router"),
        (catalog::F_FIREWALL, "firewall"),
        (catalog::F_AP, "ap"),
        (catalog::F_HUB, "hub"),
        (catalog::F_CLOUD, "cloud"),
        (catalog::F_NAT, "nat"),
        (catalog::F_ANNOTATION, "note"),
    ];
    let v: Vec<String> = names.iter().filter(|(f, _)| m.has(*f)).map(|(_, n)| js(n)).collect();
    format!("[{}]", v.join(", "))
}

fn model_json(m: &Model) -> String {
    let groups: Vec<String> = m
        .groups
        .iter()
        .map(|g| {
            // (an unnumbered port has the name alone: first is u16::MAX)
            let first = if g.first == u16::MAX { String::from("null") } else { format!("{}", g.first) };
            format!(
                "{{\"prefix\": {}, \"first\": {}, \"count\": {}, \"media\": {}, \"kbps\": {}, \"mdix\": {}, \"poe\": {}}}",
                js(g.prefix),
                first,
                g.count,
                js(media_id(g.media)),
                g.kbps,
                g.mdix,
                js(poe_id(g.poe))
            )
        })
        .collect();
    let cli = match m.cli {
        CliStyle::Ios => "ios",
        CliStyle::Linux => "linux",
        CliStyle::None => "",
    };
    let hw = match catalog::hw_of(m.id) {
        Some(h) => format!(
            "{{\"cores\": {}, \"speed_pm\": {}, \"ram_mb\": {}, \"swap_mb\": {}, \"disk\": {}, \"disk_gb\": {}}}",
            h.cores,
            h.speed_pm,
            h.ram_mb,
            h.swap_mb,
            js(h.disk.as_str()),
            h.disk_gb
        ),
        None => String::from("null"),
    };
    format!(
        "    {{\"id\": {}, \"name\": {}, \"kind\": {}, \"category\": {}, \"category_title\": {}, \"title\": {}, \"symbol\": {}, \"cli\": {}, \"features\": {}, \"summary\": {}, \"groups\": [{}], \"hw\": {}}}",
        js(m.id),
        js(m.name),
        js(m.kind.as_str()),
        js(&format!("{:?}", m.category)),
        js(m.category.title()),
        js(m.title),
        js(m.symbol),
        js(cli),
        features(m),
        js(&m.summary()),
        groups.join(", "),
        hw
    )
}

fn cable_json(k: CableKind) -> String {
    // the speeds the docs compare (limits_msg's choice), each with its reach per grade
    let speeds: &[u32] = match k {
        CableKind::CopperStraight | CableKind::CopperCross => &[100_000, K1G, 2_500_000, 5_000_000, K10G, K25G],
        CableKind::FiberMm => &[K1G, K10G, K25G, K100G],
        CableKind::FiberSm => &[K1G, K10G, K25G, K100G],
        CableKind::Serial | CableKind::SerialDce | CableKind::SerialDte => &[64, 2048],
        CableKind::Console => &[115],
        CableKind::Phone => &[100_000],
        CableKind::Coax => &[K1G],
        CableKind::Dac | CableKind::Aoc => &[K10G, K25G, K100G],
        CableKind::Wireless | CableKind::Wan => &[],
    };
    let grades: Vec<CableGrade> = CableGrade::ALL.iter().copied().filter(|g| *g == CableGrade::Unset || g.fits(k)).collect();
    let optic = if k == CableKind::FiberSm { Some(Optic::Lr) } else { None };
    let mut reach = Vec::new();
    for g in &grades {
        for &s in speeds {
            if let Some(r) = jfn_model::phys::reach_graded(k, *g, s, optic) {
                reach.push(format!("{{\"grade\": {}, \"kbps\": {}, \"max_m\": {}, \"std\": {}}}", js(g.as_str()), s, r.max_m, js(r.std)));
            }
        }
    }
    let gl: Vec<String> = grades.iter().filter(|g| **g != CableGrade::Unset).map(|g| js(g.as_str())).collect();
    format!(
        "    {{\"id\": {}, \"title\": {}, \"tile\": {}, \"copper\": {}, \"grades\": [{}], \"limits\": {}, \"reach\": [{}]}}",
        js(k.as_str()),
        js(k.title()),
        js(k.tile()),
        k.is_copper(),
        gl.join(", "),
        js(&jfn_model::cable::limits_msg(k, CableGrade::Unset).text()),
        reach.join(", ")
    )
}

/// jfn-sim catalog [--out FILE]
pub fn catalog(args: &[String]) -> i32 {
    let models: Vec<String> = catalog::catalog().iter().map(model_json).collect();
    let cables: Vec<String> = CableKind::ALL.iter().map(|k| cable_json(*k)).collect();
    let text = format!("{{\n  \"format\": \"jfn-catalog\",\n  \"version\": 1,\n  \"models\": [\n{}\n  ],\n  \"cables\": [\n{}\n  ]\n}}\n", models.join(",\n"), cables.join(",\n"));
    match args.iter().position(|a| a == "--out").and_then(|i| args.get(i + 1)) {
        Some(f) => {
            if !plat::write_file(f, text.as_bytes()) {
                complain(&format!("{} lässt sich nicht schreiben.\n", f));
                return 2;
            }
            say(&format!("{}: {} Modelle, {} Kabelarten\n", f, models.len(), cables.len()));
        }
        None => say(&text),
    }
    0
}
