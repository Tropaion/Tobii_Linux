//! The tray icon — a StatusNotifierItem published on the session bus.
//!
//! The hub is a program that owns a device: it has to outlive its own window,
//! because a game needs head tracking long after the settings have been put
//! away. Dismissing the window therefore cannot end the process — and a process
//! with no window and no icon is one the user cannot get back. This module is
//! the way back: an icon in the system tray whose click raises the hub again,
//! and the only visible sign of the hub when it is autostarted at login with no
//! window at all.
//!
//! # Why this is written out against D-Bus by hand
//!
//! There is no XEmbed system tray under Wayland. What Plasma reads instead —
//! and with it waybar, xfce4-panel, LXQt, and GNOME via the AppIndicator
//! extension — is StatusNotifierItem: an object the application exports on the
//! session bus, announced to a `StatusNotifierWatcher` that the desktop
//! publishes. The part of that protocol worth having is seven properties, four
//! methods and a handful of change signals, which is small enough to write
//! directly against `gio`'s D-Bus API. `gio` is already in the tree underneath
//! GTK, whereas every tray *library* on offer (libappindicator,
//! libayatana-appindicator, ksni's zbus stack) is either a C dependency to
//! package on each distro this ships to or a second async runtime to carry.
//!
//! # No tray menu, deliberately
//!
//! `ItemIsMenu` is published as false and no `Menu` property is published at
//! all. A tray menu is not part of this interface: it is `com.canonical.dbusmenu`,
//! a second exported object with its own layout-revision, item-property and
//! event model. That is a large amount of surface to carry for one "Quit"
//! entry, and the hub already has Quit in its cogwheel popover. Right-click and
//! middle-click therefore raise the window just as left-click does — see
//! [`raises_the_window`] — a click that does nothing reads as a broken icon.
//!
//! # Threading
//!
//! Everything here runs on the GLib main context that is thread-default when
//! [`install`] is called, so [`install`] must be called from the GTK main
//! thread: that is what makes it safe for `on_activate` to touch widgets.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use gtk::gio;
use gtk::glib;
use gtk::prelude::*;

use crate::APP_ID;

/// The interface hosts look for, and the object it lives on.
///
/// `org.kde.*` rather than `org.freedesktop.*`: the spec was written for
/// freedesktop but never adopted there, so KDE's original names are what the
/// interface is still called everywhere. Only the *bus name* below varies —
/// applications publish items under both prefixes, and the same item exports
/// this interface either way.
const ITEM_INTERFACE: &str = "org.kde.StatusNotifierItem";
const ITEM_PATH: &str = "/StatusNotifierItem";
const WATCHER_NAME: &str = "org.kde.StatusNotifierWatcher";
const WATCHER_PATH: &str = "/StatusNotifierWatcher";

/// What the tray shows under the icon, and what a host with no `ToolTip` falls
/// back to. The same string as `Name` in `assets/com.tobiilinux.Configuration.desktop`,
/// so the tray and the application menu agree on what this program is called.
const TITLE: &str = "Tobii Eye Tracker";

/// The icon theme name.
///
/// The same string as [`APP_ID`] because that is what `Icon=` in
/// `assets/com.tobiilinux.Configuration.desktop` names, and
/// `scripts/install-payload.sh` installs `com.tobiilinux.Configuration.svg`
/// into `…/icons/hicolor/scalable/apps`. A host looks for it in `IconThemePath`
/// first — see [`private_icon_theme`], which exists because Plasma could not
/// find the installed copy for a whole session after a first install. It is
/// still not sent over the bus as pixels: rendering the SVG takes an image
/// loader this machine does not have (checked: no SVG loader for gdk-pixbuf,
/// and GdkTexture refuses the file).
const ICON_NAME: &str = APP_ID;

/// How long to wait for either of the two calls this module makes.
///
/// Both go over the session bus's local socket to a process that is sitting in
/// its own main loop, which answers in well under a millisecond; a wait worth
/// measuring means something is already wrong. The bound is there so that when
/// something is, the hub pays two seconds rather than GDBus's 25-second
/// default.
const CALL_TIMEOUT_MS: i32 = 2_000;

