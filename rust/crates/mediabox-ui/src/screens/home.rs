//! The home screen of the appliance, which is not a media screen.
//!
//! MediaBox is an environment for this board rather than a player with a menu,
//! so the first screen is what the box can be used for and how the box itself
//! is doing: a grid of applications, and a column of the machine's own vital
//! signs. The film and series catalogue is one of those applications and lives
//! behind its own tile — it used to be printed straight onto this screen,
//! which made an operating system's home look like a video shop's front page.

use crate::api;
use crate::app::{Nav, Recent, Route, recents};
use crate::components::{Rail, human_size};
use crate::model::{AppEntry, DisplayStatus, MetaPreview};
use leptos::prelude::*;
use leptos::task::spawn_local;
use serde_json::Value;

/// A tile on the home screen.
///
/// Two kinds, and the difference is what "opening" means. A screen of this
/// same interface is a route change and nothing else moves. An application of
/// the box is a unit that takes the display, which ends this page — only one
/// process may hold DRM master.
#[derive(Clone)]
enum Tile {
    Screen {
        id: &'static str,
        name: &'static str,
        route: Route,
    },
    Unit(AppEntry),
}

/// The screens of this interface that are applications in their own right.
fn screens() -> Vec<Tile> {
    vec![
        Tile::Screen {
            id: "media",
            name: "Filmler ve Diziler",
            route: Route::Media,
        },
        Tile::Screen {
            id: "browser",
            name: "Tarayıcı",
            route: Route::Browser,
        },
        // Settings is an application on both of the interfaces this screen
        // follows, and it is one here too: the top bar is for moving between
        // screens, the grid is what the box can do.
        Tile::Screen {
            id: "settings",
            name: "Ayarlar",
            route: Route::Settings,
        },
    ]
}

#[component]
pub fn Home() -> impl IntoView {
    let nav = expect_context::<Nav>();
    let applications = RwSignal::new(Vec::<AppEntry>::new());
    let owner = RwSignal::new(None::<String>);
    let failed = RwSignal::new(false);
    let asked = RwSignal::new(false);
    spawn_local(async move {
        match api::typed::<DisplayStatus>(api::applications()).await {
            Ok(display) => {
                owner.set(display.owner);
                applications.set(display.applications);
            }
            Err(_) => failed.set(true),
        }
        asked.set(true);
    });

    let recent = RwSignal::new(recents());
    let open = Callback::new(move |item: MetaPreview| {
        nav.seed(item.clone());
        nav.go(Route::Detail {
            kind: if item.is_library() {
                "library".to_string()
            } else {
                item.kind.clone()
            },
            id: item.id.clone(),
        });
    });

    view! {
        <div class="home">
            <div class="wallpaper"></div>
            <Vitals />
            <main class="home-main">
                <Launcher applications=applications owner=owner asked=asked failed=failed />
                {move || {
                    let entries: Vec<MetaPreview> = recent
                        .get()
                        .into_iter()
                        .map(Recent::into)
                        .collect();
                    view! { <Rail title="Devam Et" items=entries on_pick=open /> }
                }}
            </main>
        </div>
    }
}

// -------------------------------------------------------------- the machine

