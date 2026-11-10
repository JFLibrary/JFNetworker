// docs -- "Erklärung öffnen": the explanation of a device or a cable on networker.jfreund.org/docs
//
//   page_of(target) -> Option<String>   the address of its page for this version of the app, in
//                                       the window's language (None: no page yet)
//   context(n, target)                  a palette tile's context menu is about this model or cable
//   states(n, ui)                       CMD_EXPLAIN enabled when the thing in question has a page
//   command(n, ui, id) -> handled       CMD_EXPLAIN asks first; CMD_EXPLAIN_GO opens the browser
//   dialog(n, ui)                       the question, with the address
//
// The context menus of a device and a cable on the canvas and of a palette tile have the entry
// (with the arrow of JN_EXPLAIN).  Nothing leaves the program before the question is answered;
// PERMISSIONS allows the browser for https.
//
// Versions: a page's address names one version (/docs/devices/jf-s24p/v1), and the website never
// changes a published version, so this app's links keep showing what they showed when it was
// built.  PAGES lists what this version links; the website's list is
// jfreundWebsite web/content/docs/networker/versions.ts (a link here must be a version there).

use alloc::format;
use alloc::string::String;

use jfn_model::cable::CableKind;
use ui::*;

use crate::app::constants::*;
use crate::app::tr::{t, tf};
use crate::app::Net;

/// the website of the app
pub(crate) const SITE: &str = "https://networker.jfreund.org";

/// the pages this version of the app links: (devices or cables, the model's or cable's id, the
/// version) -- written by tools/docs_pages.py from the website's page.json files
pub(crate) const PAGES: &[(&str, &str, &str)] = &[
    ("devices", "bridge", "1"),
    ("devices", "cable-modem", "2"),
    ("devices", "cache", "2"),
    ("devices", "cloud", "2"),
    ("devices", "container-host", "2"),
    ("devices", "database", "2"),
    ("devices", "dsl-modem", "2"),
    ("devices", "home-router", "2"),
    ("devices", "hub", "1"),
    ("devices", "iot-camera", "1"),
    ("devices", "iot-sensor", "1"),
    ("devices", "ip-phone", "2"),
    ("devices", "jf-ap6e", "1"),
    ("devices", "jf-ap7", "1"),
    ("devices", "jf-fw", "1"),
    ("devices", "jf-fw2", "1"),
    ("devices", "jf-fw8", "1"),
    ("devices", "jf-r2", "2"),
    ("devices", "jf-r4", "2"),
    ("devices", "jf-r8", "1"),
    ("devices", "jf-s24", "1"),
    ("devices", "jf-s24l3", "1"),
    ("devices", "jf-s24m", "1"),
    ("devices", "jf-s24p", "1.1"),
    ("devices", "jf-s32c", "1"),
    ("devices", "jf-s48p", "1"),
    ("devices", "jf-s48x", "1"),
    ("devices", "jf-s48y", "1"),
    ("devices", "jf-s8", "1"),
    ("devices", "jf-s8p", "1"),
    ("devices", "laptop", "1"),
    ("devices", "load-balancer", "2"),
    ("devices", "modem-ont", "2"),
    ("devices", "nas", "2"),
    ("devices", "pc", "2"),
    ("devices", "printer", "1"),
    ("devices", "queue", "2"),
    ("devices", "repeater", "1"),
    ("devices", "server-1u", "1"),
    ("devices", "server-1u-sfp", "1"),
    ("devices", "server-l", "1"),
    ("devices", "server-m", "1"),
    ("devices", "server-s", "1"),
    ("devices", "server-tower", "2"),
    ("devices", "server-xl", "1"),
    ("devices", "smart-tv", "1"),
    ("devices", "smartphone", "1"),
    ("devices", "tablet", "1"),
    ("devices", "traffic-gen", "2"),
    ("cables", "aoc", "1"),
    ("cables", "coax", "2"),
    ("cables", "console", "2"),
    ("cables", "copper", "1"),
    ("cables", "copper-cross", "2"),
    ("cables", "dac", "1"),
    ("cables", "fiber-mm", "1"),
    ("cables", "fiber-sm", "1"),
    ("cables", "phone", "2"),
    ("cables", "serial", "2"),
    ("cables", "serial-dce", "2"),
    ("cables", "serial-dte", "2"),
    ("cables", "wan", "2"),
    ("cables", "wireless", "1"),
];

