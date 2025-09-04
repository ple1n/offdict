#![allow(warnings)]
#![cfg_attr(
    all(not(debug_assertions), target_os = "windows"),
    windows_subsystem = "windows"
)]

use gdkx11::gdk::{ffi::GDK_CURRENT_TIME, ModifierType};
use gtk::{
    gdk,
    traits::{GtkWindowExt, WidgetExt},
    ApplicationWindow,
};
use gtk::{prelude::*, HeaderBar};
// use lazy_regex;
use lazy_regex::{lazy_regex, Lazy};
use offdictd::{self, config::OffdictConfig, def_bin::WrapperDef, topk::Strprox, *};
use std::io;
use std::{
    borrow::Cow, collections::BTreeSet, env, fs, iter::FromIterator, path::PathBuf, sync::Arc,
    sync::RwLock, thread, time::Instant,
};
use tauri::{
    self, api::dialog, regex::Regex, utils::debug_eprintln, ClipboardManager,
    GlobalShortcutManager, Manager, PhysicalPosition, Window, WindowEvent,
};
use tauri_plugin_positioner::{Position, WindowExt};
use timed::timed;

struct Handler<T: ClipboardManager> {
    app: Window,
    clip: T,
    last: String,
}

use gtk::glib;

static mut pendingText: String = String::new();
static mut pos: Option<PhysicalPosition<i32>> = None;
pub static re1: Lazy<Regex> = lazy_regex!(r"[;\[\]{}<>#@$%^&*/\\:]");
pub static re2: Lazy<Regex> = lazy_regex!(r"[;\[\]{}<>#@$%^&*/\\:,.?!。，]");

impl<T: ClipboardManager> ClipboardHandler for Handler<T> {
    // TODO: Don't use X11 if on Wayland.
    // Selection watching only works on XWayland apps on wayland
    fn on_clipboard_change(&mut self, mut x1: Option<String>, explicit: bool) -> CallbackResult {
        let k: String;
        println!("is_explicit, {}", explicit);
        if x1.is_none() {
            if explicit {
                k = self.clip.read_text().unwrap().unwrap_or_default();
            } else {
                println!("clip is none");
                return CallbackResult::Next;
            }
        } else {
            k = x1.unwrap();
        }
        unsafe {
            if !explicit && EntryFocus {
                return CallbackResult::Next;
            }
        }
        if k == self.last {
            return CallbackResult::Next;
        }
        self.last = k.clone();
        if denied_clip(&k) {
            return CallbackResult::Next;
        }

        let r: Cow<str> = Cow::Owned(cleanup_clipboard_input(k.clone()));

        println!("clip cleaned: {}", r.as_ref());

        if r.is_empty() {
            return CallbackResult::Next;
        }
        unsafe {
            pendingText = r.into_owned();
        }

        if self.app.is_visible().unwrap() {
            glib::idle_add(|| unsafe {
                ENTRY.as_ref().unwrap().set_text(&pendingText);
                glib::source::Continue(false)
            });
        }

        CallbackResult::Next
    }

    fn on_clipboard_error(&mut self, error: io::Error) -> CallbackResult {
        println!("clip, error: {}", error);
        CallbackResult::Next
    }
}

fn cleanup_clipboard_input(s: String) -> String {
    let trimmed = s.trim().to_owned();
    let removed = re2.replace_all(&trimmed, "");
    removed.into_owned()
}

fn denied_clip(k: &str) -> bool {
    k.len() > 25 || re1.is_match(&k)
}

#[test]
fn test_clip() {
    assert!(denied_clip("io::Error"));
    assert!(!denied_clip("self.app.show"));
    assert!(!denied_clip("Concretely,"));
    assert!(!denied_clip("呃"));
    assert!(!denied_clip("呃🙃"));
    assert_eq!(
        cleanup_clipboard_input("   c,   ".to_owned()),
        "c".to_owned()
    );
}

pub struct OffdictState {
    conf: OffdictConfig,
}

// Learn more about Tauri commands at https://tauri.app/v1/guides/features/command

#[timed]
#[tauri::command]
fn input<'a>(
    state: tauri::State<'a, OffdictState>,
    query: String,
    expensive: bool,
) -> Result<(), &'static str> {
    on_input(query, expensive);
    Ok(())
}