/// The board's own vital signs, down the side of its home screen.
///
/// This is a single-board computer and the interface is the only thing anybody
/// will ever see of it, so how the machine itself is doing belongs on its front
/// page rather than three screens deep in a settings panel. Nothing here is
/// focusable: it is read, not operated, and a remote should walk applications
/// rather than gauges.
#[component]
fn Vitals() -> impl IntoView {
    let clock = RwSignal::new(now());
    let vitals = RwSignal::new(None::<Value>);
    let read = move || {
        spawn_local(async move {
            if let Ok(answer) = api::control(api::diagnostics()).await {
                vitals.set(Some(answer));
            }
        });
    };
    read();
    // Ten seconds. Load average and temperature do not move faster than that
    // in any way a person can see, and a home screen that re-renders four
    // times a second is a home screen that is never idle.
    set_interval(
        move || {
            clock.set(now());
            read();
        },
        std::time::Duration::from_secs(10),
    );

    let cpu = move || {
        vitals.with(|found| {
            let root = found.as_ref()?;
            // What the processors are actually doing. The load average that
            // used to be shown here is a queue length, not a share of time:
            // eight cores nearly idle with two tasks waiting read as a quarter
            // busy, which is what the television was reporting.
            if let Some(usage) = root.pointer("/cpu/usage").and_then(Value::as_f64) {
                return Some(usage.clamp(0.0, 1.0));
            }
            let count = root
                .pointer("/cpu/count")
                .and_then(Value::as_f64)
                .unwrap_or(1.0)
                .max(1.0);
            let load = root.pointer("/cpu/load/one").and_then(Value::as_f64)?;
            Some((load / count).clamp(0.0, 1.0))
        })
    };
    let memory = move || {
        vitals.with(|found| {
            let root = found.as_ref()?;
            let used = root.pointer("/memory/usedBytes").and_then(Value::as_f64)?;
            let total = root.pointer("/memory/totalBytes").and_then(Value::as_f64)?;
            (total > 0.0).then(|| (used / total, used, total))
        })
    };
    let hottest = move || {
        vitals.with(|found| {
            found
                .as_ref()?
                .get("temperatures")?
                .as_array()?
                .iter()
                .filter_map(|entry| entry.get("celsius").and_then(Value::as_f64))
                .fold(None::<f64>, |hot, value| {
                    Some(hot.map_or(value, |hot: f64| hot.max(value)))
                })
        })
    };
    let disk = move || {
        vitals.with(|found| {
            let volume = found.as_ref()?.get("storage")?.as_array()?.first()?;
            let used = volume.get("usedBytes").and_then(Value::as_f64)?;
            let total = volume.get("totalBytes").and_then(Value::as_f64)?;
            (total > 0.0).then(|| (used / total, used, total))
        })
    };
    let network = move || {
        vitals.with(|found| {
            found
                .as_ref()?
                .pointer("/network/default/interface")?
                .as_str()
                .map(str::to_string)
        })
    };

    view! {
        <aside class="vitals" aria-hidden="true">
            <div class="widget widget-clock">
                <span class="widget-time">{move || clock.get().0}</span>
                <span class="widget-date">{move || clock.get().1}</span>
            </div>

            <div class="widget">
                <span class="widget-title">"Performans"</span>
                <div class="dials">
                    <Dial
                        label="İşlemci"
                        fraction=Signal::derive(move || cpu().unwrap_or(0.0))
                        text=Signal::derive(move || {
                            cpu().map_or("—".to_string(), |value| format!("%{:.0}", value * 100.0))
                        })
                    />
                    <Dial
                        label="Bellek"
                        fraction=Signal::derive(move || {
                            memory().map_or(0.0, |(fraction, _, _)| fraction)
                        })
                        text=Signal::derive(move || {
                            memory()
                                .map_or("—".to_string(), |(fraction, _, _)| {
                                    format!("%{:.0}", fraction * 100.0)
                                })
                        })
                    />
                </div>
                <span class="widget-note">
                    {move || {
                        let heat = hottest()
                            .map_or("—".to_string(), |celsius| format!("{celsius:.0} °C"));
                        let used = memory()
                            .map_or("—".to_string(), |(_, used, total)| {
                                format!("{} / {}", human_size(used), human_size(total))
                            });
                        format!("{used}  ·  {heat}")
                    }}
                </span>
            </div>

            <div class="widget">
                <span class="widget-title">"Depolama"</span>
                <span class="widget-strong">
                    {move || {
                        disk()
                            .map_or("—".to_string(), |(_, used, total)| {
                                format!("{} / {}", human_size(used), human_size(total))
                            })
                    }}
                </span>
                <span class="bar">
                    <i style=move || {
                        format!("width:{:.1}%", disk().map_or(0.0, |(f, _, _)| f) * 100.0)
                    }></i>
                </span>
            </div>

            <div class="widget">
                <span class="widget-title">"Ağ"</span>
                <span class="widget-strong">{move || network().unwrap_or_else(|| "—".into())}</span>
                <span class="widget-note">"Orange Pi 5 Ultra · RK3588"</span>
            </div>
        </aside>
    }
}

