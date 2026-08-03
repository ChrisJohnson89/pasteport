//! GTK4 front end for Pasteport.
//!
//! A search-first window: type to filter, Enter to copy the selection back to
//! the clipboard. The same interaction model as the macOS app, because muscle
//! memory should transfer between them.
//!
//! Needs system GTK4 development headers, which is why this crate sits outside
//! the root workspace:
//!
//! ```text
//! sudo apt install libgtk-4-dev
//! cargo build --manifest-path crates/pasteport-gtk/Cargo.toml
//! ```

mod client;

use std::cell::RefCell;
use std::rc::Rc;

use gtk::gdk;
use gtk::glib;
use gtk::prelude::*;
use gtk::{Application, ApplicationWindow};

use client::Engine;

const APP_ID: &str = "com.pasteport.Pasteport";
const PAGE_SIZE: usize = 200;

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("pasteport_gtk=info")),
        )
        .init();

    let app = Application::builder().application_id(APP_ID).build();
    app.connect_activate(build_ui);

    // GTK would otherwise try to parse our own CLI arguments as its own.
    app.run_with_args::<&str>(&[])
}

fn build_ui(app: &Application) {
    let socket = match pasteport_core::paths::socket_path() {
        Ok(p) => p,
        Err(e) => {
            show_fatal(app, &format!("Could not determine the Pasteport socket path: {e}"));
            return;
        }
    };
    let engine = Rc::new(Engine::new(socket));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Pasteport")
        .default_width(720)
        .default_height(560)
        .build();

    let root = gtk::Box::new(gtk::Orientation::Vertical, 0);

    // ---- search bar ----
    let search = gtk::SearchEntry::builder()
        .placeholder_text("Search clipboard history")
        .margin_top(8)
        .margin_bottom(8)
        .margin_start(8)
        .margin_end(8)
        .build();
    root.append(&search);

    // ---- results list ----
    let list = gtk::ListBox::builder()
        .selection_mode(gtk::SelectionMode::Single)
        .build();
    let scroller = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vexpand(true)
        .child(&list)
        .build();
    root.append(&scroller);

    // ---- status bar ----
    let status = gtk::Label::builder()
        .xalign(0.0)
        .margin_top(6)
        .margin_bottom(6)
        .margin_start(10)
        .margin_end(10)
        .build();
    status.add_css_class("dim-label");
    root.append(&status);

    window.set_child(Some(&root));

    // Clip ids in display order, so keyboard actions know what is selected
    // without reaching back into the widget tree.
    let visible: Rc<RefCell<Vec<i64>>> = Rc::new(RefCell::new(Vec::new()));

    // Takes the query as an argument rather than capturing the search entry.
    // Capturing it would make a cycle: the entry owns the signal handler, the
    // handler owns this Rc, and this Rc would own the entry.
    let refresh: Rc<dyn Fn(&str)> = {
        let engine = Rc::clone(&engine);
        let list = list.clone();
        let status = status.clone();
        let visible = Rc::clone(&visible);

        Rc::new(move |query: &str| {
            while let Some(child) = list.first_child() {
                list.remove(&child);
            }
            visible.borrow_mut().clear();

            match engine.clips(query, PAGE_SIZE) {
                Ok(clips) if clips.is_empty() => {
                    status.set_text("No clips match.");
                }
                Ok(clips) => {
                    for clip in &clips {
                        list.append(&clip_row(clip));
                        visible.borrow_mut().push(clip.id);
                    }
                    if let Some(row) = list.row_at_index(0) {
                        list.select_row(Some(&row));
                    }
                    let pinned = clips.iter().filter(|c| c.pinned).count();
                    status.set_text(&format!("{} clips · {pinned} pinned", clips.len()));
                }
                Err(e) => {
                    status.set_text(&format!("{e}"));
                }
            }
        })
    };

    // Search as you type.
    {
        let refresh = Rc::clone(&refresh);
        search.connect_search_changed(move |entry| refresh(entry.text().as_str()));
    }

    // Enter, or a double click, copies and closes.
    {
        let engine = Rc::clone(&engine);
        let visible = Rc::clone(&visible);
        let status = status.clone();
        let window = window.clone();
        list.connect_row_activated(move |_, row| {
            let index = row.index();
            let Some(&id) = visible.borrow().get(index as usize) else { return };
            match engine.copy(id) {
                Ok(()) => window.close(),
                Err(e) => status.set_text(&format!("{e}")),
            }
        });
    }

    // Keyboard shortcuts: Escape closes, Ctrl+P pins, Delete removes.
    {
        let engine = Rc::clone(&engine);
        let visible = Rc::clone(&visible);
        let refresh = Rc::clone(&refresh);
        let list = list.clone();
        let status = status.clone();
        let window_for_keys = window.clone();
        let search_for_keys = search.clone();

        let keys = gtk::EventControllerKey::new();
        keys.connect_key_pressed(move |_, key, _, modifiers| {
            let selected_id = || -> Option<i64> {
                let row = list.selected_row()?;
                visible.borrow().get(row.index() as usize).copied()
            };

            match key {
                gdk::Key::Escape => {
                    window_for_keys.close();
                    glib::Propagation::Stop
                }
                gdk::Key::p if modifiers.contains(gdk::ModifierType::CONTROL_MASK) => {
                    if let Some(id) = selected_id() {
                        // The daemon holds the current pin state; flipping it
                        // here would need a read first, so toggle optimistically
                        // and let the refresh show the truth.
                        if let Err(e) = engine.set_pinned(id, true) {
                            status.set_text(&format!("{e}"));
                        }
                        refresh(search_for_keys.text().as_str());
                    }
                    glib::Propagation::Stop
                }
                // Delete only, deliberately not BackSpace. The search entry has
                // focus most of the time, and while bubble-phase propagation
                // means the entry consumes BackSpace for editing, binding a
                // destructive action to the key people press to fix a typo is
                // asking for an accident.
                gdk::Key::Delete => {
                    if let Some(id) = selected_id() {
                        if let Err(e) = engine.delete(id) {
                            status.set_text(&format!("{e}"));
                        }
                        refresh(search_for_keys.text().as_str());
                    }
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        });
        window.add_controller(keys);
    }

    // Warn up front if the service is not running, rather than showing an empty
    // list that looks like "you have never copied anything".
    if !engine.is_running() {
        status.set_text(&format!(
            "The Pasteport service is not running. Start it with: pasteportd  ({})",
            engine.socket_path().display()
        ));
    } else {
        refresh("");
        // Put the version in the title bar, where it is visible without being
        // in the way of the list.
        match engine.status() {
            Ok(report) => {
                window.set_title(Some(&format!(
                    "Pasteport {} — {} clips",
                    report.version, report.stats.total_clips
                )));
            }
            Err(e) => tracing::warn!(error = %e, "could not read service status"),
        }
    }

    window.present();
    search.grab_focus();
}

/// One row: pin marker, kind, preview, and where it came from.
fn clip_row(clip: &pasteport_core::Clip) -> gtk::ListBoxRow {
    let row = gtk::ListBoxRow::new();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 10);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);
    hbox.set_margin_start(10);
    hbox.set_margin_end(10);

    let marker = gtk::Label::new(Some(if clip.pinned { "★" } else { " " }));
    marker.set_width_chars(2);
    hbox.append(&marker);

    let kind = gtk::Label::new(Some(clip.kind.as_str()));
    kind.set_width_chars(10);
    kind.set_xalign(0.0);
    kind.add_css_class("dim-label");
    hbox.append(&kind);

    let preview = gtk::Label::new(Some(&clip.preview(90)));
    preview.set_xalign(0.0);
    preview.set_hexpand(true);
    preview.set_ellipsize(gtk::pango::EllipsizeMode::End);
    hbox.append(&preview);

    if let Some(app) = &clip.source_app {
        let source = gtk::Label::new(Some(app));
        source.add_css_class("dim-label");
        hbox.append(&source);
    }

    row.set_child(Some(&hbox));
    row
}

/// Last-resort window for a failure that happens before the UI exists.
fn show_fatal(app: &Application, message: &str) {
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Pasteport")
        .default_width(480)
        .build();
    let label = gtk::Label::new(Some(message));
    label.set_wrap(true);
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_margin_start(24);
    label.set_margin_end(24);
    window.set_child(Some(&label));
    window.present();
}