#[tauri::command]
fn import<'a>(state: tauri::State<'a, OffdictState>) {
    println!("import");

    dialog::FileDialogBuilder::default()
        .add_filter("tar.gz/yaml", &["tar.gz", "yaml"])
        .pick_folder(move |folder| match folder {
            Some(folder) => {
                thread::spawn(move || {
                    let db = unsafe { DB.as_ref().unwrap() };

                    println!("folder picked, {}", folder.display());
                    let paths = fs::read_dir(folder).unwrap();
                    for path in paths {
                        let p: String = path.as_ref().unwrap().path().to_str().unwrap().to_string();
                        if p.ends_with(".yaml") {
                            println!("importing yaml, {}", &p);
                            let pre = path.unwrap().file_name();
                            let name = pre.to_str().unwrap().split_once(".").unwrap();

                            unsafe {
                                w.as_ref().unwrap().emit("importing", &p).unwrap();
                            }

                            db.import_from_file(&p, &name.0.to_string()).unwrap();

                            unsafe {
                                w.as_ref().unwrap().emit("imported", &p).unwrap();
                            }
                        } else if p.ends_with(".yaml.gz") {
                            println!("yaml.gz , {}", p)
                        }
                    }
                });
            }
            None => (),
        });
}

static mut ENTRY: Option<gtk::Entry> = None;
static mut EntryFocus: bool = false;
static mut w: Option<Window> = None;

const CSS: &[u8] = b"
headerbar entry,
headerbar spinbutton,
headerbar button,
headerbar separator {
    margin-top: 0px; /* same as headerbar side padding for nicer proportions */
    margin-bottom: 0px;
}


headerbar {
    min-height: 0;
    padding-left: 2px; /* same as childrens vertical margins for nicer proportions */
    padding-right: 2px;
    margin: 0px; /* same as headerbar side padding for nicer proportions */
    padding: 0px;
}

.inputheader entry {
    margin: 10px
}

.inputheader button {
	margin: 10 10px 10px 0px;
}
.inputheader {
	padding-left: 25px;
}
               ";

fn save_pos(win: &Window) {
    unsafe {
        pos = Some(win.outer_position().unwrap());
    }
}

#[inline]
fn window_visible(win: &Window, vis: bool) {
    if vis {
        win.set_always_on_top(true).unwrap();
        restore_pos(win);
        win.show().unwrap();
    } else {
        save_pos(win);
        win.hide().unwrap();
    }
}

fn restore_pos(win: &Window) {
    // unsafe {
    //     win.set_position(pos.unwrap()).unwrap();
    // }
    if !win.is_visible().unwrap() {
        win.move_window(Position::BottomRight)
            .expect("cannot move window");
    }
}

fn plain_header() -> HeaderBar {
    let header = gtk::HeaderBar::builder()
        .opacity(1.0)
        .title("Offdict")
        .visible(true)
        .build();

    header
}

fn input_header(win: Window) -> HeaderBar {
    let header = gtk::HeaderBar::builder()
        .opacity(1.0)
        .visible(true)
        .hexpand(true)
        .build();
    let bo = gtk::Box::builder().visible(true).hexpand(true).build();
    let btn = gtk::Button::builder().visible(true).label("import").build();
    let en = unsafe {
        ENTRY = Some(gtk::Entry::builder().visible(true).build());
        ENTRY.as_ref().unwrap()
    };
    bo.pack_start(en, true, true, 0);
    en.connect_focus_in_event(|_, _| unsafe {
        EntryFocus = true;
        Inhibit(false)
    });
    en.connect_focus_out_event(|_, _| unsafe {
        EntryFocus = false;
        Inhibit(false)
    });
    en.connect_changed(|e| {
        dbg!(e.text());
        let _ = on_input(e.text().to_string(), false);
    });
    en.connect_key_press_event(move |e, k| {
        if k.keyval() == gdk::keys::constants::Return {
            println!("expensive search");
            let _ = on_input(e.text().to_string(), true);
        } else if k.keyval() == gdk::keys::constants::Escape {
            // save_pos(&win);
            // win.minimize().unwrap();
        }
        Inhibit::default()
    });
    bo.pack_end(&btn, false, false, 0);
    btn.connect_clicked(|b| {
        println!("import btn");
    });
    header.set_custom_title(Some(&bo));
    header.style_context().add_class("inputheader");

    header
}

#[derive(Serialize, Clone)]
struct set_input {
    inputWord: String,
    extensive: bool,
}

// has results ?
fn on_input(s: String, expensive: bool) -> Result<()> {
    tauri::async_runtime::spawn_blocking(move || {
        if let Some(ref mut db) = unsafe { &mut DB } {
            let mut d = db.search(&s, 4, expensive)?;
            let mut def_list = offdictd::flatten_human(d);

            unsafe {
                let si = set_input {
                    inputWord: s.to_owned(),
                    extensive: expensive,
                };
                w.as_ref().unwrap().emit("set_input", si).unwrap();
            }
            unsafe {
                w.as_ref().unwrap().emit("def_list", &def_list).unwrap();
            }
        } else {
            println!("DB not inited yet");
        }

        anyhow::Ok(())
    });

    Ok(())
}