/// One ring, drawn with a conic gradient rather than an SVG arc: no element per
/// segment, and the only thing that changes when the reading does is one angle.
#[component]
fn Dial(
    #[prop(into)] label: String,
    #[prop(into)] fraction: Signal<f64>,
    #[prop(into)] text: Signal<String>,
) -> impl IntoView {
    view! {
        <div class="dial">
            <div
                class="dial-ring"
                style=move || format!("--sweep:{:.1}deg", fraction.get().clamp(0.0, 1.0) * 360.0)
            >
                <span class="dial-value">{move || text.get()}</span>
            </div>
            <span class="dial-label">{label}</span>
        </div>
    }
}

/// The clock, as a television writes it: the time large, the day under it.
fn now() -> (String, String) {
    const DAYS: [&str; 7] = [
        "Pazar",
        "Pazartesi",
        "Salı",
        "Çarşamba",
        "Perşembe",
        "Cuma",
        "Cumartesi",
    ];
    const MONTHS: [&str; 12] = [
        "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
        "Kasım", "Aralık",
    ];
    let date = js_sys::Date::new_0();
    let day = DAYS[(date.get_day() as usize).min(6)];
    let month = MONTHS[(date.get_month() as usize).min(11)];
    (
        format!("{:02}:{:02}", date.get_hours(), date.get_minutes()),
        format!("{day}, {} {month}", date.get_date()),
    )
}

// ---------------------------------------------------------------- the grid

/// What this appliance can be used for.
///
/// Every tile is an application of equal standing: the catalogue, the browser,
/// the hardware player. This interface is simply the one that happens to be on
/// the screen, and choosing a unit ends it — only one process may hold the
/// display.
#[component]
fn Launcher(
    applications: RwSignal<Vec<AppEntry>>,
    owner: RwSignal<Option<String>>,
    asked: RwSignal<bool>,
    failed: RwSignal<bool>,
) -> impl IntoView {
    let nav = expect_context::<Nav>();
    let toaster = expect_context::<crate::app::Toaster>();
    let launch = move |id: String, name: String| {
        toaster.say(format!("{name} açılıyor…"));
        spawn_local(async move {
            if let Err(error) = api::control(api::application_launch(&id)).await {
                toaster.warn(format!("{name} açılamadı: {}", error.message));
            }
        });
    };

    view! {
        <section class="apps" data-row="1">
            <div class="app-grid">
                {move || {
                    let here = owner.get();
                    let mut tiles = screens();
                    tiles.extend(
                        applications
                            .get()
                            .into_iter()
                            // The interface itself and the browser are already
                            // on this screen as their own tiles; and the unit
                            // on the television is where you are, so a tile for
                            // it would reopen what you are looking at.
                            .filter(|entry| {
                                entry.id != "mediabox"
                                    && entry.id != "browser"
                                    && here.as_deref() != Some(entry.id.as_str())
                            })
                            .map(Tile::Unit),
                    );
                    tiles
                        .into_iter()
                        .enumerate()
                        .map(|(position, tile)| {
                            // The remote starts on the first application,
                            // because that is what this screen is for.
                            let first = position == 0;
                            match tile {
                                Tile::Screen { id, name, route } => {
                                    let mark = app_mark(id);
                                    let go = move |_| nav.go(route.clone());
                                    view! {
                                        <button
                                            class="app-tile"
                                            data-focus="1"
                                            data-autofocus=first.then_some("1")
                                            data-focus-key=format!("app:{id}")
                                            tabindex="-1"
                                            on:click=go
                                        >
                                            <span class="app-icon" style=mark.tint>
                                                <svg
                                                    class="app-mark"
                                                    viewBox="0 0 24 24"
                                                    aria-hidden="true"
                                                    focusable="false"
                                                >
                                                    <path d=mark.path />
                                                </svg>
                                            </span>
                                            <span class="app-name">{name}</span>
                                        </button>
                                    }
                                        .into_any()
                                }
                                Tile::Unit(entry) => {
                                    let mark = app_mark(&entry.id);
                                    let id = entry.id.clone();
                                    let name = entry.name.clone();
                                    let label = entry.name.clone();
                                    let ready = entry.installed;
                                    let go = launch;
                                    view! {
                                        <button
                                            class="app-tile"
                                            class:is-missing=!ready
                                            data-focus=ready.then_some("1")
                                            data-autofocus=first.then_some("1")
                                            data-focus-key=format!("app:{}", entry.id)
                                            tabindex="-1"
                                            disabled=!ready
                                            on:click=move |_| go(id.clone(), name.clone())
                                        >
                                            <span class="app-icon" style=mark.tint>
                                                <svg
                                                    class="app-mark"
                                                    viewBox="0 0 24 24"
                                                    aria-hidden="true"
                                                    focusable="false"
                                                >
                                                    <path d=mark.path />
                                                </svg>
                                            </span>
                                            <span class="app-name">{label}</span>
                                            {(!ready)
                                                .then(|| {
                                                    view! {
                                                        <span class="app-state">"kurulu değil"</span>
                                                    }
                                                })}
                                        </button>
                                    }
                                        .into_any()
                                }
                            }
                        })
                        .collect_view()
                }}
                // The box's own applications arrive from the control plane, so
                // for a moment there is a gap after the two that are always
                // here. It says what it is waiting for rather than standing
                // empty, and says so plainly if the answer never comes.
                {move || {
                    (!asked.get()).then(|| view! { <div class="app-waiting">"Aranıyor…"</div> })
                }}
                {move || {
                    failed
                        .get()
                        .then(|| {
                            view! {
                                <div class="app-waiting bad">"Uygulama listesi alınamadı"</div>
                            }
                        })
                }}
            </div>
        </section>
    }
}