/// The interface, as the hosts' introspection expects to find it.
///
/// Only what is implemented is declared, and this list is load-bearing in one
/// direction only. GDBus answers `Get`/`GetAll` and `Introspect` from it and
/// refuses what is absent — an undeclared property or method is rejected
/// before the closures below ever run — but it does NOT check what the getter
/// hands back against the signature declared here. A property declared with no
/// arm in [`property`] therefore goes out as an empty string under a type the
/// host was promised, and neither side says a word: the host simply reads
/// nonsense or nothing. There is no runtime check to catch that, which is why
/// the test module holds the two lists together instead.
const ITEM_XML: &str = r#"
<node>
  <interface name="org.kde.StatusNotifierItem">
    <method name="ContextMenu">
      <arg name="x" type="i" direction="in"/>
      <arg name="y" type="i" direction="in"/>
    </method>
    <method name="Activate">
      <arg name="x" type="i" direction="in"/>
      <arg name="y" type="i" direction="in"/>
    </method>
    <method name="SecondaryActivate">
      <arg name="x" type="i" direction="in"/>
      <arg name="y" type="i" direction="in"/>
    </method>
    <method name="Scroll">
      <arg name="delta" type="i" direction="in"/>
      <arg name="orientation" type="s" direction="in"/>
    </method>
    <property name="Category" type="s" access="read"/>
    <property name="Id" type="s" access="read"/>
    <property name="Title" type="s" access="read"/>
    <property name="Status" type="s" access="read"/>
    <property name="IconName" type="s" access="read"/>
    <property name="IconThemePath" type="s" access="read"/>
    <property name="ItemIsMenu" type="b" access="read"/>
    <property name="ToolTip" type="(sa(iiay)ss)" access="read"/>
    <signal name="NewTitle"/>
    <signal name="NewIcon"/>
    <signal name="NewStatus">
      <arg name="status" type="s"/>
    </signal>
    <signal name="NewToolTip"/>
  </interface>
</node>
"#;

/// Whether a method call from a host means "bring the hub back".
///
/// Right-click (`ContextMenu`) and middle-click (`SecondaryActivate`) raise the
/// window like a left-click, because this item publishes no menu for a
/// right-click to open — see the module documentation. Everything else is
/// answered and otherwise ignored; in practice that is only `Scroll`, since
/// GDBus rejects any method not in [`ITEM_XML`] before this runs.
fn raises_the_window(method: &str) -> bool {
    matches!(method, "Activate" | "SecondaryActivate" | "ContextMenu")
}

/// The value of one `org.kde.StatusNotifierItem` property.
///
/// `None` for a name this interface does not declare, which GDBus never asks
/// for: it rejects properties absent from [`ITEM_XML`] before the getter runs.
fn property(name: &str, tooltip: &str, icon_theme_path: &str) -> Option<glib::Variant> {
    Some(match name {
        // The category describes what the item IS, not what it watches. This
        // is an application the user opens; "Hardware", the tempting one for a
        // program that talks to a device, is for readouts of the machine
        // itself — battery, temperature, disk.
        "Category" => "ApplicationStatus".to_variant(),
        // The application id, so a host that looks for a desktop entry by this
        // name finds com.tobiilinux.Configuration.desktop and can offer its
        // actions.
        "Id" => APP_ID.to_variant(),
        "Title" => TITLE.to_variant(),
        // Never "Passive": a passive item is one the host is free to hide, and
        // an icon that hides itself is exactly the way back that this module
        // exists to provide.
        "Status" => "Active".to_variant(),
        "IconName" => ICON_NAME.to_variant(),
        // Where the host should look for that name before its own theme — see
        // `private_icon_theme`. Empty when it could not be written, which a host
        // reads as "no path" and falls back to its own lookup.
        "IconThemePath" => icon_theme_path.to_variant(),
        "ItemIsMenu" => false.to_variant(),
        "ToolTip" => tooltip_variant(tooltip),
        _ => return None,
    })
}

/// The icon, compiled in, so the tray never depends on where — or whether — it
/// was installed.
const ICON_SVG: &[u8] = include_bytes!("../../../assets/com.tobiilinux.Configuration.svg");