fn main() -> Result<()> {
    let conf = offdictd::config::get_config();

    let db_path = PathBuf::from(conf.data_path.clone());
    let dont_hide = true; // minimize instead of hiding
                          // FIXME: Temp fix for wayland where shortcut doesnt bring the window back.
    println!("{:?}", conf);

    if !process_cmd(|| init_db(db_path.clone()))? {
        return Ok(());
    }

    let ta = tauri::Builder::default()
        .setup(move |app| {
            println!("{}", env::current_dir().unwrap().to_str().unwrap());
            let window = app.get_window("main").unwrap();
            let w_on_ev = app.get_window("main").unwrap();
            let w_on_shortcut = app.get_window("main").unwrap();
            let w_on_esc = app.get_window("main").unwrap();

            unsafe {
                w = Some(app.get_window("main").unwrap());
            }

            let win = window.gtk_window().unwrap();
            let header = input_header(w_on_esc);
            let provider = gtk::CssProvider::new();
            provider.load_from_data(&CSS).unwrap();

            gtk::StyleContext::add_provider_for_screen(
                &gdk::Screen::default().expect("Error initializing gtk css provider."),
                &provider,
                gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
            );
            // win.add(&header);
            win.connect_key_press_event(|wi, ek| unsafe {
                use gdk::keys::constants::*;
                let excl: BTreeSet<gdk::keys::Key> = BTreeSet::from_iter(
                    vec![
                        Return, Left, Right, Escape, Alt_L, Alt_R, Shift_L, Shift_R, Caps_Lock,
                        Tab, Up, Down, Super_L, Super_R, Home, End, Page_Down, Page_Up, Control_L,
                        Control_R,
                    ]
                    .into_iter(),
                );
                println!("{:?}", ek.keyval());
                if !excl.contains(&ek.keyval())
                    && !ENTRY.as_ref().unwrap().is_focus()
                    && !ek
                        .state()
                        .intersects(ModifierType::CONTROL_MASK | ModifierType::SUPER_MASK)
                // No modifier key present
                {
                    ENTRY.as_ref().unwrap().set_is_focus(true);
                }
                Inhibit(false)
            });
            win.set_titlebar(Some(&header));

            win.set_border_width(0);
            // window.set_decorations(true).unwrap();
            // window.set_always_on_top(true).unwrap();
            // window.set_focus().unwrap();
            window
                .move_window(Position::BottomRight)
                .expect("cannot move window");

            if conf.hide_on_blur {
                window.on_window_event(move |e| match e {
                    WindowEvent::Focused(b) => {
                        if !b {
                            save_pos(&w_on_ev);
                            w_on_ev.hide().unwrap();
                        }
                    }
                    _ => (),
                });
            }

            match app
                .global_shortcut_manager()
                .register("ctrl+alt+c", move || {
                    if w_on_shortcut.is_visible().unwrap() {
                        save_pos(&w_on_shortcut);
                        if dont_hide {
                            w_on_shortcut.minimize().unwrap();
                        } else {
                            w_on_shortcut.hide().unwrap()
                        }
                    } else {
                        unsafe {
                            // use case: select when window hidden then ctrl+alt+c
                            ENTRY.as_ref().unwrap().set_text(&pendingText);
                        }
                        w_on_shortcut.set_always_on_top(true).unwrap();
                        restore_pos(&w_on_shortcut);
                        if dont_hide {
                            w_on_shortcut.unminimize().unwrap();
                        } else {
                            w_on_shortcut.show().unwrap()
                        }
                    }
                    // w_on_shortcut.set_focus().unwrap();
                    // w_on_shortcut.set_always_on_top(true).unwrap();
                }) {
                Ok(x) => {}
                Err(x) => {
                    println!("cannot reg global shortcut {:?}", x)
                }
            }
            // let state: tauri::State<OffdictState> = app.state();

            let mut m = Master::new(Handler {
                app: app.get_window("main").unwrap(),
                clip: app.clipboard_manager(),
                last: "".to_owned(),
            });

            thread::spawn(move || {
                println!("clipboard ..");
                m.run().unwrap()
            });

            Ok(())
        })
        .on_page_load(|a, b| {
            static mut RUN_ONCE: bool = false;
            unsafe { RUN_ONCE = if RUN_ONCE { return } else { true } }

            tauri::async_runtime::spawn_blocking(move || {
                let db = init_db(a.state::<OffdictState>().conf.data_path.clone()).unwrap();
                db.set_input = Some(on_input);
                tauri::async_runtime::spawn(serve(db));
            });
        })
        .manage(OffdictState { conf })
        .invoke_handler(tauri::generate_handler![input, import]);
    ta.run(tauri::generate_context!())
        .expect("error while running tauri application");
    Ok(())
}