/// How one application is drawn on its tile.
struct Mark {
    path: &'static str,
    tint: &'static str,
}

/// A mark per application, drawn rather than fetched.
///
/// An appliance has no icon theme to ask and no network to depend on, so each
/// application is a stroked path and one colour. An application nobody has
/// registered a mark for still gets a proper tile rather than an empty box.
fn app_mark(id: &str) -> Mark {
    match id {
        "media" => Mark {
            path: "M3.6 4.6h16.8v14.8H3.6V4.6ZM3.6 9.2h16.8M8.2 4.6v4.6M15.8 4.6v4.6M10.2 12.4v4.2l4-2.1-4-2.1Z",
            tint: "--tint:340",
        },
        "kodi" => Mark {
            path: "M12 3.4a8.6 8.6 0 1 0 0 17.2 8.6 8.6 0 0 0 0-17.2ZM10.3 8.5v7l5.9-3.5-5.9-3.5Z",
            tint: "--tint:198",
        },
        "browser" => Mark {
            path: "M12 3.2a8.8 8.8 0 1 0 0 17.6 8.8 8.8 0 0 0 0-17.6ZM3.2 12h17.6M12 3.2c2.3 2.4 3.5 5.4 3.5 8.8s-1.2 6.4-3.5 8.8c-2.3-2.4-3.5-5.4-3.5-8.8S9.7 5.6 12 3.2Z",
            tint: "--tint:32",
        },
        "settings" => Mark {
            path: "M12 9.4a2.6 2.6 0 1 0 0 5.2 2.6 2.6 0 0 0 0-5.2ZM12 3.4l1.4 2.2 2.6-.5.6 2.6 2.2 1.3L19.6 12l1.2 2.4-2.2 1.3-.6 2.6-2.6-.5-1.4 2.2-1.4-2.2-2.6.5-.6-2.6-2.2-1.3L8.4 12 7.2 9.6l2.2-1.3.6-2.6 2.6.5L12 3.4Z",
            tint: "--tint:150",
        },
        "stremio" => Mark {
            path: "M12 3.6 4.4 8v8l7.6 4.4L19.6 16V8L12 3.6ZM12 3.6v16.8M4.4 8l7.6 4.4L19.6 8",
            tint: "--tint:262",
        },
        "screenbridge" => Mark {
            path: "M3.4 5.6h17.2v10.2H3.4V5.6ZM8.6 19.6h6.8M3.4 5.6 12 11l8.6-5.4",
            tint: "--tint:268",
        },
        _ => Mark {
            path: "M4.4 4.4h6v6h-6v-6ZM13.6 4.4h6v6h-6v-6ZM4.4 13.6h6v6h-6v-6ZM13.6 13.6h6v6h-6v-6Z",
            tint: "--tint:212",
        },
    }
}