/// Put the icon where `IconThemePath` can point at it, and return that path.
///
/// Measured on 2026-09-11: Plasma's icon lookup scans the icon folders that
/// existed when it started, and no others. A first tarball install creates
/// `~/.local/share/icons/hicolor/scalable/` after Plasma has started, so the
/// tray showed a placeholder until the next login — restarting plasmashell fixed
/// it. `IconThemePath` makes the host look in a folder named by the item, and
/// this writes the icon into it at every start, so the icon should no longer
/// depend on the install location, and a build run straight from the tree gets
/// the real icon too. Plasma was seen reading `IconThemePath`; that it then
/// draws the icon after a first install has not been confirmed — see
/// Quality-and-Risks 11.3c.
///
/// Under the runtime directory the tracking socket already uses: per user, mode
/// 0700, on tmpfs, gone at logout.
fn private_icon_theme() -> Option<PathBuf> {
    let base = tobii_ipc::path::ensure_socket_dir().ok()?;
    write_icon_theme(&base).ok()
}

/// [`private_icon_theme`] against any directory, so a test can use its own.
///
/// Both layouts a host might search: the theme-shaped
/// `hicolor/scalable/apps/<name>.svg`, with the `index.theme` a Qt lookup needs
/// before it will look inside a theme folder at all, and the flat
/// `<path>/<name>.svg` some hosts look for directly.
fn write_icon_theme(base: &Path) -> std::io::Result<PathBuf> {
    let theme = base.join("icons");
    let hicolor = theme.join("hicolor");
    let apps = hicolor.join("scalable/apps");
    std::fs::create_dir_all(&apps)?;
    let files: [(PathBuf, &[u8]); 3] = [
        (apps.join(format!("{ICON_NAME}.svg")), ICON_SVG),
        (theme.join(format!("{ICON_NAME}.svg")), ICON_SVG),
        (
            hicolor.join("index.theme"),
            b"[Icon Theme]\nName=Hicolor\nComment=tobii-linux tray icon\nDirectories=scalable/apps\n\n\
              [scalable/apps]\nSize=128\nMinSize=8\nMaxSize=512\nType=Scalable\nContext=Applications\n",
        ),
    ];
    for (file, bytes) in files {
        // Rewritten only when it differs, so a second hub starting does not
        // replace a file a host may be reading at that moment.
        if std::fs::read(&file).ok().as_deref() != Some(bytes) {
            let tmp = file.with_extension(format!("new-{}", std::process::id()));
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &file)?;
        }
    }
    Ok(theme)
}

/// A `ToolTip` value: icon name, icon pixmaps, title, description.
///
/// The icon name is left empty so the host reuses the item's own icon, and the
/// text goes in the title rather than the description because a host that shows
/// only one of the two shows the title.
fn tooltip_variant(text: &str) -> glib::Variant {
    // Typed from Rust rather than from a parsed type string: `(i32, i32,
    // Vec<u8>)` *is* `(iiay)`, so an empty pixmap array cannot be built with
    // the wrong element type and cannot panic building it.
    let pixmaps = glib::Variant::array_from_iter::<(i32, i32, Vec<u8>)>(std::iter::empty());
    glib::Variant::tuple_from_iter(["".to_variant(), pixmaps, text.to_variant(), "".to_variant()])
}

/// The bus name this item is published under.
///
/// The `-<pid>-<n>` shape is the spec's, and the pid makes it unique without
/// asking the bus for anything; `1` because this process publishes exactly one
/// item. Convention rather than enforcement — KDE's watcher was observed
/// holding an item registered under a bare unique name — but conforming costs
/// nothing and is what a host is entitled to assume.
fn bus_name() -> String {
    format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
}

/// Everything the D-Bus callbacks share.
///
/// `Rc`, not `Arc`: every one of those callbacks is dispatched on the main
/// context this was built on, and none of them is `Send`.
struct State {
    bus_name: String,
    /// The hover text, read back by the `ToolTip` getter.
    tooltip: String,
    /// The `IconThemePath` value, fixed at install.
    icon_theme_path: String,
    /// Whether the bus name is ours, and whether a watcher is up.
    ///
    /// The item can only be announced once both hold, and either can become
    /// true first — the name is acquired asynchronously and the watcher may
    /// appear at any time, including after a panel restart. So both transitions
    /// call [`State::announce`] and it decides.
    have_name: Cell<bool>,
    watcher_up: Cell<bool>,
}