/// what an explanation is about
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Target {
    /// a model of the catalogue, by its id ("jf-s24p")
    Model(&'static str),
    Cable(CableKind),
}

#[derive(Default)]
pub(crate) struct Docs {
    /// the question is open
    pub ask: Shared<i32>,
    /// what it asks about: the shown name and the address
    pub name: String,
    pub url: String,
    /// a palette tile's context menu is open about this (else the canvas' selection counts)
    pub ctx: Option<Target>,
}

impl Docs {
    pub(crate) fn new() -> Docs {
        Docs::default()
    }
}

/// the address of a target's page for this version of the app, in the window's language
pub(crate) fn page_of(target: Target) -> Option<String> {
    page_in(target, crate::app::tr::en())
}

/// ... in English (en) or German
pub(crate) fn page_in(target: Target, en: bool) -> Option<String> {
    let (kind, id) = match target {
        Target::Model(id) => ("devices", id),
        Target::Cable(k) => ("cables", k.as_str()),
    };
    let (_, _, v) = PAGES.iter().find(|p| p.0 == kind && p.1 == id)?;
    Some(format!("{}{}/docs/{}/{}/v{}", SITE, if en { "" } else { "/de" }, kind, id, v))
}

/// the name a target is shown with ("JF-S24P", "Kupfer, gerade")
fn name_of(target: Target) -> String {
    match target {
        Target::Model(id) => String::from(jfn_model::catalog::model(id).map_or(id, |m| m.name)),
        Target::Cable(k) => String::from(t(k.title())),
    }
}

/// what the canvas' selection is: one device or one cable
fn selected(n: &Net) -> Option<Target> {
    match crate::workspace::canvas::single_item(n)? {
        crate::doc::edits::Item::Device(d) => n.doc.proj.device(d).and_then(|d| jfn_model::catalog::model(&d.model)).map(|m| Target::Model(m.id)),
        crate::doc::edits::Item::Link(l) => n.doc.proj.link(l).map(|l| Target::Cable(l.cable)),
        crate::doc::edits::Item::Note(_) => None,
    }
}

/// what CMD_EXPLAIN is about now
fn current(n: &Net) -> Option<Target> {
    n.docs.ctx.or_else(|| selected(n))
}

/// a palette tile's context menu opens about this (None: back to the canvas' selection)
pub(crate) fn context(n: &mut Net, target: Option<Target>) {
    n.docs.ctx = target;
}

pub(crate) fn states(n: &Net, ui: &mut Ui) {
    let on = current(n).and_then(page_of).is_some();
    ui.cmd_state(CMD_EXPLAIN, if on { 0 } else { CF_DISABLED });
}

pub(crate) fn command(n: &mut Net, ui: &mut Ui, id: u32) -> bool {
    match id {
        CMD_EXPLAIN => {
            let target = current(n);
            n.docs.ctx = None;
            let Some(target) = target else { return true };
            let Some(url) = page_of(target) else { return true };
            n.docs.name = name_of(target);
            n.docs.url = url;
            n.docs.ask.set(1);
        }
        CMD_EXPLAIN_GO => {
            let url = n.docs.url.clone();
            if ui.open_url(&url) != 0 {
                n.file.msg = Some((IB_WARNING, String::from(t("Der Browser ließ sich nicht öffnen")), url));
            }
        }
        _ => return false,
    }
    ui.repass();
    true
}

/// "Erklärung im Browser öffnen?": the program is left for the browser only on the answer
pub(crate) fn dialog(n: &mut Net, ui: &mut Ui) {
    let open = n.docs.ask.clone();
    if ui.dialog_begin(ID_DOCS_DLG, t("Erklärung im Browser öffnen?"), &open) {
        ui.next_size(460, 0);
        ui.text(TS_BODY | TX_WRAP, &tf("Die Erklärung zu {} steht auf der Website von JFNetworker. Dafür verlassen Sie das Programm und wechseln in Ihren Browser:", &[&n.docs.name]));
        ui.next_size(460, 0);
        ui.text(TS_BODY_STRONG | TX_WRAP, &n.docs.url);
        if ui.dialog_buttons(Some(t("Im Browser öffnen")), None, Some(t("Abbrechen"))) == DLG_PRIMARY {
            ui.cmd_run(CMD_EXPLAIN_GO, 0);
        }
    }
    ui.dialog_end();
}
