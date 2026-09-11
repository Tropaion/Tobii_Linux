//! `tobii-gtk` — GTK4 configuration GUI for the Tobii ET5 Linux runtime.
//!
//! A styled hub window (status + live eye-position view) over the device
//! thread, from which the guided display-setup and calibration flows, the
//! gaze-preview overlay, and the select-eyes control are driven.

pub mod accuracy;
pub mod align;
pub mod autostart;
pub mod calibrate_flow;
pub mod calibration_area;
pub mod device;
pub mod eye_preview;
pub mod eyeview;
pub mod focus;
pub mod games;
pub mod head_model;
pub mod outputs;
pub mod overlay;
pub mod particles;
pub mod screen_pick;
pub mod setup_flow;
pub mod tray;
pub mod update;
pub mod widget;

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::time::Duration;

use gtk::glib;
use gtk::prelude::*;
use gtk::{
    Align, Application, ApplicationWindow, CheckButton, DrawingArea, Label, Orientation, Switch,
};

use tobii_protocol::EnabledEye;

const APP_ID: &str = "com.tobiilinux.Configuration";

/// The stylesheet.
///
/// This is a control panel for a measuring instrument, and it is styled as one
/// rather than as a settings app: the live trackbox is the subject, everything
/// else is a quiet rack of controls beside it.
///
/// Three rules carry that:
///
/// * **Two surfaces, not one.** A darker ground with cards raised on it, so
///   grouping is visible without boxes-inside-boxes. The old sheet painted
///   everything one flat colour and leaned on headings alone.
/// * **One accent, spent once per view.** Teal marks the action that commits;
///   everything else is a quiet surface button. Previously every button was
///   accent-filled, which made five equal settings look like five urgent calls
///   to action.
/// * **Uppercase micro-labels and monospace values.** The panel reports
///   millimetres and degrees; labelling it the way instruments are labelled is
///   truer to the subject than sentence-case captions, and monospace digits
///   stop live values from twitching as they change width.
const CSS: &str = "
/* --- ground and surfaces ------------------------------------------------ */
window { background-color: #0d1013; color: #e8ecef; }
.surface { background-color: #161a1f; border: 1px solid #232a32; border-radius: 12px; }
.panel-pad { padding: 16px; }
.hairline { background-color: #232a32; min-height: 1px; }

/* --- type --------------------------------------------------------------- */
.app-title { font-size: 20px; font-weight: bold; letter-spacing: 0.01em; }
.eyebrow { font-size: 10px; font-weight: bold; letter-spacing: 0.16em;
           color: #79838d; }
.section-title { font-size: 14px; font-weight: bold; }
.section-desc { font-size: 12px; color: #8a949d; }
.section-warn { color: #f2b134; font-weight: bold; }
.status { font-size: 12px; color: #8a949d; }
.guidance { font-size: 14px; }
.hint { font-size: 11px; color: #6f7982; }

/* --- buttons ------------------------------------------------------------ */
/* Base is QUIET. The hub is five co-equal settings; filling every one of them
   with the accent made them shout over each other and over the instrument. */
button { background-image: none; background-color: #1e242b; color: #dfe5ea;
         border: 1px solid #2b333c; border-radius: 9px; padding: 9px 16px; }
button:hover { background-color: #262e37; border-color: #37414c; }
button:active { background-color: #2c3540; }
button:disabled { background-color: #171b20; color: #5b646c; border-color: #232a32; }
button:checked { background-color: #14696b; border-color: #1f9ea0; color: #ffffff; }
/* The accent, for the one action in a view that commits. */
button.primary, button.suggested {
    background-color: #1f9ea0; color: #ffffff; border-color: #1f9ea0; }
button.primary:hover, button.suggested:hover {
    background-color: #27b4b6; border-color: #27b4b6; }
button.primary:disabled, button.suggested:disabled {
    background-color: #1a3f42; border-color: #1a3f42; color: #6d8b8c; }
/* Tertiary: outlined, no fill. Rarely the point, but still a button — an
   invisible border that only appears on hover leaves it looking like a link
   until you happen to touch it. */
button.quiet { background-color: transparent; border-color: #2b333c;
               color: #8a949d; padding: 8px 12px; }
button.quiet:hover { background-color: #1e242b; border-color: #3a444f;
                     color: #e8ecef; }
button.spin-btn { min-width: 26px; padding: 2px 10px; }
/* The header cogwheel. Square, borderless until touched: it sits beside the
   connection status, where a filled button would read as an action to take. */
/* Also the save/copy pair in the settings popover, which are plain buttons —
   hence both selectors: a GtkMenuButton wraps its own button node, a
   GtkButton is one. */
/* Outlined, not borderless. The same lesson as `button.quiet` above: a border
   that only appears on hover leaves the control looking like decoration until
   you happen to touch it, and these two have no caption to give it away. */
menubutton.icon-btn > button, button.icon-btn {
    min-width: 30px; min-height: 30px; padding: 4px;
    background-color: transparent; border-color: #2b333c; color: #8a949d; }
menubutton.icon-btn > button:hover, button.icon-btn:hover {
    background-color: #1e242b; border-color: #3a444f; color: #e8ecef; }
menubutton.icon-btn > button:checked, button.icon-btn:active {
    background-color: #1e242b; border-color: #3a444f; color: #e8ecef; }

/* --- the settings popover ----------------------------------------------- */
/* GTK draws a popover on its own surface, outside `window`, so it inherits
   none of the ground colour above and would otherwise arrive in the system
   theme's light grey in the middle of a dark hub. */
popover > contents { background-color: #161a1f; color: #e8ecef;
                     border: 1px solid #232a32; border-radius: 12px;
                     padding: 4px; box-shadow: 0 6px 20px rgba(0,0,0,0.45); }
popover > arrow { background-color: #161a1f; border: 1px solid #232a32; }
button.help-btn { min-width: 22px; padding: 0 8px; background-color: transparent;
                  border-color: transparent; color: #8a949d; font-size: 12px; }
button.help-btn:hover { background-color: #1e242b; color: #e8ecef; }
.spin-entry { padding: 2px 6px; }
.overlay-window { background-color: transparent; }
/* The scrollbar's slider needs an explicit floor. The theme sizes it with a
   negative margin over a small min-size, and against this sheet that computes
   to -2, which GTK reports once per scrollbar as
   \"GtkGizmo (slider) reported min width -2\". Stating the minimum here is the
   fix; it is not a cosmetic choice. */
scrollbar slider { min-width: 8px; min-height: 8px; }

/* --- dialogs ------------------------------------------------------------ */
.dialog-heading { font-size: 19px; font-weight: bold; }
.dialog-lead { font-size: 14px; color: #c9d1d8; }
.dialog-terms { font-size: 13px; color: #8a949d; }
.dialog-facts { font-size: 12px; color: #79838d; border-top: 1px solid #232a32;
                padding-top: 10px; margin-top: 2px; }
.dialog-note { font-size: 12px; color: #79838d; border-top: 1px solid #232a32;
               padding-top: 10px; margin-top: 2px; }
.dialog-url { font-size: 11px; }
.cal-fail-heading { font-size: 26px; font-weight: bold; }
.cal-fail-tips { font-size: 14px; color: #8a949d; }
.cal-fail-detail { font-size: 12px; color: #6f7982; font-style: italic; }
.cal-success-heading { font-size: 32px; font-weight: bold; }

/* --- the live readout ---------------------------------------------------- */
/* Monospace on the VALUES only: they change every frame, and a proportional
   font makes the column width breathe as digits change, which reads as the
   number twitching rather than the value moving. */
.readout-name { font-size: 11px; color: #79838d; letter-spacing: 0.05em; }
.readout-value { font-size: 13px; color: #e8ecef; font-family: monospace; }
.readout-alert { color: #f2b134; font-weight: bold; }

/* --- banner -------------------------------------------------------------- */
.banner { background-color: #2a2313; border: 1px solid #4a3d1a; border-radius: 10px;
          padding: 10px 12px; }
.banner-text { color: #f2b134; font-size: 13px; }
";

/// Run the GTK application.
pub fn run() -> glib::ExitCode {
    // No GSK_RENDERER override. A previous revision forced "gl" here, purely to
    // silence a cosmetic Mesa/radv "not a conformant Vulkan implementation"
    // warning — and that pinned the app to one specific renderer on a machine
    // where the tops of tall glyphs are visibly shaved off (reported repeatedly;
    // this display runs at fractional scale 1.15, which is where glyph-atlas
    // rounding bugs live). Silencing a warning is not worth overriding the
    // toolkit's own choice of renderer. The warning may come back; the text
    // matters more.
    //
    // NOT yet proven to be the cause: a minimal program using this stylesheet
    // renders cleanly under "gl" here, so if the clipping survives this change,
    // the renderer was innocent. `GSK_RENDERER=gl|ngl|vulkan|cairo` still works
    // as an override for whoever tests it next.
    // Answered before GTK is initialised, so it needs no display and opens no
    // window. The updater runs this on a freshly downloaded binary to check it
    // can execute here before replacing the installed one, and a GUI binary
    // that needs a display to say its own version would be useless for that.
    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("tobii-gtk {}", env!("CARGO_PKG_VERSION"));
        return gtk::glib::ExitCode::SUCCESS;
    }

    let mut builder = Application::builder().application_id(APP_ID);
    if accuracy_mode() {
        // GApplication is single-instance by default: with the hub already
        // running, a second launch would hand off to it and merely raise its
        // window — in a process whose argv has no `--accuracy`, so the
        // diagnostic would silently never run.
        builder = builder.flags(gtk::gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
    // The stylesheet and the tray icon, both once the app starts.
    //
    // The tray is kept in a thread-local rather than passed down, because the
    // two places that need it are far apart and neither can reach the other: it
    // is created here, before any window exists (which is the whole point in
    // background mode), and it is CONSULTED by the hub's close handler, which
    // has to know whether hiding the window would leave the user no way back.
    // GTK is single-threaded and there is exactly one application, so the
    // sharing is real rather than incidental.
    //
    // There is no retry. There used to be one, five seconds later and only in
    // background mode, because `install` answered "is a status-area host up?"
    // once and collapsed "not yet" into "no icon" — which at login is a race
    // with the panel. It now answers that question whenever it is asked, so a
    // host arriving at any point is handled by the same subscription that
    // already handled a host restarting.
    app.connect_startup(|app| {
        load_css();
        publish_tray(app);
    });

    // The device thread, started once. In background mode it exists before any
    // window does; otherwise the first activation creates it.
    let session: Rc<RefCell<Option<device::Session>>> = Rc::new(RefCell::new(None));
    // The hub window, while it is open.
    let hub: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    if autostart::background_mode() {
        let session = session.clone();
        app.connect_startup(move |app| {
            // No window, so nothing would otherwise keep the main loop running.
            //
            // Leaked rather than stored. It was an
            // `Rc<RefCell<Option<ApplicationHoldGuard>>>` that was written once
            // and never read back or cleared — ceremony around a value that
            // must simply never drop. `mem::forget` is already this file's
            // idiom for a claim held for the life of the process (see the
            // accuracy diagnostic below).
            std::mem::forget(app.hold());
            // The device thread starts, but the tracker does not: `Demand` is
            // empty, so no USB session is opened and the illuminators stay off
            // until something actually asks for data.
            *session.borrow_mut() = Some(device::spawn());
        });
    }

    {
        let session = session.clone();
        let hub = hub.clone();
        // Whether the launch-time activation has already been seen.
        let launched = std::rc::Rc::new(Cell::new(false));
        app.connect_activate(move |app| {
            // THE BACKGROUND-MODE GUARD, and it is the whole feature.
            //
            // `GApplication` emits `activate` from `run()` whenever argv has no
            // files or options left to handle — and `--background` is filtered
            // out of argv before GTK sees it, so argc is 1 and activate fires.
            // Without this, `--background` built and presented the hub at
            // login, which took the focus claim, which opened a USB session:
            // the settings window popping up and the illuminators coming on at
            // every login, the exact inverse of what the flag means and of what
            // its own documentation promised.
            //
            // Only the FIRST activation is suppressed. Later ones arrive from a
            // second launch handing off to this instance — the user picking the
            // app from the menu — and must raise the hub.
            if autostart::background_mode() && !launched.replace(true) {
                return;
            }
            // A second launch — from the menu, the dock, `tobii-gtk` again —
            // hands off to this instance and lands here. Raise the hub that
            // already exists rather than building a second one.
            if let Some(w) = hub.borrow().as_ref() {
                w.present();
                return;
            }
            // Cloned, not taken. Every handle in a `device::Session` is a
            // clone of a shared thing — an `Arc`, a `Sender`, a `Demand` — so
            // this shares the one device thread. Taking it left the slot empty,
            // and the next hub in a long-lived background process spawned a
            // SECOND device thread that never exits; two of them then raced for
            // a USB interface only one can claim.
            let s = session
                .borrow_mut()
                .get_or_insert_with(device::spawn)
                .clone();
            if let Some(w) = build_hub(app, s) {
                *hub.borrow_mut() = Some(w);
            }
        });
    }
    {
        // When the hub is closed, forget it — so the next activation builds a
        // new one instead of presenting a destroyed window. In background mode
        // the process stays alive for the next time; otherwise the last window
        // closing ends it, as usual.
        let hub = hub.clone();
        app.connect_window_removed(move |_, w| {
            let is_hub = hub
                .borrow()
                .as_ref()
                .is_some_and(|h| h.upcast_ref::<gtk::Window>() == w);
            if is_hub {
                *hub.borrow_mut() = None;
            }
        });
    }
    // GApplication also parses argv itself and aborts on any option it does
    // not recognise, so our own flags have to be withheld from it.
    // `accuracy_mode` reads them straight from the environment instead.
    let gtk_args: Vec<String> = std::env::args().filter(|a| !is_our_flag(a)).collect();
    app.run_with_args(&gtk_args)
}

/// Flags this binary handles itself, which must never reach GTK's parser.
fn is_our_flag(arg: &str) -> bool {
    matches!(arg, "--accuracy" | "--version" | "-V" | "--background")
}

/// What the status line says for a device state.
///
/// "Idle" is not an error and must not read like one: it is the normal state of
/// a hub sitting in the background with the tracker deliberately switched off,
/// and a user who sees "Disconnected" there will go looking for a fault that
/// does not exist.
pub fn status_text(status: &device::ConnStatus) -> &'static str {
    match status {
        device::ConnStatus::Connected => "Connected",
        device::ConnStatus::Connecting => "Connecting…",
        device::ConnStatus::Idle => "Tracker off",
        device::ConnStatus::Error(_) => "Disconnected",
    }
}

/// Keep the tracker running for as long as `win` exists.
///
/// Every flow that needs live data runs in its own window, and the hub's own
/// claim is released the moment it loses focus to one of them — so each flow
/// has to ask for the tracker itself, for exactly as long as it is on screen.
fn hold_while_open(demand: &device::Demand, win: &impl IsA<gtk::Window>, reason: &'static str) {
    let guard = RefCell::new(Some(demand.hold(reason)));
    win.as_ref().connect_destroy(move |_| {
        *guard.borrow_mut() = None;
    });
}

/// Whether to run the gaze-accuracy diagnostic instead of the hub.
pub(crate) fn accuracy_mode() -> bool {
    std::env::args().any(|a| a == "--accuracy")
}

/// Ask GTK to round font metrics to whole pixels.
///
/// **A targeted guess at the reported glyph clipping, not a proven fix.** The
/// tops of tall glyphs are shaved on the reporter's display, which runs at
/// fractional scale 1.15 — GTK renders at scale 2 and the compositor scales the
/// buffer back down by 0.575. Everything else has been ruled out: the
/// stylesheet, the `GSK_RENDERER=gl` override this program used to force, and
/// rendering the same widgets offscreen at 1.0/1.15/1.25/1.5/2.0, none of which
/// reproduce it.
///
/// `gtk-hint-font-metrics` exists for this situation: with it off, font metrics
/// carry fractional values through a scaled pipeline and glyph extents can land
/// a fraction of a pixel short. It costs nothing to turn on.
///
/// If the clipping survives this, the remaining suspect is the compositor's own
/// downscale, which nothing here can reach — the test for that is to put the
/// display on an integer scale (100% or 200%) and look again.
fn hint_font_metrics() {
    if let Some(settings) = gtk::Settings::default() {
        settings.set_gtk_hint_font_metrics(true);
    }
}

/// Every `font-size: Npx` in `css`, multiplied by `scale`.
///
/// # Why the stylesheet is rewritten rather than a root size set
///
/// The obvious way to scale text in GTK is a `font-size` on the root, or the
/// `gtk-xft-dpi` setting. Neither works here on its own: this stylesheet states
/// every size in **absolute px**, and an absolute size is exactly what refuses
/// to inherit from a root rule or to follow a font DPI. So the sizes themselves
/// are what move, and the relative proportions the design was drawn with —
/// 20px title against 10px eyebrow — are preserved by construction.
///
/// `gtk-xft-dpi` is still set alongside this, for the text that has no class
/// and therefore no px size of its own; see [`apply_text_scale`].
///
/// Rounded to whole pixels, and never below 1: a fractional font-size is legal
/// CSS but lands on a half-pixel baseline, which is the fuzz this project has
/// already chased once.
fn scaled_css(css: &str, scale: f64) -> String {
    const KEY: &str = "font-size: ";
    if !scale.is_finite() || (scale - 1.0).abs() < f64::EPSILON {
        return css.to_string();
    }
    let mut out = String::with_capacity(css.len() + 64);
    let mut rest = css;
    while let Some(at) = rest.find(KEY) {
        let (before, after) = rest.split_at(at + KEY.len());
        out.push_str(before);
        let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
        match after[digits.len()..].strip_prefix("px") {
            // A size in some other unit, or none at all: copied through
            // untouched rather than guessed at.
            None => rest = after,
            Some(tail) => {
                let px: f64 = digits.parse().unwrap_or(0.0);
                out.push_str(&format!("{}px", ((px * scale).round() as i64).max(1)));
                rest = tail;
            }
        }
    }
    out.push_str(rest);
    out
}

/// The provider this app's stylesheet is loaded into.
///
/// Kept so the sheet can be replaced when the text scale changes. Loading a
/// second provider instead would leave the old sizes in the cascade at equal
/// priority, where the winner is whichever was added last — which works until
/// something else adds one.
///
/// Thread-local because a `CssProvider` is a GObject and therefore neither
/// `Send` nor `Sync` — which is not a limitation to work around: everything
/// that touches it runs on the GTK main thread by definition.
fn css_provider() -> gtk::CssProvider {
    thread_local! {
        static PROVIDER: std::cell::OnceCell<gtk::CssProvider> = const {
            std::cell::OnceCell::new()
        };
    }
    // Cloning a GObject is a refcount bump onto the same provider, so callers
    // get the one the display already has installed, not a second one.
    PROVIDER.with(|p| p.get_or_init(gtk::CssProvider::new).clone())
}

thread_local! {
    /// The published tray icon, for as long as the process runs.
    static TRAY: RefCell<Option<tray::Tray>> = const { RefCell::new(None) };
}

/// Publish the tray icon.
///
/// Called once, from `startup`.
///
/// Clicking it goes through `activate`, which already knows how to either
/// present the hub that exists or build one — doing it here instead would be a
/// second, divergent copy of that decision.
fn publish_tray(app: &Application) {
    let app = app.clone();
    let tray = tray::install("Tobii Eye Tracker 5", move || app.activate());
    TRAY.with(|c| *c.borrow_mut() = tray);
}

/// Whether a tray icon is somewhere the user can see it, and therefore whether
/// hiding the window leaves them a way back to it.
///
/// Asked at the moment of hiding rather than remembered from startup: a panel
/// can appear or die at any point in a session, and a `Tray` that exists is
/// not the same thing as an icon anybody is showing.
fn tray_is_published() -> bool {
    TRAY.with(|c| c.borrow().as_ref().is_some_and(tray::Tray::is_published))
}

thread_local! {
    /// How to fit the window to its content, registered once the window exists.
    ///
    /// Beside [`TRAY`] and for the same reason: the two ends are far apart and
    /// neither can reach the other. The cogwheel — which is what changes the
    /// text size — is built as part of the header, and the header is built
    /// before the window it would have to resize.
    ///
    /// Consulted by [`apply_text_scale`] rather than by the button, so that
    /// "the text got bigger" and "the window has to grow with it" are one fact
    /// in one place. They were two, threaded through three signatures to reach
    /// the click handler, and `load_css` — the other caller — did not know the
    /// second one. It happens not to need it, there being no window yet at
    /// startup, which is exactly the shape where the next caller gets it wrong.
    static REFIT: RefCell<Option<Rc<dyn Fn()>>> = const { RefCell::new(None) };
}

/// The gap between any two cards, whichever direction they are stacked.
///
/// One number for both the grid's row spacing and the spacing inside a column,
/// because the eye reads them as the same gap: the live band sits above the
/// first row of cards in the grid, and the second row of cards sits below the
/// first inside each column. Two different values there — 16 and 12, plus
/// whatever a justify added on top — made the rows visibly unevenly spaced.
const CARD_GAP: i32 = 16;

/// How wide a control column is.
///
/// All three share it, so they read as one rack that happens to be split
/// rather than as panels that disagree. It is a floor, not the width they end
/// up at: a card's padding and border sit outside it, and the columns stretch
/// to fill the window — which is why the breakpoints below measure the layouts
/// instead of doing arithmetic on this number.
const COLUMN_WIDTH: i32 = 360;

/// The width to lay the hub out for.
///
/// Not `width()`, which is 0 until the first allocation and stale while the
/// window is hidden in the tray, and not `default_width()`, which is what the
/// window was last *asked* to be. The larger of the two is the only expression
/// that is right in all three states, and it used to be written out at each of
/// the four places that needed it.
fn effective_width(w: &ApplicationWindow) -> i32 {
    w.width().max(w.default_width())
}

/// One column of the control rack.
///
/// A function rather than three copies, because [`COLUMN_WIDTH`]'s promise —
/// that the columns read as one rack rather than as panels that disagree — is
/// only true while all three are built the same way.
fn control_column() -> gtk::Box {
    let c = gtk::Box::new(Orientation::Vertical, CARD_GAP);
    c.set_size_request(COLUMN_WIDTH, -1);
    // Packed from the top: a column is as tall as its cards, and the row is as
    // tall as the tallest column. Anything else stretches the gaps.
    c.set_valign(Align::Start);
    c.set_hexpand(true);
    c
}

/// Install this app's stylesheet on the default display.
///
/// Public so a dialog can be rendered outside the hub for a visual check —
/// GTK's own `render_texture` gives an honest picture of a widget tree without
/// a compositor screenshot, which is how this dialog's width bug was found.
pub fn load_css() {
    hint_font_metrics();
    if let Some(display) = gtk::gdk::Display::default() {
        gtk::style_context_add_provider_for_display(
            &display,
            &css_provider(),
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }
    apply_text_scale(tobii_config::text_scale());
}

/// Re-render the stylesheet at `scale` and apply it live.
///
/// Two halves, because they cover different text. The stylesheet carries every
/// size this app states itself; `gtk-xft-dpi` carries the default font, which
/// is what any label without a style class is drawn in. Scaling only one of
/// them makes the UI grow unevenly, which looks like a bug rather than a
/// setting.
pub fn apply_text_scale(scale: f64) {
    css_provider().load_from_string(&scaled_css(CSS, scale));
    // The window has to grow with the text, or the last card is simply cut off
    // — which reads as the setting being broken rather than as a window that
    // needs dragging. Taken out of the cell so the fit itself cannot re-enter.
    let refit = REFIT.with(|c| c.borrow().clone());
    if let Some(fit) = refit {
        fit();
    }
    if let Some(settings) = gtk::Settings::default() {
        // Relative to whatever this desktop already asked for, not an absolute
        // DPI: a user on a HiDPI session has a large value here already, and
        // replacing it would undo their own scaling in this one app.
        thread_local! {
            static BASE_DPI: std::cell::OnceCell<i32> = const { std::cell::OnceCell::new() };
        }
        let base = BASE_DPI.with(|b| *b.get_or_init(|| settings.gtk_xft_dpi()));
        if base > 0 {
            settings.set_gtk_xft_dpi((f64::from(base) * scale).round() as i32);
        }
    }
}

/// The primary monitor, if one can be resolved.
fn primary_monitor() -> Option<gtk::gdk::Monitor> {
    gtk::gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_then(|obj| obj.downcast::<gtk::gdk::Monitor>().ok())
}

/// Aspect ratio (w/h) of the primary monitor, so the eye-position box mirrors
/// the screen's shape (e.g. 21:9). Falls back to 16:9.
pub(crate) fn screen_aspect() -> f64 {
    primary_monitor()
        .map(|m| {
            let g = m.geometry();
            if g.height() > 0 {
                g.width() as f64 / g.height() as f64
            } else {
                16.0 / 9.0
            }
        })
        .unwrap_or(16.0 / 9.0)
}

/// Primary monitor height (px), used by the fullscreen flows to place their
/// header a bit above the middle. Falls back to 1080.
pub(crate) fn screen_height() -> i32 {
    primary_monitor()
        .map(|m| m.geometry().height())
        .filter(|h| *h > 0)
        .unwrap_or(1080)
}

/// Make Esc close `win`. Closing is the flows' single exit route, so each flow's
/// own `close_request` handler still runs (that is where calibration aborts its
/// session) — this only triggers it.
pub(crate) fn add_escape_to_close(win: &ApplicationWindow) {
    let keys = gtk::EventControllerKey::new();
    let win_for_key = win.clone();
    keys.connect_key_pressed(move |_, key, _, _| {
        if key == gtk::gdk::Key::Escape {
            win_for_key.close();
            glib::Propagation::Stop
        } else {
            glib::Propagation::Proceed
        }
    });
    win.add_controller(keys);
}

/// Build the hub over an existing device thread.
///
/// Returns the hub window, or `None` when `--accuracy` took over and there is
/// no hub to return.
///
/// Public, together with [`load_css`], so the layout can be rendered outside a
/// normal run: GTK's own `render_texture` gives an honest picture of a widget
/// tree without a compositor screenshot, and that is how the model dialog's
/// width and the panel's margins were checked. Exposing the real constructor
/// rather than a probe-only twin means there is nothing here that only test
/// scaffolding calls.
pub fn build_hub(app: &Application, session: device::Session) -> Option<ApplicationWindow> {
    let (state, cmd_tx, demand, joystick_status) = session;
    // `--accuracy` runs the gaze-accuracy diagnostic instead of the hub. It
    // needs the device thread, so it branches here rather than in `run`.
    if accuracy_mode() {
        // The diagnostic drives the tracker for its whole run, and nothing else
        // is open to ask for it, so the claim is held for the life of the
        // process rather than tied to a window.
        std::mem::forget(demand.hold("the accuracy diagnostic"));
        accuracy::launch(app, state);
        return None;
    }
    // The gaze-preview overlay window, while it is open.
    let overlay_win: Rc<RefCell<Option<ApplicationWindow>>> = Rc::new(RefCell::new(None));
    // Set by the Quit item, and only by it. See the close handler: pressing X
    // hides or minimises rather than exiting, so something has to tell the two
    // apart.
    let really_quitting = Rc::new(Cell::new(false));
    // "Select eyes to detect": guard against echoing our own seeding as a user
    // change, and seed the radios from the device once per connection.
    let eye_seeding = Rc::new(Cell::new(false));
    let eye_seeded = Rc::new(Cell::new(false));
    // Gate for the once-per-connection `decide()` evaluation (force/recommend
    // calibration). Unlike `eye_seeded`, this must reset on disconnect so a
    // later reconnect — e.g. moved to a different monitor — is re-evaluated.
    let cal_evaluated = Rc::new(Cell::new(false));
    // True while a forced flow (setup or calibration) is open. Prevents a
    // disconnect/reconnect glitch mid-flow from stacking a second forced
    // window on top of the first (the disconnect branch resets
    // `cal_evaluated`, so without this the very next `Connected` tick would
    // re-enter `decide()` while the first window is still up). Cleared when
    // the flow closes, at which point `cal_evaluated` is also reset so
    // `decide()` re-runs immediately — this is what chains ForceSetup ->
    // ForceCalibration within one session instead of waiting for a reconnect.
    let forced_flow_open = Rc::new(Cell::new(false));

    // --- Header ---
    let title = Label::new(Some("Tobii Configuration"));
    title.add_css_class("app-title");
    title.set_halign(Align::Start);

    // Connection status (round indicator + label) — lower-left corner.
    let connected = Rc::new(Cell::new(false));
    let status_dot = DrawingArea::new();
    status_dot.set_content_width(14);
    status_dot.set_content_height(14);
    status_dot.set_valign(Align::Center);
    {
        let connected = connected.clone();
        status_dot.set_draw_func(move |_, cr, w, h| {
            let (w, h) = (w as f64, h as f64);
            if connected.get() {
                cr.set_source_rgb(0.18, 0.80, 0.45);
            } else {
                cr.set_source_rgb(0.85, 0.25, 0.25);
            }
            cr.arc(
                w / 2.0,
                h / 2.0,
                (w.min(h) / 2.0) - 1.0,
                0.0,
                std::f64::consts::TAU,
            );
            let _ = cr.fill();
        });
    }
    let status_label = Label::new(Some("Disconnected"));
    status_label.add_css_class("status");
    // Connection state belongs beside the title, not stranded at the bottom of
    // the window: it is the first thing that decides whether anything else on
    // screen means anything.
    let status_bar = gtk::Box::new(Orientation::Horizontal, 8);
    status_bar.set_halign(Align::End);
    status_bar.set_valign(Align::Center);
    status_bar.append(&status_dot);
    status_bar.append(&status_label);

    // --- Left column: live eye position ---
    let eye_title = Label::new(Some("EYE POSITION"));
    eye_title.add_css_class("eyebrow");
    eye_title.set_halign(Align::Start);

    // The trackbox is a volume in front of the sensor, not a window onto the
    // screen, so its preview must NOT take the monitor's aspect: the original
    // always stretches the normalized [0,1] box into a fixed golden-ratio
    // rectangle (`SetTrackBoxToGoldenRatio`: height = min(W,H)/2.25, width =
    // height * 1.618). Mirroring the monitor instead — as this did — badly skews
    // the motion gain on a wide screen: on this machine's 5120x1440 (3.56:1)
    // panel the drawn box came out 4.1:1, compressing vertical head movement by
    // ~2.6x relative to horizontal, which reads as the dot being sluggish
    // vertically and twitchy horizontally.
    //
    // `draw_eye_view` insets by `EYE_VIEW_PAD` on each side, so the *content*
    // size is padded out to make the DRAWN rectangle golden.
    // Shared with the draw func below: head orientation is drawn ON the
    // eye-position box rather than in a second widget showing the same head.
    let head_view: Rc<RefCell<Option<widget::HeadView>>> = Rc::new(RefCell::new(None));
    let head_for_draw = head_view.clone();
    let area = DrawingArea::new();
    // A minimum, not a size. `widget::eye_view_rect` letterboxes the golden box
    // inside whatever it is given, so the instrument can grow with the window
    // without skewing the motion gain — which is what "fixed 380px" was
    // protecting against, at the cost of never using the space.
    let pad = 2 * widget::EYE_VIEW_PAD as i32;
    // The content height that makes the DRAWN rectangle — inset by `pad` on
    // every side — golden.
    let golden_height =
        move |w: i32| (((w - pad) as f64) / widget::EYE_VIEW_RATIO).round() as i32 + pad;
    let min_w = 320;
    let min_h = golden_height(min_w);
    area.set_size_request(min_w, min_h);
    area.set_hexpand(true);
    // Height follows width, so the golden box fills the card instead of
    // pillarboxing inside a taller-than-needed allocation. Without this the
    // widget's height is whatever the layout hands it and the drawn box shrinks
    // to fit the *smaller* constraint — which is why it stayed small in a wide
    // window.
    {
        // Capped lower than the card could give it. Side by side with the
        // sensor view, this widget's height IS the live band's height — the
        // camera stretches to match it — and the band sits above everything
        // else rather than beside it, so every pixel here is a pixel of window.
        const MAX_H: i32 = 260;
        area.connect_resize(move |a, w, _h| {
            let want = golden_height(w).clamp(min_h, MAX_H);
            if a.content_height() != want {
                a.set_content_height(want);
            }
        });
    }
    {
        // Sample at PAINT time (as the calibration flow's preview already does)
        // rather than reading a snapshot written by the 33 ms tick: that tick is
        // slightly slower than the ~33 Hz gaze stream, so it silently dropped
        // roughly 3 frames a second — making the dot take an occasional
        // double-length step — and showed the rest up to 33 ms stale.
        let state = state.clone();
        area.set_draw_func(move |_, cr, w, h| {
            let view = widget::eye_view_for(&state.lock().unwrap());
            widget::draw_eye_view(cr, w, h, &view);
            widget::draw_head_overlay(cr, w, h, &view, head_for_draw.borrow().as_ref());
        });
    }
    // Redraw on the frame clock so every gaze frame reaches the screen exactly
    // once, instead of being resampled on the 33 ms tick. Same pattern as
    // `overlay.rs`. The original is likewise push-driven: its stream callback
    // marshals one update per frame straight onto the UI dispatcher.
    area.add_tick_callback(|a, _clock| {
        a.queue_draw();
        glib::ControlFlow::Continue
    });

    // Live NIR camera preview below the eye-position box (square, matches the
    // 280×280 stream). Shared frame drawn by the tick.
    let cam_title = Label::new(Some("SENSOR VIEW"));
    cam_title.add_css_class("eyebrow");
    cam_title.set_halign(Align::Start);
    let cam_frame: Rc<RefCell<Option<std::sync::Arc<tobii_protocol::CameraFrame>>>> =
        Rc::new(RefCell::new(None));
    let cam_area = DrawingArea::new();
    // A floor plus room to grow. `draw_camera_view` letterboxes the square
    // frame, so extra height makes the image bigger rather than adding black
    // bands — which is what lets this soak up the card's spare height instead of
    // leaving an empty gap under it.
    // A floor, not a size. It is low enough that the instrument column does not
    // decide the window's height on its own — with 230 it was 141px taller than
    // the shortest rack, which is the "almost aligned" gap that reads as a
    // mistake — and `vexpand` above means the view grows straight back into
    // whatever the tallest column turns out to need. So the image is as large
    // as the layout can afford rather than a number chosen in advance.
    cam_area.set_size_request(230, 180);
    cam_area.set_hexpand(true);
    cam_area.set_vexpand(true);
    {
        let cam_frame = cam_frame.clone();
        // The shape of the last frame this device sent, so the placeholder is
        // the same rectangle a frame occupies. Seeded with the ET5's 280x280
        // eye camera for the moment before the first one arrives, and corrected
        // by the first frame — a device that sends something else is then
        // matched rather than assumed about.
        let last_shape = Cell::new((280i32, 280i32));
        cam_area.set_draw_func(move |_, cr, w, h| {
            if let Some(f) = cam_frame.borrow().as_ref() {
                last_shape.set((f.width as i32, f.height as i32));
                widget::draw_camera_view(cr, w, h, f);
            } else {
                let (iw, ih) = last_shape.get();
                widget::draw_camera_placeholder(cr, w, h, iw, ih);
            }
        });
    }

    // One readout for the whole live view. Previously two labels sat here — a
    // guidance nudge and a head-pose sentence — reporting different halves of
    // the same measurement in different formats. A table scans faster and can
    // say "absent" where a sentence would have to omit the value silently.
    let readout = gtk::Grid::new();
    readout.add_css_class("readout");
    readout.set_halign(Align::Center);
    readout.set_valign(Align::Center);
    readout.set_row_spacing(2);
    // Two groups side by side — where the head is, and which way it faces — so
    // the readout is three rows tall rather than five. Column 2 is a spacer
    // between the groups.
    readout.set_column_spacing(10);
    // `width_chars` per group is what stops the table jumping: the values change
    // every frame, and a grid column sized to its content would resize the whole
    // readout as "good" becomes "not detected" or an angle gains a digit.
    let readout_cells: Vec<Vec<(Label, Label)>> = [(0i32, 2i32, 12), (3, 3, 9)]
        .iter()
        .map(|&(col, rows, chars)| {
            (0..rows)
                .map(|row| {
                    let name = Label::new(None);
                    name.add_css_class("readout-name");
                    name.set_halign(Align::Start);
                    name.set_xalign(0.0);
                    let value = Label::new(None);
                    value.add_css_class("readout-value");
                    value.set_halign(Align::End);
                    value.set_xalign(1.0);
                    value.set_width_chars(chars);
                    readout.attach(&name, col, row, 1, 1);
                    readout.attach(&value, col + 1, row, 1, 1);
                    (name, value)
                })
                .collect()
        })
        .collect();
    let spacer = Label::new(Some(" "));
    readout.attach(&spacer, 2, 0, 1, 1);

    // --- Live data, as one card: the two views SIDE BY SIDE. ---
    //
    // Stacked, these two made the instrument twice as tall as any control
    // column, and no amount of balancing the columns beside it could fix a
    // window whose height one card decided. Across the top instead, the card is
    // about as tall as a single view and the whole hub gets shorter — which is
    // what the split into columns was for in the first place.
    let gaze_side = gtk::Box::new(Orientation::Vertical, 12);
    gaze_side.set_hexpand(true);
    gaze_side.append(&eye_title);
    gaze_side.append(&area);

    // The numbers go BESIDE the box they describe, not under it. Under it they
    // added their own full height to a band that is now the top of the window,
    // and they are a narrow block of text next to a wide one — which is the
    // shape that fits in the space the trackbox cannot use anyway.
    let cam_side = gtk::Box::new(Orientation::Vertical, 12);
    cam_side.set_hexpand(true);
    cam_side.append(&cam_title);
    cam_side.append(&cam_area);

    let live = gtk::Box::new(Orientation::Horizontal, 20);
    live.add_css_class("surface");
    live.add_css_class("panel-pad");
    live.set_hexpand(true);
    live.append(&gaze_side);
    live.append(&readout);
    live.append(&cam_side);

    // --- Right column: settings sections (original wording) ---
    let b_setup = crate::widget::button("Set up display");
    {
        let app = app.clone();
        let cmd_tx = cmd_tx.clone();
        let demand = demand.clone();
        b_setup.connect_clicked(move |btn| {
            // One flow at a time, the same guard `b_cal` has. A GtkButton emits
            // `clicked` per release and presenting a fullscreen Wayland surface
            // is asynchronous, so a double click lands both on the hub before
            // the first window maps — two wizards over one device session, both
            // seeded from the same config snapshot, and whichever is completed
            // second silently reverts the first.
            btn.set_sensitive(false);
            let win = setup_flow::launch(&app, cmd_tx.clone());
            hold_while_open(&demand, &win, "display setup");
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    let sw_preview = Switch::new();
    sw_preview.set_valign(Align::Center);
    sw_preview.set_tooltip_text(Some("Show a dot on screen where you're looking"));
    {
        let app = app.clone();
        let state = state.clone();
        let overlay_win = overlay_win.clone();
        let demand = demand.clone();
        sw_preview.connect_state_set(move |_sw, on| {
            let mut ow = overlay_win.borrow_mut();
            if on {
                if ow.is_none() {
                    let w = overlay::show(&app, state.clone());
                    // The overlay is the one consumer that is useful precisely
                    // when the hub is not focused, so it must ask for the
                    // tracker in its own right.
                    hold_while_open(&demand, &w, "the gaze preview");
                    *ow = Some(w);
                }
            } else if let Some(w) = ow.take() {
                w.close();
            }
            glib::Propagation::Proceed
        });
    }

    // Radio indicators with SEPARATE labels: a CheckButton's built-in label
    // clips its text on this theme, a standalone GtkLabel does not.
    let eyes_ctl = gtk::Box::new(Orientation::Horizontal, 16);
    // What the user last chose, for the case the device cannot answer. The
    // radios were seeded only from the device, so a hub opened with the tracker
    // unplugged showed "both" no matter what had been selected — and touching
    // a radio to correct it would send a command that was then dropped.
    let saved_eye = tobii_config::load_enabled_eye().ok().flatten();
    let radio = |text: &str, group: Option<&CheckButton>| {
        let cb = CheckButton::new();
        if let Some(g) = group {
            cb.set_group(Some(g));
        }
        cb.set_valign(Align::Center);
        let lbl = Label::new(Some(text));
        lbl.set_valign(Align::Center);
        lbl.set_margin_top(2);
        lbl.set_margin_bottom(2);
        let row = gtk::Box::new(Orientation::Horizontal, 5);
        row.append(&cb);
        row.append(&lbl);
        (cb, row)
    };
    let (r_both, box_both) = radio("Both eyes", None);
    let (r_left, box_left) = radio("Left eye only", Some(&r_both));
    let (r_right, box_right) = radio("Right eye only", Some(&r_both));
    // Seeded from the SAVED preference before the device has said anything.
    // The device's own value still overrides this on connect (below), which is
    // the authority when there is one — but with the tracker unplugged the hub
    // used to show "both" whatever the user had chosen, and correcting it sent
    // a command that was then dropped.
    eye_seeding.set(true);
    match saved_eye {
        Some(EnabledEye::Left) => r_left.set_active(true),
        Some(EnabledEye::Right) => r_right.set_active(true),
        _ => r_both.set_active(true),
    }
    eye_seeding.set(false);
    eyes_ctl.append(&box_both);
    eyes_ctl.append(&box_left);
    eyes_ctl.append(&box_right);

    // Selecting a radio pushes the choice to the device (unless we're seeding).
    for (cb, eye) in [
        (&r_both, EnabledEye::Both),
        (&r_left, EnabledEye::Left),
        (&r_right, EnabledEye::Right),
    ] {
        let cmd_tx = cmd_tx.clone();
        let seeding = eye_seeding.clone();
        cb.connect_toggled(move |c| {
            if c.is_active() && !seeding.get() {
                // Saved HERE, at the point of choice, not in the device thread
                // where the command is applied. A preference belongs to the
                // user, not to whether the tracker happens to be reachable:
                // persisting it only on a successful apply meant that choosing
                // an eye with the tracker unplugged was lost permanently, since
                // the queued command is dropped when the connect fails and
                // nothing else ever writes it.
                if let Err(e) = tobii_config::save_enabled_eye(eye) {
                    tobii_diagnostics::log::warn(&format!("could not save the eye selection: {e}"));
                }
                let _ = cmd_tx.send(device::DeviceCommand::SetEnabledEye(eye));
            }
        });
    }

    let b_cal = crate::widget::button("Improve calibration");
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        let sw_preview = sw_preview.clone();
        let demand = demand.clone();
        b_cal.connect_clicked(move |btn| {
            // The gaze preview is a layer-shell surface on the Overlay layer,
            // which composites ABOVE a fullscreen window — the user would end
            // up chasing their own gaze dot instead of the stimulus dot, which
            // poisons every sample while still reporting success. Switching the
            // toggle off closes it through the switch's own handler, so the
            // switch and the overlay window cannot end up disagreeing.
            sw_preview.set_active(false);
            // One flow at a time: a second window would drive the same device
            // session and corrupt the first's point accounting.
            btn.set_sensitive(false);
            // The hub's entry is "Improve calibration": refine what is there.
            let win = calibrate_flow::launch(&app, state.clone(), cmd_tx.clone(), true);
            hold_while_open(&demand, &win, "calibration");
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    // The controls sit in three columns BENEATH the live data: the two setup
    // wizards, the two things the tracker measures, and the two places the
    // result goes.
    //
    // The grouping is also what makes them the same height, and that is not a
    // coincidence — it was chosen from the measured cards (180/141/161/120/
    // 213/222) so no column runs away with the row. The obvious grouping put
    // both game sections together at 447 against 293, and a row is as tall as
    // its tallest column.
    //
    // They pack from the top and every gap is `CARD_GAP`. Stretching the gaps
    // to level the bottoms — which is what this did while the columns were
    // badly unbalanced — is no longer worth it: with 357/370/358 (the pairs
    // above, plus the `CARD_GAP` between them) the levelling buys at most 13px,
    // and it costs a gap inside a column that visibly differs from the gap
    // between the rows.
    let col_calib = control_column();
    col_calib.append(&section(
        "Improve my calibration",
        "If the light conditions change or if you experience less tracker precision, you might \
         benefit from improving your calibration.",
        &b_cal,
    ));
    col_calib.append(&section(
        "Change screen",
        "If you move the sensor to a different monitor, you'll need to set up the new display.",
        &b_setup,
    ));

    // --- Recommend-recalibration banner: a dismissible row, not a modal. Shown
    // by the tick's once-per-connection `decide()` evaluation below. ---
    let banner = gtk::Box::new(Orientation::Horizontal, 12);
    banner.add_css_class("banner");
    banner.set_visible(false);
    let banner_label = Label::new(None);
    banner_label.add_css_class("banner-text");
    banner_label.set_hexpand(true);
    banner_label.set_xalign(0.0);
    banner_label.set_wrap(true);
    banner_label.set_valign(Align::Center);
    // The banner is the one place in the hub with a call to action, so it is
    // the one place the accent is spent.
    let banner_recal = crate::widget::button("Recalibrate");
    banner_recal.add_css_class("primary");
    let banner_dismiss = crate::widget::button("Dismiss");
    banner_dismiss.add_css_class("quiet");
    banner.append(&banner_label);
    banner.append(&banner_recal);
    banner.append(&banner_dismiss);
    {
        let banner = banner.clone();
        banner_dismiss.connect_clicked(move |_| banner.set_visible(false));
    }
    {
        let app = app.clone();
        let state = state.clone();
        let cmd_tx = cmd_tx.clone();
        let sw_preview = sw_preview.clone();
        let banner = banner.clone();
        let demand = demand.clone();
        banner_recal.connect_clicked(move |btn| {
            // Same reasoning as `b_cal`: the gaze-preview overlay would poison
            // the recalibration's own samples.
            sw_preview.set_active(false);
            // A recalibration is now in flight; the recommendation no longer
            // applies (and would otherwise reappear stale once this closes).
            banner.set_visible(false);
            btn.set_sensitive(false);
            // The banner fires when the existing calibration is no longer
            // trusted, so seeding from it would be self-defeating.
            let win = calibrate_flow::launch(&app, state.clone(), cmd_tx.clone(), false);
            hold_while_open(&demand, &win, "calibration");
            let btn = btn.clone();
            win.connect_close_request(move |_| {
                btn.set_sensitive(true);
                glib::Propagation::Proceed
            });
        });
    }

    // What the tracker measures: which eyes to look for, and head pose.
    let col_display = control_column();
    col_display.append(&section(
        "Select eyes to detect",
        "If you typically squint or have poor sight in one eye, you can make the eye tracker \
         detect one eye only.",
        &eyes_ctl,
    ));
    col_display.append(&section(
        "Head tracking",
        "Sends your head position and angle to games and apps, over opentrack.",
        &head_model::control(state.clone(), cmd_tx.clone()),
    ));

    // Where the result goes: onto the screen, and out to a game.
    let col_games = control_column();
    col_games.append(&section(
        "Preview my gaze",
        "Shows you a visual trail of your gaze.",
        &sw_preview,
    ));
    // Beside "Head tracking" rather than in the cogwheel: it is about what the
    // tracker does, not about how this program behaves.
    let games_row = crate::games::GamesRow::build(joystick_status);
    col_games.append(&section(
        "Head tracking for games",
        "Sends head tracking and gaze to a game. Wrap the game with \
         `tobii game -- <command>` — in Steam, put that in Launch Options.",
        &games_row.controls,
    ));

    // --- Responsive split -------------------------------------------------
    // Side by side when there is room, stacked when there is not. GTK4 has no
    // media queries, so the breakpoint is watched on the window's width and the
    // container's orientation is swapped — which is all this layout needs,
    // because both halves already expand.
    // A Grid, not a Box. In a Box the rack's natural width won over the
    // instrument's `hexpand` and it absorbed every spare pixel (measured: 590px
    // allocated against a 389px natural, while the instrument sat at its floor).
    // A Grid lets the two columns' expansion be stated per child and honoured.
    let split = gtk::Grid::new();
    split.set_hexpand(true);
    // NOT vexpand. With it, the row took the window's whole remaining height and
    // the instrument card — which fills its cell — grew past the control rack,
    // which sizes to its cards. That is why the two columns kept ending at
    // different heights however the card itself was configured: the mismatch was
    // the ROW being taller than either column needed. Sized to content, both
    // columns end level and any spare window height is plain background.
    split.set_vexpand(false);
    split.set_valign(Align::Start);
    split.set_column_spacing(CARD_GAP as u32);
    split.set_row_spacing(CARD_GAP as u32);
    // The three layouts. One function, so the arrangement a width selects and
    // the arrangement it was measured against cannot drift apart.
    let arrange: Rc<dyn Fn(i32)> = {
        let split = split.clone();
        let live = live.clone();
        let col_calib = col_calib.clone();
        let col_display = col_display.clone();
        let col_games = col_games.clone();
        Rc::new(move |columns: i32| {
            let columns = columns.clamp(1, 3);
            // Only what is actually in the grid: the first call is the one that
            // fills it, and removing an unattached child is a GTK-CRITICAL.
            for card in [
                live.upcast_ref::<gtk::Widget>(),
                col_calib.upcast_ref(),
                col_display.upcast_ref(),
                col_games.upcast_ref(),
            ] {
                if card.parent().is_some() {
                    split.remove(card);
                }
            }
            // The live data always spans the full width, whatever that is: it
            // is one card and splitting it across rows would put the two views
            // in different places depending on the window size. Spanning
            // `columns` rather than a literal per arm is what keeps that true —
            // the two disagreeing is a card that stops short of the window edge.
            split.attach(&live, 0, 0, columns, 1);
            // And the first control column is always directly under it.
            split.attach(&col_calib, 0, 1, 1, 1);
            match columns {
                3 => {
                    split.attach(&col_display, 1, 1, 1, 1);
                    split.attach(&col_games, 2, 1, 1, 1);
                }
                // Games spans both columns rather than sitting under one of
                // them: it is the widest card of the three, and a half-width
                // hole beside it would read as something missing.
                2 => {
                    split.attach(&col_display, 1, 1, 1, 1);
                    split.attach(&col_games, 0, 2, 2, 1);
                }
                _ => {
                    split.attach(&col_display, 0, 2, 1, 1);
                    split.attach(&col_games, 0, 3, 1, 1);
                }
            }
            // Stacked, the two live views go one above the other — side by side
            // in a narrow window each would be too small to read.
            live.set_orientation(if columns == 1 {
                Orientation::Vertical
            } else {
                Orientation::Horizontal
            });
        })
    };

    // What each layout ACTUALLY needs, asked of the widgets rather than added
    // up by hand. Two hand-added versions were wrong before this: the first was
    // 56px short of three columns because a card's padding and border are in
    // none of the numbers you would add up, and the second assumed dropping a
    // column frees its width plus a gutter, which is 63px more than it frees —
    // the columns are wider than `COLUMN_WIDTH` for the same padding reason,
    // and the games card that spans the two remaining columns has a floor of
    // its own. Both mistakes look the same from the outside: GTK warns that it
    // is being measured for less width than it needs, and the cards clip.
    //
    // Measured widest-last so the grid is left in the layout the window opens
    // in, which is also what the height below is measured from — measuring an
    // empty grid once gave a window the height of its header with every card
    // clipped off the bottom.
    arrange(2);
    let two_col_min = split.measure(Orientation::Horizontal, -1).0;
    arrange(3);
    let three_col_min = split.measure(Orientation::Horizontal, -1).0;

    let header = gtk::Box::new(Orientation::Horizontal, 12);
    title.set_hexpand(true);
    header.append(&title);
    header.append(&status_bar);
    header.append(&settings_button(&really_quitting));

    // One margin all round, so the frame of background around the content is
    // even. Anything else reads as a mistake at the corners.
    const PAGE_MARGIN: i32 = 20;
    let root = gtk::Box::new(Orientation::Vertical, 14);
    root.set_margin_top(PAGE_MARGIN);
    root.set_margin_bottom(PAGE_MARGIN);
    root.set_margin_start(PAGE_MARGIN);
    root.set_margin_end(PAGE_MARGIN);
    root.append(&header);
    let update_banner = update::banner();
    root.append(&update_banner);
    root.append(&banner);
    root.append(&split);

    // How tall the content wants to be at the width the window will open at.
    // Measured before the window exists, so the window can be built around it.
    // Wide enough to open in the three-column layout, from what the columns
    // measured rather than from a number. The slack above their minimum goes to
    // the instrument, which is the only thing here that benefits from more.
    let default_width = (three_col_min + PAGE_MARGIN * 2).max(1180);
    let (_, natural_height, _, _) = root.measure(Orientation::Vertical, default_width);

    // Scroll rather than clip: a short window (or a stacked narrow one) must
    // still be able to reach the controls at the bottom.
    let scroller = gtk::ScrolledWindow::new();
    scroller.set_hscrollbar_policy(gtk::PolicyType::Never);
    scroller.set_child(Some(&root));
    scroller.set_propagate_natural_height(true);

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Tobii Configuration")
        .default_width(default_width)
        // Measured, not chosen. Asking the content how tall it wants to be at
        // this width means the background below the last card is exactly
        // `PAGE_MARGIN`, matching the sides — and it stays that way if the
        // user's font metrics differ from the ones this was written against,
        // which a hardcoded number could not.
        .default_height(natural_height)
        .build();
    window.set_child(Some(&scroller));

    // The window follows its content from here on.
    //
    // `natural_height` above is a single measurement taken at build time, and
    // two ordinary things invalidate it: the text size, which the cogwheel can
    // change at any moment, and a banner appearing — the update notice arrives
    // from a network check hundreds of milliseconds after the window is drawn.
    // Both used to leave the window at whatever height it was born with, so
    // larger text was clipped and a banner pushed the last card out of view.
    {
        let root = root.clone();
        let fit_window = window.clone();
        // The last size this closure itself set. Anything else is the user.
        //
        // The comment here used to claim it never touched "a window the user
        // has sized themselves", and checked only maximised and fullscreen —
        // which are the two cases a user cannot produce by dragging an edge.
        // Dragging is the ordinary one, and it was overridden: on GTK4
        // `default_height` tracks the current size, so a window made smaller
        // was silently grown back to its content the next time anything
        // re-fitted.
        let fit: Rc<dyn Fn()> = {
            let ours = Cell::new((0i32, 0i32));
            Rc::new(move || {
                // Maximised or fullscreen, the height is not ours to choose.
                if fit_window.is_maximized() || fit_window.is_fullscreen() {
                    return;
                }
                let (w, h) = (fit_window.default_width(), fit_window.default_height());
                // Sized by hand since we last set it: leave it alone. The first
                // call sees (0, 0) and proceeds, which is what opens the window
                // at its content height.
                if ours.get() != (0, 0) && ours.get() != (w, h) {
                    return;
                }
                let width = effective_width(&fit_window);
                let (_, wanted, _, _) = root.measure(Orientation::Vertical, width);
                if wanted != h {
                    fit_window.set_default_size(width, wanted);
                    ours.set((width, wanted));
                }
            })
        };
        REFIT.with(|c| *c.borrow_mut() = Some(fit.clone()));

        // Once, and latched — because `map` is not a first-appearance signal.
        // It fires on every show, so closing to the tray and coming back used to
        // re-measure and resize the window each time, which is how a window the
        // user had made smaller grew back. `natural_height` above is measured
        // against a widget tree that has never been laid out and comes out a
        // little short — the cards' padding is not all accounted for until they
        // have been allocated once — so one re-fit is worth it and the rest are
        // not.
        {
            let fit = fit.clone();
            let first = Cell::new(true);
            window.connect_map(move |_| {
                if !first.replace(false) {
                    return;
                }
                // One idle later, not inline: `map` runs BEFORE the first
                // allocation, so measuring here returns the same slightly-short
                // answer `natural_height` already got. After one turn of the
                // main loop the tree has been laid out once and the measurement
                // is the real one.
                let fit = fit.clone();
                glib::idle_add_local_once(move || fit());
            });
        }

        // Watched rather than called from the code that shows them: both
        // banners are shown from several places (a timeout, a dismiss, the
        // once-per-connection evaluation), and one signal covers all of them.
        for b in [&update_banner, &banner] {
            let fit = fit.clone();
            b.connect_visible_notify(move |_| fit());
        }
    }
    // A height floor, kept deliberately low: height only decides how much
    // scrolling there is, so there is no reason to stop the user shrinking it
    // past the point where the content fits.
    //
    // Width is left out on purpose. Whatever is asked for here, GTK enforces
    // the larger of it and what the content measures, and the content measures
    // 1152px in the widest layout even at the smallest text — so any floor
    // below that is inert, and any floor above it would fight the breakpoints
    // below, which exist precisely so the window CAN be made narrow.
    window.set_size_request(-1, 340);

    // The breakpoints. Three layouts, chosen by width alone, each switching at
    // the width its own layout measured — see `arrange` above.
    let breakpoint: Rc<dyn Fn(i32)> = {
        // Measured above, plus the page margins either side.
        let three_below = three_col_min + PAGE_MARGIN * 2;
        let two_below = two_col_min + PAGE_MARGIN * 2;

        let arrange = arrange.clone();
        // Three, because that is how the children were attached above — so a
        // window that opens wide enough is not needlessly torn down and rebuilt
        // before it is first drawn.
        let columns_now = std::cell::Cell::new(3i32);
        let apply = move |width: i32| {
            let columns = if width >= three_below {
                3
            } else if width >= two_below {
                2
            } else {
                1
            };
            if columns_now.get() == columns {
                return;
            }
            columns_now.set(columns);
            arrange(columns);
        };
        // Shared, because one signal is not enough to be sure. `default-width`
        // does not fire for every way a window can change size (tiling and
        // maximising among them), and a breakpoint that misses those is exactly
        // the "not responsive" complaint. The hub already runs a 33 ms tick, so
        // it also re-checks there — one integer compare, and `apply` returns
        // immediately when nothing changed.
        let apply: Rc<dyn Fn(i32)> = Rc::new(apply);
        apply(effective_width(&window));
        let on_notify = apply.clone();
        window.connect_default_width_notify(move |w| on_notify(effective_width(w)));
        apply
    };

    // ~30 fps tick: read the device snapshot, refresh status + eye view.
    let tick_app = app.clone();
    let tick_window = window.clone();
    let tick_cmd_tx = cmd_tx.clone();
    // Used twice inside the tick: to sync the hub's own focus claim, and
    // because a forced flow opens without the user clicking anything and so has
    // to ask for the tracker itself, just like the flows the buttons open.
    let tick_demand = demand.clone();
    let tick_banner = banner.clone();
    let tick_banner_label = banner_label.clone();
    // The forced-calibration path needs this for the same reason the two
    // user-initiated ones do: the layer-shell gaze overlay composites ABOVE the
    // fullscreen calibration window, so the user chases their own gaze dot
    // instead of the stimulus dot — which poisons every sample while still
    // reporting success. This branch is reachable with the preview on: turn it
    // on, have no usable saved calibration, then unplug and replug.
    let tick_sw_preview = sw_preview.clone();
    // The games row's status line is driven from here so that a game starting
    // or stopping — or `tobii games` run in a terminal — is reflected without
    // reopening the window, which is how somebody setting this up works.
    let tick_games = Rc::new(games_row);
    // The hub's claim on the tracker, synced from `window.is_active()` on every
    // tick. Declared here because the tick below owns it.
    let focus_hold: Rc<RefCell<Option<device::DemandGuard>>> = Rc::new(RefCell::new(None));
    let tick_focus = focus_hold.clone();

    // Kept so the close handler can retire it: the tick captures the
    // application and can open a forced flow, so it must not outlive the hub.
    // Also so it can be stopped while the window is hidden — see `restart_tick`.
    let tick_id: Rc<RefCell<Option<glib::SourceId>>> = Rc::new(RefCell::new(None));
    let tick_body: Rc<dyn Fn() -> glib::ControlFlow> = Rc::new({
        move || {
            // Ask for the tracker exactly while this window has focus. Polled
            // rather than driven by `notify::is-active`: see the note below.
            {
                let active = tick_window.is_active();
                let mut h = tick_focus.borrow_mut();
                match (active, h.is_some()) {
                    (true, false) => *h = Some(tick_demand.hold("the hub window")),
                    (false, true) => *h = None,
                    _ => {}
                }
            }
            // Nothing below this line is worth doing for a window nobody can
            // see. Everything above it still runs: the claim has to be released
            // when the window stops being active, which is the whole reason the
            // tracker goes dark when you minimise.
            //
            // `is_suspended` rather than `is_minimized`: GTK sets it for any
            // reason the surface is not visible to the user — minimised, fully
            // occluded, on another workspace — which is exactly the set of
            // states where redrawing a readout is wasted work. That is the case
            // this guard is for, since a minimised window stays mapped and so
            // keeps ticking.
            //
            // `is_mapped` as well, and it is not redundant: a HIDDEN window —
            // which is what closing to the tray produces — reports
            // `is_suspended() == false`. Measured, on this window: hidden gives
            // `mapped=false suspended=false`, minimised gives `suspended=true`.
            // The tick is stopped outright while hidden, so this is the window
            // between construction and the first `map`, plus whatever gap a
            // compositor leaves between the two signals.
            if tick_window.is_suspended() || !tick_window.is_mapped() {
                return glib::ControlFlow::Continue;
            }

            // Re-check the layout breakpoint. `notify::default-width` misses some
            // ways a window changes size (tiling, maximising), and this costs one
            // integer compare — `apply` returns immediately when nothing changed.
            //
            // Guarded, because `width()` is 0 before the first allocation — and
            // 0 is below every breakpoint, so it collapsed the grid to the
            // one-column layout. That is not only a startup race: it also
            // happened for the whole time the window was hidden, and the re-fit
            // on the next show then measured the stacked tree and sized the
            // window for it. Measured: natural height 803 -> 1736, window left
            // 420px too tall for the rest of the run. `fit` already guards the
            // same way.
            breakpoint(effective_width(&tick_window));
            // Move the camera frame out (no 78 KB clone) and clone the rest cheaply,
            // under one lock. `new_cam` is None on the ticks between device frames.
            let (snap, new_cam) = {
                let mut s = state.lock().unwrap();
                let cam = s.latest_camera.take();
                (s.clone(), cam)
            };
            let conn = matches!(snap.status, device::ConnStatus::Connected);
            connected.set(conn);
            status_label.set_text(status_text(&snap.status));
            status_dot.queue_draw();
            // Evaluate the calibration state machine once per fresh `Connected`
            // transition (reset on disconnect so a later reconnect — e.g. moved to
            // a different monitor — is re-evaluated). All branching logic lives in
            // `tobii_config::decide`; this only computes its inputs and maps its
            // output to a UI action.
            if conn {
                if !cal_evaluated.get() && !forced_flow_open.get() {
                    cal_evaluated.set(true);
                    match evaluate_calibration_state() {
                        tobii_config::CalAction::ForceSetup => launch_forced(
                            &tick_app,
                            &tick_window,
                            &forced_flow_open,
                            &cal_evaluated,
                            tobii_config::CalAction::ForceSetup,
                            {
                                let cmd_tx = tick_cmd_tx.clone();
                                let demand = tick_demand.clone();
                                move |app| {
                                    let w = setup_flow::launch(app, cmd_tx.clone());
                                    hold_while_open(&demand, &w, "display setup");
                                    w
                                }
                            },
                        ),
                        tobii_config::CalAction::ForceCalibration => launch_forced(
                            &tick_app,
                            &tick_window,
                            &forced_flow_open,
                            &cal_evaluated,
                            tobii_config::CalAction::ForceCalibration,
                            {
                                tick_sw_preview.set_active(false);
                                let state = state.clone();
                                let cmd_tx = tick_cmd_tx.clone();
                                let demand = tick_demand.clone();
                                move |app| {
                                    let w = calibrate_flow::launch(
                                        app,
                                        state.clone(),
                                        cmd_tx.clone(),
                                        false,
                                    );
                                    hold_while_open(&demand, &w, "calibration");
                                    w
                                }
                            },
                        ),
                        tobii_config::CalAction::RecommendCalibration(reason) => {
                            show_recommend_banner(&tick_banner_label, &tick_banner, reason);
                        }
                        tobii_config::CalAction::None => {}
                    }
                }
            } else {
                cal_evaluated.set(false);
                // Re-seed on the next connection, like `cal_evaluated`. This
                // latch used to be set once for the life of the hub, so a value
                // that changed while the tracker was away — a failed re-apply,
                // another program, a firmware reset — left the radios showing
                // the old one until the window was closed and reopened. The
                // saved preference cannot be lost by re-seeding: it is written
                // at the point of choice and re-applied on every connect, so
                // what the device reports afterwards is that preference.
                eye_seeded.set(false);
            }
            // Seed the eye-selection radios once from the device's current value.
            if conn && !eye_seeded.get() {
                if let Some(e) = snap.enabled_eye {
                    eye_seeding.set(true);
                    match e {
                        EnabledEye::Both => r_both.set_active(true),
                        EnabledEye::Left => r_left.set_active(true),
                        EnabledEye::Right => r_right.set_active(true),
                    }
                    eye_seeding.set(false);
                    eye_seeded.set(true);
                }
            }
            tick_games.refresh(conn);

            // Only the guidance *text* is driven from this tick — the dots redraw
            // themselves on the frame clock (see `area.add_tick_callback` above).
            // Text at 33 ms is ample: it is damped over 11-49 frames upstream.
            let head_now = widget::head_view_for(&snap);
            {
                let ev = widget::eye_view_for(&snap);
                for (cells, rows) in readout_cells
                    .iter()
                    .zip(widget::readout_groups(&ev, head_now.as_ref()))
                {
                    for ((n, v), r) in cells.iter().zip(rows) {
                        n.set_text(r.label);
                        v.set_text(&r.value);
                        if r.alert {
                            v.add_css_class("readout-alert");
                        } else {
                            v.remove_css_class("readout-alert");
                        }
                    }
                }
            }
            // Camera preview: keep the last frame between device frames; update on a
            // new one; clear on disconnect so nothing stale lingers. Only redraw when
            // something actually changed — repainting a 78 KB frame every tick is
            // pure waste on the ticks between device frames.
            if !conn {
                let had = cam_frame.borrow().is_some();
                *cam_frame.borrow_mut() = None;
                if had {
                    cam_area.queue_draw();
                }
            } else if new_cam.is_some() {
                *cam_frame.borrow_mut() = new_cam;
                cam_area.queue_draw();
            }
            // Head pose: same discipline as the camera — only redraw when the view
            // actually changed, so an unchanged pose costs nothing.
            if *head_view.borrow() != head_now {
                *head_view.borrow_mut() = head_now;
                area.queue_draw();
            }
            glib::ControlFlow::Continue
        }
    });

    // Started now, stopped whenever the window is unmapped, started again when
    // it comes back.
    //
    // The guard inside the body already skips the work for a window nobody can
    // see, but the timer still fired 30 times a second to reach that guard —
    // and closing to the tray is a state the hub sits in for hours while a game
    // plays. Measured on this machine: 30 wakeups/s at 12.5 us each, 0.038% of
    // a core, ~324k no-op wakeups over a three-hour session, and 30 scheduler
    // wakeups a second is enough to keep a laptop out of its deeper idle
    // states.
    //
    // `map`/`unmap` and not `is_suspended`, because those are the transitions
    // that bracket exactly the hidden case: measured on this window,
    // `minimize()` leaves it MAPPED (so a minimised hub keeps ticking and the
    // body's `is_suspended` guard handles it, unchanged), while
    // `set_visible(false)` unmaps and `present()` maps again.
    let restart_tick: Rc<dyn Fn()> = {
        let tick_id = tick_id.clone();
        let tick_body = tick_body.clone();
        Rc::new(move || {
            let mut slot = tick_id.borrow_mut();
            if slot.is_some() {
                return;
            }
            let body = tick_body.clone();
            *slot = Some(glib::timeout_add_local(
                Duration::from_millis(33),
                move || body(),
            ));
        })
    };
    restart_tick();
    {
        let restart_tick = restart_tick.clone();
        window.connect_map(move |_| restart_tick());
    }
    {
        let tick_id = tick_id.clone();
        let unmap_focus = focus_hold.clone();
        window.connect_unmap(move |_| {
            if let Some(id) = tick_id.borrow_mut().take() {
                id.remove();
            }
            // The one thing above the body's guard that still has to happen.
            // It was the tick that released the tracker claim when the window
            // stopped being active; with the tick stopped, hiding the window
            // has to release it here — which it does at the moment of hiding
            // rather than a frame or two later.
            *unmap_focus.borrow_mut() = None;
        });
    }

    // The tracker runs while you are looking at the hub, and not otherwise.
    //
    // Focus rather than visibility: a hub left open on another workspace, or
    // behind a game, is not being read, and a bar of infrared LEDs glowing
    // under the monitor all day is the single most irritating thing a device
    // like this can do. `Demand`'s three-second linger absorbs the focus
    // flicker of opening a dialog, and every flow that needs the tracker
    // without the hub focused — calibration, setup, the gaze overlay — holds
    // its own claim.
    //
    // Driven from the tick above, from `window.is_active()`, and from nowhere
    // else. It was previously an unconditional claim taken at build time plus a
    // `notify::is-active` handler to release it — and where a compositor never
    // granted focus the notification never fired, so the claim was never
    // released and the tracker stayed lit for the life of the process. Polling
    // a boolean 30 times a second is not elegant, but it is self-correcting,
    // which a one-shot signal is not.
    // Everything the hub owns is released here, and all of it matters.
    {
        let focus_hold = focus_hold.clone();
        let overlay_win = overlay_win.clone();
        let tick_id = tick_id.clone();
        let quitting = really_quitting.clone();
        window.connect_close_request(move |w| {
            // PRESSING X PUTS THE PROGRAM IN THE BACKGROUND; it does not exit.
            //
            // The tracker's data has to keep reaching a game while the user is
            // playing, and configuring it means opening this window — so the
            // program that owns the device cannot be one that dies when its
            // window is dismissed.
            //
            // WHERE it goes depends on whether there is a tray icon, and the
            // difference matters: hiding a window with no way back is how a
            // program becomes a thing you have to kill from a terminal.
            //
            // * With a tray icon, the window is HIDDEN. It leaves the taskbar
            //   entirely and lives in the status area, which is what "runs in
            //   the background" means to a desktop — and the icon is the way
            //   back.
            // * Without one — stock GNOME publishes no StatusNotifierWatcher —
            //   it is MINIMISED instead, staying in the taskbar where it is
            //   visible and one click from returning.
            //
            // Nothing else is needed to keep the process alive: the window is
            // never destroyed, so `GApplication` still has one and does not
            // exit. And the tracker still goes dark without this handler doing
            // anything about it — hiding unmaps, and the unmap handler releases
            // the claim; minimising leaves it mapped, and the 33 ms tick
            // releases it from `is_active()` a frame or two later.
            //
            // The gaze overlay is deliberately NOT closed here. It has its own
            // switch and its own claim; if the user left it on, something
            // genuinely wants data and the tracker is right to stay lit.
            if !quitting.get() {
                if tray_is_published() {
                    w.set_visible(false);
                } else {
                    w.minimize();
                }
                return glib::Propagation::Stop;
            }

            // From here down: an actual quit, from the Quit item.
            //
            // 1. The tracker claim. A hub closed while focused would otherwise
            //    leave one behind for the life of the process — which in
            //    background mode is until logout, i.e. the illuminators never
            //    go out again.
            *focus_hold.borrow_mut() = None;

            // 2. The gaze overlay. It is a second ApplicationWindow of the same
            //    GtkApplication, on layer-shell's Overlay layer, click-through
            //    and with no decorations — so closing the hub used to leave a
            //    full-screen surface on top of everything with no way to
            //    dismiss it, and a GtkApplication that never exits because a
            //    window remained.
            if let Some(w) = overlay_win.borrow_mut().take() {
                w.close();
            }

            // 3. The 33 ms tick. It captures the application and can open a
            //    forced setup or calibration flow, so left running it would
            //    keep waking on a destroyed hub and could raise a fullscreen
            //    flow after the user closed the window.
            if let Some(id) = tick_id.borrow_mut().take() {
                id.remove();
            }

            // 4. The tray icon, and then the application.
            //
            //    Neither is optional. `--background` holds a `GApplication`
            //    hold for the life of the process precisely so that having no
            //    window does not end it — which means destroying the last
            //    window does not end it either, and Quit left the process
            //    running with its tray icon still live and still able to open
            //    a new hub. Dropping the tray first keeps the icon from
            //    outliving the program that owns it by even a moment.
            TRAY.with(|c| *c.borrow_mut() = None);
            if let Some(app) = w.application() {
                app.quit();
            }
            glib::Propagation::Proceed
        });
    }

    window.present();
    Some(window)
}

/// Everything `decide()` reads, gathered in one place.
///
/// Called from the connection tick and again when a forced flow closes, so the
/// two cannot ask the question differently.
fn evaluate_calibration_state() -> tobii_config::CalAction {
    let setup = tobii_config::load().ok().flatten();
    let display_configured = setup.is_some();
    let fp = setup.map(|s| s.fingerprint()).unwrap_or(0);
    let cal = tobii_config::load_calibration()
        .ok()
        .flatten()
        .and_then(|(_, m)| m);
    let active = device::active_monitor_id();
    tobii_config::decide(display_configured, cal.as_ref(), active.as_deref(), fp)
}

/// Open a flow the user cannot skip (missing display setup or calibration):
/// disable the hub while it is open, and re-enable once it closes. Mirrors the
/// existing `b_cal`/`b_setup` single-flow-at-a-time pattern, but disables the
/// whole hub window rather than a single button, since a forced flow has no
/// button of its own to anchor to.
///
/// `forced_flow_open` is set for the lifetime of the window, so the tick's
/// `decide()` evaluation does not re-enter while a forced flow is already up
/// (e.g. a disconnect/reconnect glitch mid-setup). On close, `cal_evaluated`
/// is also reset so `decide()` re-runs on the very next tick while still
/// connected — this is what chains ForceSetup -> ForceCalibration within one
/// session (a fresh install has neither display config nor a calibration, and
/// completing setup must not wait for a reconnect before calibration is
/// forced too).
fn launch_forced(
    app: &Application,
    window: &ApplicationWindow,
    forced_flow_open: &Rc<Cell<bool>>,
    cal_evaluated: &Rc<Cell<bool>>,
    launched_for: tobii_config::CalAction,
    open: impl FnOnce(&Application) -> ApplicationWindow,
) {
    window.set_sensitive(false);
    forced_flow_open.set(true);
    let win = open(app);
    let hub = window.clone();
    let forced_flow_open = forced_flow_open.clone();
    let cal_evaluated = cal_evaluated.clone();
    win.connect_close_request(move |_| {
        hub.set_sensitive(true);
        forced_flow_open.set(false);
        // Re-open only if the answer has CHANGED.
        //
        // This used to reset the gate unconditionally, so the next 33 ms tick
        // re-ran `decide()` — and when the user had *cancelled* rather than
        // completed, it got the same answer and put the same window straight
        // back up, with the hub disabled behind it. There is no way out of that
        // except killing the process.
        //
        // The reset exists for a reason worth keeping: completing setup makes
        // `decide()` return ForceCalibration, which is how the two chain within
        // one session instead of waiting for a reconnect. Comparing the answers
        // keeps the chain and drops the loop — a cancel leaves the answer where
        // it was, so the gate stays latched until the next connection, which is
        // the disconnect branch's job.
        if evaluate_calibration_state() != launched_for {
            cal_evaluated.set(false);
        }
        glib::Propagation::Proceed
    });
}

/// Populate and show the "recommend recalibration" banner for `reason`.
fn show_recommend_banner(label: &Label, banner: &gtk::Box, reason: tobii_config::RecommendReason) {
    label.set_text(match reason {
        tobii_config::RecommendReason::OtherScreen => {
            "This calibration was made for a different screen. Recalibrate?"
        }
        tobii_config::RecommendReason::GeometryChanged => {
            "Display settings changed since calibration. Recalibrate?"
        }
    });
    banner.set_visible(true);
}

/// The cogwheel in the header, and the settings that are not about tracking.
///
/// # Why these three, and why not in the rack
///
/// The control rack answers one question per card — *how should the tracker
/// behave?* Autostart, the update check and the diagnostics report answer a
/// different one — *how should this program behave?* — and three cards' worth
/// of prose about login sessions and network requests pushed the instrument
/// panel down and made the hub taller than the thing it is monitoring. Moving
/// them behind a cogwheel gives the window back to what it is for and costs one
/// click for settings that are set once and then forgotten.
///
/// A `Popover`, not a second window: it is anchored to the button that opened
/// it, it closes on click-away, and it needs no title bar, no size negotiation
/// and no place in the window list for what is three rows of content.
fn settings_button(quitting: &Rc<Cell<bool>>) -> gtk::MenuButton {
    let popover = gtk::Popover::new();
    popover.set_child(Some(&settings_list(quitting)));
    popover.set_position(gtk::PositionType::Bottom);
    popover.set_has_arrow(false);
    // Right edge flush with the cogwheel's, not centred under it. A popover is
    // centred on its anchor by default, and the cogwheel is the last thing in
    // the header — so half of a 330px panel hung off the side of the window.
    // `halign` on the popover itself is what GTK4 offers for this; the widget
    // is aligned within the space its anchor allows rather than being centred
    // in it.
    popover.set_halign(Align::End);
    // A few pixels of air under the cogwheel. Flush against the button the
    // panel reads as part of it rather than as something it opened.
    popover.set_offset(0, 8);

    let btn = gtk::MenuButton::new();
    btn.add_css_class("icon-btn");
    btn.set_valign(Align::Center);
    btn.set_tooltip_text(Some("Settings"));
    btn.set_popover(Some(&popover));
    // A cogwheel, whichever icon theme is installed, or a glyph if none is.
    //
    // The name has to be checked rather than assumed twice over. GTK's lookup
    // falls back to a "missing image" square rather than to nothing, so a theme
    // without the name leaves a broken tile in the header. And the two obvious
    // names do not agree about what they draw: `preferences-system-symbolic` is
    // a cogwheel in Adwaita but a row of SLIDERS in Breeze — measured, by
    // running the hub on this machine, which is a Breeze desktop. Both themes
    // draw `emblem-system-symbolic` as a cogwheel (Adwaita keeps it under
    // symbolic/legacy/), so that is the first choice.
    let theme = gtk::gdk::Display::default().map(|d| gtk::IconTheme::for_display(&d));
    let cog = ["emblem-system-symbolic", "preferences-system-symbolic"]
        .into_iter()
        .find(|n| theme.as_ref().is_some_and(|t| t.has_icon(n)));
    match cog {
        Some(name) => btn.set_icon_name(name),
        None => {
            let label = Label::new(Some("⚙"));
            // The same vertical slack `widget::button` gives every glyph in
            // this hub: the ink of a tall one needs more room than its
            // logical box.
            label.set_margin_top(2);
            label.set_margin_bottom(2);
            btn.set_child(Some(&label));
        }
    }
    btn
}

/// One press of the text-size control, clamped to what the UI can render.
///
/// Free so the two ends of the range can be tested without a GTK widget.
fn stepped(current: f64, by: f64) -> f64 {
    (current + by).clamp(tobii_config::TEXT_SCALE_MIN, tobii_config::TEXT_SCALE_MAX)
}

/// The text-size control: minus, the current percentage, plus.
///
/// Buttons rather than a slider. The range that is useful is small and the
/// steps are meaningful, so a slider would offer a hundred positions to choose
/// between five — and a slider is the harder thing to hit for exactly the user
/// who is here because the text is too small to read.
fn text_size_row() -> gtk::Box {
    /// One press. Large enough to be worth pressing, small enough that the
    /// range is not crossed in two.
    const STEP: f64 = 0.1;

    let minus = crate::widget::button("−");
    minus.add_css_class("quiet");
    let plus = crate::widget::button("+");
    plus.add_css_class("quiet");
    let current = Label::new(None);
    current.add_css_class("status");
    current.set_width_chars(5);

    let controls = gtk::Box::new(Orientation::Horizontal, 6);
    controls.set_valign(Align::Center);
    controls.append(&minus);
    controls.append(&current);
    controls.append(&plus);

    // Shared by both buttons and the label, so the three cannot disagree about
    // what the current scale is.
    let refresh = {
        let current = current.clone();
        let minus = minus.clone();
        let plus = plus.clone();
        move |scale: f64| {
            current.set_text(&format!("{:.0}%", scale * 100.0));
            // Greyed at the ends rather than silently doing nothing, so a
            // press that cannot help says so before it is made.
            minus.set_sensitive(scale > tobii_config::TEXT_SCALE_MIN + f64::EPSILON);
            plus.set_sensitive(scale < tobii_config::TEXT_SCALE_MAX - f64::EPSILON);
        }
    };
    let saved = tobii_config::text_scale();
    refresh(saved);

    // The live value, held here rather than re-read from disk on every press.
    //
    // Reading it back meant the buttons only moved if the previous press had
    // been SAVED: with a read-only config directory the save fails, the next
    // press re-reads the old value, and the control appears stuck at one step
    // from where it started — while the text on screen had actually changed.
    // The save is best-effort; the setting is not.
    let current = std::rc::Rc::new(Cell::new(saved));
    let bump = {
        let refresh = refresh.clone();
        let current = current.clone();
        move |by: f64| {
            let next = stepped(current.get(), by);
            current.set(next);
            // Applied before it is saved: the change is visible instantly, and
            // a read-only config directory costs the user the persistence
            // rather than the feature.
            // Which also re-fits the window around it.
            apply_text_scale(next);
            if let Err(e) = tobii_config::save_text_scale(next) {
                tobii_diagnostics::log::warn(&format!("could not save the text size: {e}"));
            }
            refresh(next);
        }
    };
    {
        let bump = bump.clone();
        minus.connect_clicked(move |_| bump(-STEP));
    }
    plus.connect_clicked(move |_| bump(STEP));

    settings_row(
        "Text size",
        "Scales this window's text. The tracker, the games and everything else \
         are unaffected — this is only how large the program draws itself.",
        &controls,
    )
}

fn settings_list(quitting: &Rc<Cell<bool>>) -> gtk::Box {
    let list = gtk::Box::new(Orientation::Vertical, 4);
    // An explicit width rather than one negotiated from the longest sentence.
    // `set_max_width_chars` is a hint about where a label may wrap, not a cap
    // on what it may be allocated, so without this the panel's width is
    // whatever the prose happens to measure — and it changes whenever the
    // prose is edited.
    list.set_size_request(330, -1);
    list.set_margin_top(6);
    list.set_margin_bottom(6);
    list.set_margin_start(6);
    list.set_margin_end(6);

    list.append(&settings_row(
        "Start when I log in",
        // What this actually does, which is less than it used to claim. It said
        // "keeps the tracker set up from login, so it works in games and other
        // apps without opening this window first" — and that cannot be true
        // alongside the standby behaviour: `--background` opens no window, so
        // nothing ever takes a `Demand` hold, so the device is never opened and
        // the saved display area and calibration are never applied to anything.
        // A game does not need it either; `tobii headpose` opens the device and
        // re-applies the display area itself.
        "Starts this program at login with no window, so the hub opens instantly \
         and a second launch raises it instead of starting another copy. The \
         tracker itself stays off until something asks for it.",
        &autostart_switch(),
    ));
    list.append(&hairline());
    list.append(&settings_row(
        "Check for updates",
        "Asks GitHub for the latest release when this window opens. It's the only thing this \
         program does on the network without being asked. Nothing is downloaded until you \
         choose to update.",
        &update_check_switch(),
    ));
    list.append(&hairline());
    list.append(&text_size_row());
    list.append(&hairline());

    list.append(&diagnostics_row());
    list.append(&hairline());
    list.append(&quit_row(quitting));

    list
}

/// The way out, since the window's own X no longer is one.
///
/// A program that keeps running after you close its window has to say where
/// its exit went, and this is where somebody looks for it. The description is
/// not decoration: without it, the only way to discover that X does not quit is to
/// press X.
fn quit_row(quitting: &Rc<Cell<bool>>) -> gtk::Box {
    let btn = crate::widget::button("Quit");
    btn.add_css_class("quiet");
    btn.set_tooltip_text(Some(
        "Exit completely. Games stop receiving head tracking until this is \
         started again.",
    ));
    let quitting = quitting.clone();
    btn.connect_clicked(move |b| {
        // The flag is what tells the hub's close handler that this is a real
        // exit rather than the X button, so it tears down instead of
        // minimising. Closing the window rather than calling `app.quit()`
        // directly keeps ONE teardown path — the claim, the overlay and the
        // tick are all released there, and a second exit route would be a
        // second place to forget one of them.
        quitting.set(true);
        if let Some(w) = b.root().and_downcast::<gtk::Window>() {
            w.close();
        }
    });
    settings_row(
        "Quit",
        "Closing the window leaves it running, so games keep getting head \
         tracking while you play — it goes to the tray icon if your desktop has \
         one, and minimises if it does not. This exits for real.",
        &btn,
    )
}

/// One row of the settings popover: a title, why it exists, and its control.
///
/// Horizontal rather than [`section`]'s vertical stack — a popover is a narrow
/// column, and a switch under its own paragraph reads as a separate control
/// from the sentence above it.
fn settings_row<W: IsA<gtk::Widget>>(title: &str, desc: &str, control: &W) -> gtk::Box {
    let row = gtk::Box::new(Orientation::Horizontal, 12);
    row.set_margin_start(8);
    row.set_margin_end(8);
    row.set_margin_top(8);
    row.set_margin_bottom(8);

    let text = gtk::Box::new(Orientation::Vertical, 2);
    text.set_hexpand(true);
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    t.set_xalign(0.0);
    let d = Label::new(Some(desc));
    d.add_css_class("section-desc");
    d.set_halign(Align::Start);
    d.set_xalign(0.0);
    d.set_wrap(true);
    // A wrapping label reports its UNWRAPPED width as natural, so without a cap
    // the longest sentence sets the popover's width — the same trap `section`
    // documents, and a popover has even less room to give away.
    d.set_max_width_chars(34);
    text.append(&t);
    text.append(&d);

    control.set_valign(Align::Center);
    row.append(&text);
    row.append(control);
    row
}

/// A one-pixel rule between popover rows.
fn hairline() -> gtk::Box {
    let h = gtk::Box::new(Orientation::Horizontal, 0);
    h.add_css_class("hairline");
    h
}

/// The diagnostics row: save it, or copy it.
///
/// # Why two buttons and not one
///
/// This was one wide button that did both — it copied to the clipboard and
/// wrote a file, and reported the file path in its own label. That conflates
/// two different intentions. Somebody about to paste into a GitHub issue wants
/// the clipboard and nothing on disk; somebody being asked for the report in a
/// forum thread wants a file they can attach. Doing both on every click meant
/// the first case silently left a file behind and the second had to read a
/// path out of a button caption.
///
/// Icons rather than captions because the row already carries the sentence
/// that explains it, and two labelled buttons here would be wider than the
/// panel. Both have tooltips, which is where the detail about what the report
/// does and does not contain now lives.
fn diagnostics_row() -> gtk::Box {
    let row = gtk::Box::new(Orientation::Vertical, 0);

    // Where the outcome is reported. The buttons cannot say it themselves any
    // more — an icon has no caption to change — and a clipboard write is
    // otherwise completely invisible.
    let status = Label::new(None);
    status.add_css_class("hint");
    status.set_halign(Align::Start);
    status.set_xalign(0.0);
    status.set_wrap(true);
    status.set_max_width_chars(38);
    status.set_visible(false);
    status.set_margin_start(8);
    status.set_margin_end(8);
    status.set_margin_bottom(8);

    // A generation counter, so two clicks in a row do not leave the first
    // click's timer to clear the second click's message six seconds early.
    let generation = Rc::new(Cell::new(0u32));
    let say = {
        let status = status.clone();
        let generation = generation.clone();
        move |msg: String| {
            status.set_text(&msg);
            status.set_visible(true);
            let mine = generation.get().wrapping_add(1);
            generation.set(mine);
            let status = status.clone();
            let generation = generation.clone();
            glib::timeout_add_local_once(Duration::from_secs(6), move || {
                if generation.get() == mine {
                    status.set_visible(false);
                }
            });
        }
    };

    let save = icon_button(
        "document-save-symbolic",
        "🖫",
        "Save the report to a file, to attach it to a bug report.\n\nAsks you \
         where to put it.",
    );
    let copy = icon_button(
        "edit-copy-symbolic",
        "⧉",
        "Copy the report to the clipboard, to paste into an issue.\n\nVersions, \
         libraries, what is configured, and the recent log. No calibration data, \
         and your username, home path and monitor serial are left out or hashed.",
    );

    save.connect_clicked(move |b| {
        // Built before the dialog opens, not after it closes: the report
        // describes the state of the program, and that state should be the one
        // the user was looking at when they asked for it, not whatever it is
        // however long they spend choosing a folder.
        let text = tobii_diagnostics::report();
        let window = b.root().and_downcast::<gtk::Window>();

        let dialog = gtk::FileDialog::new();
        dialog.set_title("Save the diagnostics report");
        dialog.set_initial_name(Some("tobii-diagnostics.txt"));
        // Somewhere the user will find it again. The state directory is where
        // this used to write without asking, and it is not a place anybody
        // browses to.
        if let Some(home) = std::env::var_os("HOME") {
            dialog.set_initial_folder(Some(&gtk::gio::File::for_path(home)));
        }
        let parent = window.clone();
        dialog.save(window.as_ref(), gtk::gio::Cancellable::NONE, move |res| {
            // Cancelling is an answer, not a failure: the dialog reports a
            // DISMISSED error for it, and telling somebody their deliberate
            // cancel "failed" is noise.
            let Ok(file) = res else { return };
            let Some(path) = file.path() else { return };
            if let Err(e) = std::fs::write(&path, &text) {
                // Loud, because the file the user just named is not there and
                // nothing else would say so. The popover has closed by now —
                // the dialog took the focus — so the status line under the
                // buttons would never be seen.
                tobii_diagnostics::log::warn(&format!(
                    "could not write the diagnostics report as {}: {e}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
                let alert = gtk::AlertDialog::builder()
                    .message("The report could not be saved")
                    .detail(format!("{}: {e}", path.display()))
                    .build();
                alert.show(parent.as_ref());
            } else {
                // The FILE NAME, not the path. The user picked the location
                // and does not need telling; the log tail goes into the report,
                // and a save to a USB stick writes /run/media/<login>/... which
                // no redaction shape folds. Two guards close that, and this is
                // the one that does not depend on getting the shapes right.
                tobii_diagnostics::log::info(&format!(
                    "diagnostics report saved as {}",
                    path.file_name().unwrap_or_default().to_string_lossy()
                ));
            }
        });
    });
    copy.connect_clicked(move |b| {
        // `set_text` reports nothing, so whether the clipboard took it is
        // not observable from here. Saying "copied" is the best this can
        // honestly do.
        b.display()
            .clipboard()
            .set_text(&tobii_diagnostics::report());
        // "while this window is open" is the honest half. A Wayland
        // clipboard selection belongs to the process that set it: close the
        // hub and the selection goes with it, so a user who copies, closes
        // the hub and then pastes into a browser gets nothing. Save is the
        // button that survives.
        say("Copied — paste it before closing this window".to_string());
    });

    let buttons = gtk::Box::new(Orientation::Horizontal, 6);
    buttons.append(&save);
    buttons.append(&copy);

    row.append(&settings_row(
        "Diagnostics",
        "The report an issue asks for: versions, libraries, what is configured, and the \
         recent log.",
        &buttons,
    ));
    row.append(&status);
    row
}

/// A square, captionless button, with a glyph if the icon theme has no icon.
///
/// GTK's icon lookup falls back to a "missing image" square rather than to
/// nothing, so an unchecked name leaves a broken tile rather than a plain one.
fn icon_button(icon: &str, glyph: &str, tooltip: &str) -> gtk::Button {
    // The glyph fallback is built by `widget::button`, which is the one place
    // that knows how much vertical slack a tall glyph's ink needs here.
    // `set_icon_name` replaces that child when the theme does have the icon.
    let btn = crate::widget::button(glyph);
    if gtk::gdk::Display::default().is_some_and(|d| gtk::IconTheme::for_display(&d).has_icon(icon))
    {
        btn.set_icon_name(icon);
    }
    btn.add_css_class("quiet");
    btn.add_css_class("icon-btn");
    btn.set_valign(Align::Center);
    btn.set_tooltip_text(Some(tooltip));
    btn
}

/// The switch for starting the hub at login.
///
/// Writing the entry can fail — a read-only home directory, a full disk — and a
/// switch that silently slides back is worse than one that says why, so the
/// failure is put in the tooltip and the switch is returned to where it was.
fn autostart_switch() -> Switch {
    let sw = Switch::new();
    sw.set_valign(Align::Center);
    sw.set_active(autostart::is_enabled());
    sw.set_tooltip_text(Some(
        "Runs this program in the background at login, with no window.",
    ));
    sw.connect_state_set(|sw, on| {
        match autostart::set_enabled(on) {
            Ok(()) => {
                sw.set_tooltip_text(Some(if on {
                    "Runs this program in the background at login, with no window."
                } else {
                    "Not started at login."
                }));
                glib::Propagation::Proceed
            }
            Err(e) => {
                tobii_diagnostics::log::warn(&format!(
                    "could not change the start-at-login setting: {e}"
                ));
                sw.set_tooltip_text(Some(&format!("Could not save this setting: {e}")));
                // Refuse the change rather than showing a state that is not
                // what is on disk.
                glib::Propagation::Stop
            }
        }
    });
    sw
}

/// The switch that turns the launch-time release check on and off.
///
/// The check is the program's only unprompted network request, so it needs a
/// visible off switch — not just an environment variable a user would have to
/// already know about. `TOBII_NO_UPDATE_CHECK` still wins, and the switch is
/// shown insensitive when it does, because a control that silently does nothing
/// is worse than one that explains itself.
fn update_check_switch() -> Switch {
    let sw = Switch::new();
    sw.set_valign(Align::Center);
    let forced_off = std::env::var_os("TOBII_NO_UPDATE_CHECK").is_some_and(|v| v != "0");
    sw.set_active(tobii_config::update_check_enabled());
    if forced_off {
        sw.set_sensitive(false);
        sw.set_tooltip_text(Some(
            "Turned off for this session by TOBII_NO_UPDATE_CHECK.",
        ));
        return sw;
    }
    sw.set_tooltip_text(Some("Takes effect the next time this window opens."));
    sw.connect_state_set(|_, on| {
        if let Err(e) = tobii_config::save_update_check(on) {
            tobii_diagnostics::log::warn(&format!("could not save the update-check setting: {e}"));
        }
        glib::Propagation::Proceed
    });
    sw
}

/// One setting, as a card: what it is, what it does, and its control.
///
/// A card rather than a bare stack because the hub holds five co-equal
/// settings; stacked headings alone left the eye no boundary between them, so
/// the column read as one long paragraph with buttons in it.
fn section<W: IsA<gtk::Widget>>(title: &str, desc: &str, control: &W) -> gtk::Box {
    let b = gtk::Box::new(Orientation::Vertical, 6);
    b.add_css_class("surface");
    b.add_css_class("panel-pad");
    let t = Label::new(Some(title));
    t.add_css_class("section-title");
    t.set_halign(Align::Start);
    t.set_xalign(0.0);
    t.set_wrap(true);
    let d = Label::new(Some(desc));
    d.add_css_class("section-desc");
    d.set_halign(Align::Start);
    d.set_xalign(0.0);
    d.set_wrap(true);
    // A wrapping label reports its UNWRAPPED width as natural, so without a cap
    // the longest sentence in the column sets the column's width — and the
    // control rack grew until it crowded the instrument beside it.
    d.set_max_width_chars(44);
    control.set_halign(Align::Start);
    control.set_margin_top(6);
    b.append(&t);
    b.append(&d);
    b.append(control);
    b
}

#[cfg(test)]
mod tests {
    /// Scaling rewrites the sizes themselves, because they are absolute px and
    /// an absolute size is exactly what will not inherit from a root rule.
    #[test]
    fn the_stylesheet_scales_every_font_size_it_states() {
        let css = ".a { font-size: 20px; } .b { color: red; font-size: 10px; }";
        assert_eq!(
            super::scaled_css(css, 1.5),
            ".a { font-size: 30px; } .b { color: red; font-size: 15px; }"
        );
        // Identity is byte-for-byte untouched, so the default look cannot be
        // changed by a rounding decision made for the scaled path.
        assert_eq!(super::scaled_css(css, 1.0), css);
        assert_eq!(super::scaled_css(css, f64::NAN), css);
    }

    /// The design's proportions must survive scaling: a 20px title against a
    /// 10px eyebrow is the hierarchy the layout is read by, and losing it at
    /// anything but 100% would make the setting cost more than it gives.
    #[test]
    fn scaling_preserves_the_relative_sizes_and_never_rounds_to_nothing() {
        let size_of = |css: &str| -> f64 {
            css.split("font-size: ")
                .nth(1)
                .and_then(|t| t.split("px").next())
                .and_then(|n| n.parse().ok())
                .expect("a px size")
        };
        for scale in [0.8, 1.0, 1.2, 1.6] {
            let title = size_of(&super::scaled_css(".t { font-size: 20px; }", scale));
            let eyebrow = size_of(&super::scaled_css(".e { font-size: 10px; }", scale));
            assert!(
                (title / eyebrow - 2.0).abs() < 0.35,
                "the 2:1 ratio must survive scale {scale}: {title} vs {eyebrow}"
            );

            // And the real sheet, size by size, against what each one should
            // become. Asserting only that the results are positive passed just
            // as well when nothing had been scaled at all.
            let sizes = |css: &str| -> Vec<i64> {
                css.match_indices("font-size: ")
                    .filter_map(|(i, k)| {
                        let t = &css[i + k.len()..];
                        let d: String = t.chars().take_while(char::is_ascii_digit).collect();
                        t[d.len()..].starts_with("px").then(|| d.parse().ok())?
                    })
                    .collect()
            };
            let before = sizes(super::CSS);
            let after = sizes(&super::scaled_css(super::CSS, scale));
            assert_eq!(before.len(), after.len(), "a rule went missing at {scale}");
            assert!(!before.is_empty(), "the sheet states px sizes");
            for (b, a) in before.iter().zip(&after) {
                assert_eq!(
                    *a,
                    ((*b as f64 * scale).round() as i64).max(1),
                    "{b}px at {scale}"
                );
            }
        }
    }

    /// The floor is for sheets this one is not: every size the hub states is
    /// 10px or more, so even `TEXT_SCALE_MIN` leaves 8px and the clamp never
    /// fires. It is here so a 1px rule added later degrades to invisible-but-
    /// present rather than to `font-size: 0px`, which GTK rejects — taking the
    /// whole stylesheet, not just that rule, with it.
    #[test]
    fn a_size_never_scales_away_to_nothing() {
        assert_eq!(
            super::scaled_css(".a { font-size: 1px; }", 0.1),
            ".a { font-size: 1px; }"
        );
        assert!(super::CSS.match_indices("font-size: ").all(|(i, k)| {
            let t = &super::CSS[i + k.len()..];
            let d: String = t.chars().take_while(char::is_ascii_digit).collect();
            d.parse::<f64>()
                .is_ok_and(|px| px * tobii_config::TEXT_SCALE_MIN >= 8.0)
        }));
    }

    /// The ends of the text-size range, without a GTK widget — which is the
    /// reason `stepped` is a free function rather than a closure in the popover.
    #[test]
    fn the_text_size_steps_stop_at_both_ends() {
        use tobii_config::{TEXT_SCALE_MAX, TEXT_SCALE_MIN};
        const STEP: f64 = 0.1;
        assert_eq!(super::stepped(1.0, STEP), 1.1);
        assert_eq!(super::stepped(TEXT_SCALE_MAX, STEP), TEXT_SCALE_MAX);
        assert_eq!(super::stepped(TEXT_SCALE_MIN, -STEP), TEXT_SCALE_MIN);
        // Pressed from an out-of-range value — a hand-edited preference file —
        // the next press is in range rather than one step further out.
        assert_eq!(super::stepped(9.0, STEP), TEXT_SCALE_MAX);
        assert_eq!(super::stepped(0.0, -STEP), TEXT_SCALE_MIN);
    }

    /// A size in a unit this does not understand is copied through, not guessed
    /// at: silently reinterpreting `1.2em` as pixels would be a worse bug than
    /// not scaling it.
    #[test]
    fn a_size_in_another_unit_is_left_alone() {
        let css = ".a { font-size: 1.2em; } .b { font-size: 12px; }";
        assert_eq!(
            super::scaled_css(css, 2.0),
            ".a { font-size: 1.2em; } .b { font-size: 24px; }"
        );
    }
}