impl State {
    fn announce(&self, conn: &gio::DBusConnection) {
        if !self.have_name.get() || !self.watcher_up.get() {
            return;
        }
        conn.call(
            Some(WATCHER_NAME),
            WATCHER_PATH,
            // The watcher's interface name and its bus name are the same
            // string; this is not a copy-paste slip.
            WATCHER_NAME,
            "RegisterStatusNotifierItem",
            Some(&glib::Variant::tuple_from_iter([self
                .bus_name
                .to_variant()])),
            None,
            gio::DBusCallFlags::NONE,
            CALL_TIMEOUT_MS,
            gio::Cancellable::NONE,
            // Nothing to do either way: the icon appears or it does not, and
            // there is no second thing to try. A watcher that comes back later
            // re-announces through the NameOwnerChanged subscription below.
            |_reply| {},
        );
    }
}

/// A published tray icon. Dropping it takes the icon away.
pub struct Tray {
    conn: gio::DBusConnection,
    state: Rc<State>,
    /// `Option` only so that [`Drop`] can take them: the ids are move-only and
    /// each teardown call consumes one.
    registration: Option<gio::RegistrationId>,
    owner: Option<gio::OwnerId>,
    /// Held for its `Drop`, which unsubscribes; never read.
    _watcher: gio::SignalSubscription,
}

impl Tray {
    /// Whether a watcher is up right now, and therefore whether the icon is
    /// somewhere the user can see it.
    ///
    /// Asked rather than remembered, because the answer changes during a
    /// session in both directions: a panel started after this program puts a
    /// watcher up, and a panel that crashes takes one away. A caller that
    /// hides its window into the status area has to know which is true at the
    /// moment it hides, not at the moment this was installed.
    pub fn is_published(&self) -> bool {
        self.state.watcher_up.get()
    }
}

impl Drop for Tray {
    fn drop(&mut self) {
        // Before unregistering the object: dropping the name is what tells the
        // watcher the item is gone, and a host that reacts by reading one last
        // property should still find something to read.
        if let Some(id) = self.owner.take() {
            gio::bus_unown_name(id);
        }
        if let Some(id) = self.registration.take() {
            let _ = self.conn.unregister_object(id);
        }
    }
}

/// Publish a StatusNotifierItem for this application.
///
/// `tooltip` is the hover text. `on_activate` is invoked on the GTK main thread
/// when the user clicks the icon.
///
/// Call this from the GTK main thread, and hold the returned [`Tray`] for as
/// long as the icon should exist.
///
/// # What `None` means, exactly
///
/// Only that there is no session bus to publish on, or that GDBus refused the
/// object — both of which mean no icon is possible at all. It does NOT mean no
/// host is watching: that question is [`Tray::is_published`], and it is asked
/// rather than answered once because the answer changes during a session.
///
/// So on stock GNOME, which publishes no watcher, this still returns `Some`
/// and holds a bus name nobody reads. That is two round trips and a few
/// hundred bytes, and it buys the case that matters: a panel started after
/// this program — a tiling compositor launching a bar from its own config, an
/// AppIndicator extension switched on mid-session — gets the icon anyway. The
/// subscription that handles a panel *restart* is the same one that handles a
/// panel *arrival*; returning `None` early was the only thing that made the
/// second case special.
pub fn install(tooltip: &str, on_activate: impl Fn() + 'static) -> Option<Tray> {
    // Blocking, but only as far as the session bus GTK itself is already
    // connected to: GLib caches one connection per bus type, so with the
    // application registered this hands back the connection it is already
    // using. With no session bus at all it fails here and the hub starts
    // without a tray.
    let conn = gio::bus_get_sync(gio::BusType::Session, gio::Cancellable::NONE).ok()?;

    let node = gio::DBusNodeInfo::for_xml(ITEM_XML).ok()?;
    let interface = node.lookup_interface(ITEM_INTERFACE)?;

    let state = Rc::new(State {
        bus_name: bus_name(),
        tooltip: tooltip.to_owned(),
        icon_theme_path: private_icon_theme()
            .map(|p| p.display().to_string())
            .unwrap_or_default(),
        have_name: Cell::new(false),
        // Seeded from the bus daemon, then kept honest by the subscription
        // below. Either order is fine: `announce` fires on whichever of the
        // two becomes true last.
        watcher_up: Cell::new(watcher_is_up(&conn)),
    });

    let registration = conn
        .register_object(ITEM_PATH, &interface)
        .property({
            let state = state.clone();
            move |_conn, _sender, _path, _interface, name| {
                // The fallback is unreachable — GDBus rejects any name absent
                // from ITEM_XML before calling this — and exists because the
                // signature has no room to say so.
                property(name, &state.tooltip, &state.icon_theme_path)
                    .unwrap_or_else(|| "".to_variant())
            }
        })
        .method_call(
            move |_conn, _sender, _path, _interface, method, _args, invocation| {
                if raises_the_window(method) {
                    on_activate();
                }
                // Every declared method must be answered, including the ones
                // that do nothing: an unanswered call leaves the host blocked
                // until its own timeout expires.
                invocation.return_result(Ok(None));
            },
        )
        .build()
        .ok()?;

    // Owning the name before announcing it: the watcher resolves the name the
    // moment it is told about it, and announcing one nobody owns yet is how an
    // item ends up registered and invisible.
    let owner = gio::bus_own_name_on_connection(
        &conn,
        &state.bus_name,
        gio::BusNameOwnerFlags::NONE,
        {
            let state = state.clone();
            move |conn, _name| {
                state.have_name.set(true);
                state.announce(&conn);
            }
        },
        {
            let state = state.clone();
            move |_conn, _name| state.have_name.set(false)
        },
    );

    // A panel restart is routine on Plasma, and the watcher goes with it. This
    // is what puts the icon back afterwards: the new watcher starts with an
    // empty register, so every item has to announce itself again or it is gone
    // for the rest of the session.
    let _watcher = conn.subscribe_to_signal(
        Some("org.freedesktop.DBus"),
        Some("org.freedesktop.DBus"),
        Some("NameOwnerChanged"),
        Some("/org/freedesktop/DBus"),
        Some(WATCHER_NAME),
        gio::DBusSignalFlags::NONE,
        {
            let state = state.clone();
            move |signal| {
                // (name, old owner, new owner); an empty new owner means the
                // name was released rather than taken over.
                let new_owner = signal.parameters.child_value(2);
                let up = new_owner.str().is_some_and(|s| !s.is_empty());
                state.watcher_up.set(up);
                if up {
                    state.announce(signal.connection);
                }
            }
        },
    );

    Some(Tray {
        conn,
        state,
        registration: Some(registration),
        owner: Some(owner),
        _watcher,
    })
}

/// Whether anything owns the watcher name at this moment.
///
/// Asked of the bus daemon rather than of the watcher, because the daemon is
/// the one process guaranteed to be there to answer.
fn watcher_is_up(conn: &gio::DBusConnection) -> bool {
    let reply = conn.call_sync(
        Some("org.freedesktop.DBus"),
        "/org/freedesktop/DBus",
        "org.freedesktop.DBus",
        "NameHasOwner",
        Some(&glib::Variant::tuple_from_iter([WATCHER_NAME.to_variant()])),
        Some(glib::VariantTy::new("(b)").expect("a literal D-Bus type string")),
        gio::DBusCallFlags::NONE,
        CALL_TIMEOUT_MS,
        gio::Cancellable::NONE,
    );
    reply
        .ok()
        .and_then(|r| r.child_value(0).get::<bool>())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The attribute values of every `<tag ` element in [`ITEM_XML`].
    ///
    /// Enough of a parser for XML this module owns, and no more. gio exposes no
    /// way to walk a parsed `DBusInterfaceInfo`'s members from Rust — only
    /// `lookup_*` by name — so the declarations have to be read out of the text
    /// to be checked against the code that answers them.
    fn declared(tag: &str, attr: &str) -> Vec<String> {
        ITEM_XML
            .split(&format!("<{tag} "))
            .skip(1)
            .filter_map(|el| {
                let rest = el.split_once(&format!("{attr}=\""))?.1;
                Some(rest.split_once('"')?.0.to_owned())
            })
            .collect()
    }

    /// The parser above finding nothing would make every test using it pass
    /// vacuously, so the counts are pinned here.
    #[test]
    fn the_interface_declares_the_members_hosts_call() {
        let node = gio::DBusNodeInfo::for_xml(ITEM_XML).expect("the interface XML must parse");
        let iface = node
            .lookup_interface(ITEM_INTERFACE)
            .expect("the interface hosts look for");
        assert_eq!(iface.name(), ITEM_INTERFACE);

        let methods = declared("method", "name");
        assert_eq!(
            methods,
            ["ContextMenu", "Activate", "SecondaryActivate", "Scroll"],
            "the four methods a host may call"
        );
        assert_eq!(declared("property", "name").len(), 8);
        for m in &methods {
            assert!(
                iface.lookup_method(m).is_some(),
                "{m} is declared in the text but not in the parsed interface"
            );
        }
    }

    /// Nothing at runtime checks this. GDBus was measured handing a string
    /// straight through for a property declared `b`, with no warning on either
    /// side of the bus — so a getter that answers with the wrong type is a
    /// property the host silently misreads, and this test is the only thing
    /// standing between that and a shipped build.
    #[test]
    fn every_declared_property_is_answered_with_the_type_it_declares() {
        let names = declared("property", "name");
        let types = declared("property", "type");
        assert_eq!(names.len(), types.len());
        for (name, ty) in names.iter().zip(&types) {
            let value = property(name, "hover text", "/run/user/1000/tobii-linux/icons")
                .unwrap_or_else(|| panic!("{name} is declared but never answered"));
            assert_eq!(
                value.type_().as_str(),
                ty,
                "{name} is declared as {ty} but answered as {}",
                value.type_().as_str()
            );
        }
    }

    /// The hub's own state line is put here, so it has to survive the trip.
    #[test]
    fn the_tooltip_carries_the_text_a_host_shows() {
        let v = tooltip_variant("Tobii Eye Tracker 5 — tracker off");
        assert_eq!(v.type_().as_str(), "(sa(iiay)ss)");
        assert_eq!(
            v.child_value(2).get::<String>().as_deref(),
            Some("Tobii Eye Tracker 5 — tracker off"),
            "the text belongs in the title field, which is the one hosts show"
        );
        assert_eq!(
            v.child_value(1).n_children(),
            0,
            "no pixmaps: the icon is named, not sent"
        );
    }

    /// The files `IconThemePath` points a host at, in both layouts, byte for byte
    /// the shipped icon — and a second start leaves them as they are.
    #[test]
    fn the_private_icon_theme_holds_the_shipped_icon() {
        let base = std::env::temp_dir().join(format!("tobii-tray-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let theme = write_icon_theme(&base).expect("writable temp dir");
        assert_eq!(theme, base.join("icons"));
        for f in [
            theme.join(format!("hicolor/scalable/apps/{ICON_NAME}.svg")),
            theme.join(format!("{ICON_NAME}.svg")),
        ] {
            assert_eq!(
                std::fs::read(&f).expect("icon written"),
                ICON_SVG,
                "{}",
                f.display()
            );
        }
        let index = std::fs::read_to_string(theme.join("hicolor/index.theme")).expect("index");
        assert!(index.contains("Directories=scalable/apps"), "{index}");
        // A second start must leave the files as they are: a rename makes a new
        // inode, and a host may be reading the old one at that moment.
        use std::os::unix::fs::MetadataExt;
        let files = [
            theme.join(format!("hicolor/scalable/apps/{ICON_NAME}.svg")),
            theme.join(format!("{ICON_NAME}.svg")),
            theme.join("hicolor/index.theme"),
        ];
        let inodes = || -> Vec<u64> {
            files
                .iter()
                .map(|f| std::fs::metadata(f).expect("written").ino())
                .collect()
        };
        let before = inodes();
        assert_eq!(write_icon_theme(&base).expect("second start"), theme);
        assert_eq!(inodes(), before, "a second start replaced the files");
        let _ = std::fs::remove_dir_all(&base);
    }

    /// No published menu means a right-click has nothing to open, so it must
    /// raise the window instead of doing nothing.
    #[test]
    fn every_click_a_host_reports_raises_the_window() {
        for m in ["Activate", "SecondaryActivate", "ContextMenu"] {
            assert!(raises_the_window(m), "{m} must raise the hub");
        }
        assert!(!raises_the_window("Scroll"));
    }

    /// The shape is convention; the assertion that the bus would accept the
    /// name is not. A name the daemon rejects is never acquired — GLib says so
    /// on stderr, where a GUI user never looks — [`install`] still returns a
    /// [`Tray`], nothing is ever announced to the watcher, and the icon simply
    /// never appears.
    #[test]
    fn the_bus_name_is_the_one_watchers_expect() {
        let name = bus_name();
        assert_eq!(
            name,
            format!("org.kde.StatusNotifierItem-{}-1", std::process::id())
        );
        // And it is a name the bus will let us own at all.
        assert!(gio::dbus_is_name(&name));
        assert!(!gio::dbus_is_unique_name(&name));
    }
}
