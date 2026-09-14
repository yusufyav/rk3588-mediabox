//! The television's interface.
//!
//! It is a client of mediaboxd-rs and of nothing else. The control plane owns
//! which application holds the display, the media core owns what is playable
//! and how, and Kodi owns playback; this process draws, listens to the remote,
//! and asks.

mod detail;
mod images;
mod input;
mod metrics;
mod model;
mod rpc;
mod session;
mod state;

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use mediabox_core::{InputAction, InputEvent};
use slint::ComponentHandle;

slint::include_modules!();

/// Where the daemon publishes normalised input. The television asks for `tv=1`
/// because navigation actions are only sent to the client that says it is the
/// one in the living room — without it a phone on the network moves in lockstep
/// with whoever holds the remote.
const EVENTS_URL: &str = "http://127.0.0.1:8787/v1/events?tv=1";

const CACHE_DIR: &str = "/var/lib/mediabox-ui/tv-imgcache";
const STATE_FILE: &str = "/var/lib/mediabox-ui/tv-state.json";

/// The appliance is the only place this runs in earnest, and there the defaults
/// above are right. The overrides exist so the interface can be driven against
/// a forwarded socket from a developer's machine — which is how a shelf layout
/// gets looked at without a deploy.
fn socket_path() -> String {
    std::env::var("MEDIABOX_TV_SOCKET").unwrap_or_else(|_| rpc::DEFAULT_SOCKET.to_string())
}

fn events_url() -> String {
    std::env::var("MEDIABOX_TV_EVENTS").unwrap_or_else(|_| EVENTS_URL.to_string())
}

fn cache_dir() -> String {
    std::env::var("MEDIABOX_TV_CACHE").unwrap_or_else(|_| CACHE_DIR.to_string())
}

fn state_file() -> String {
    std::env::var("MEDIABOX_TV_STATE").unwrap_or_else(|_| STATE_FILE.to_string())
}

thread_local! {
    /// How the background threads reach the interface. They post a closure to
    /// the event loop and it finds everything here, because nothing the
    /// interface owns is safe to send across a thread.
    static APP: RefCell<Option<Rc<RefCell<App>>>> = const { RefCell::new(None) };
}

fn with_app(f: impl FnOnce(&mut App)) {
    let app = APP.with(|slot| slot.borrow().clone());
    if let Some(app) = app {
        f(&mut app.borrow_mut());
    }
}

struct App {
    window: slint::Weak<MediaBoxWindow>,
    home: state::Home,
    images: images::ImageManager,
    meter: metrics::Metrics,
    dispatcher: input::Dispatcher,
    store: session::Store,

    detail: Option<detail::Detail>,
    detail_backdrop: Option<String>,
    detail_fade: f32,
    /// Bumped every time a detail screen is opened, so an answer for a title
    /// the viewer has already left is dropped instead of overwriting the one
    /// they are looking at.
    epoch: u64,
    /// What to restore once the shelves land, read from disk at startup.
    resume: Option<session::Snapshot>,
}

impl App {
    fn on_detail(&self) -> bool {
        self.detail.is_some()
    }

    /// A press that survived the dispatcher.
    fn act(&mut self, action: InputAction) {
        self.meter.key_accepted();

        if self.on_detail() {
            self.act_on_detail(action);
        } else {
            self.act_on_home(action);
        }
    }

    fn act_on_home(&mut self, action: InputAction) {
        let moved = match action {
            InputAction::Up => self.home.step(0, -1),
            InputAction::Down => self.home.step(0, 1),
            InputAction::Left => self.home.step(-1, 0),
            InputAction::Right => self.home.step(1, 0),
            InputAction::Ok => {
                self.open_detail();
                return;
            }
            // Back on the home screen goes up to the bar rather than nowhere.
            // There is no screen behind this one; the television was turned on
            // here.
            InputAction::Back | InputAction::Home => {
                let moved = self.home.row != 0;
                self.home.row = 0;
                moved
            }
            _ => false,
        };

        if moved {
            self.paint();
        }
    }

    fn act_on_detail(&mut self, action: InputAction) {
        let Some(detail) = self.detail.as_mut() else { return };

        match action {
            InputAction::Up => {
                if detail.step(0, -1) {
                    self.paint_detail();
                }
            }
            InputAction::Down => {
                if detail.step(0, 1) {
                    self.paint_detail();
                }
            }
            InputAction::Left => {
                if detail.step(-1, 0) {
                    self.paint_detail();
                }
            }
            InputAction::Right => {
                if detail.step(1, 0) {
                    self.paint_detail();
                }
            }
            InputAction::Ok => self.choose(),
            InputAction::Back => self.close_detail(),
            InputAction::Home => self.close_detail(),
            _ => {}
        }
    }

    /// OK on the detail screen.
    ///
    /// On a source it selects it and puts the remote back on the play button:
    /// choosing where a film comes from and starting it are two decisions, and
    /// a television that starts playing because somebody was reading down a
    /// list is a television nobody trusts.
    fn choose(&mut self) {
        let Some(detail) = self.detail.as_mut() else { return };

        if detail.row > 0 {
            let index = detail.row - 1;
            if detail.sources.get(index).is_some() {
                detail.selected = Some(index);
                detail.row = 0;
                detail.action = detail::ACTION_KODI;
                self.analyse();
                self.paint_detail();
            }
            return;
        }

        match detail.action {
            detail::ACTION_KODI => self.play(true),
            detail::ACTION_HERE => self.play(false),
            detail::ACTION_TRAILER => self.trailer(),
            _ => self.close_detail(),
        }
    }

    fn open_detail(&mut self) {
        // The bar is not built yet; pressing OK there should do nothing rather
        // than something surprising.
        if self.home.row == 0 {
            return;
        }
        let Some(item) = self.home.focused() else { return };

        let detail = detail::Detail::seeded(item);
        let (kind, id) = (detail.kind.clone(), detail.id.clone());
        self.detail = Some(detail);
        self.detail_backdrop = None;
        self.epoch += 1;

        if let Some(window) = self.window.upgrade() {
            window.set_screen("detail".into());
        }
        self.paint_detail();
        spawn_detail_load(self.epoch, kind, id);
        self.remember();
    }

    fn close_detail(&mut self) {
        self.detail = None;
        self.epoch += 1;
        if let Some(window) = self.window.upgrade() {
            window.set_screen("home".into());
        }
        // The home screen never forgot where it was, so there is nothing to
        // restore: the shelf and the poster are still the ones that were left.
        self.paint();
        self.remember();
    }

    fn detail_loaded(
        &mut self,
        epoch: u64,
        answer: Result<DetailAnswer, String>,
    ) {
        if epoch != self.epoch {
            return;
        }
        let Some(detail) = self.detail.as_mut() else { return };

        match answer {
            Ok(DetailAnswer::Library(envelope)) => detail.take_library(*envelope),
            Ok(DetailAnswer::Catalogue { meta, listing, raw }) => {
                detail.take_meta(*meta);
                detail.take_streams(*listing, raw);
            }
            Err(why) => detail.fail(&why),
        }

        self.analyse();
        self.paint_detail();
    }

    /// Asks the media core what it would do with the chosen source.
    fn analyse(&mut self) {
        let Some(detail) = self.detail.as_ref() else { return };
        let Some(source) = detail.selected_source() else { return };
        if !source.parsed.playable {
            return;
        }
        spawn_plan(self.epoch, source.parsed.url.clone(), source.raw.clone());
    }

    fn planned(&mut self, epoch: u64, plan: Option<model::Plan>) {
        if epoch != self.epoch {
            return;
        }
        if let Some(detail) = self.detail.as_mut() {
            detail.plan = plan;
            self.paint_detail();
        }
    }

    /// Starts the film.
    ///
    /// On Kodi this is the end of this process's involvement: the control plane
    /// stops the interface's unit as part of handing the display over, so the
    /// position is written to disk before the call rather than after it.
    fn play(&mut self, on_kodi: bool) {
        let Some(detail) = self.detail.as_ref() else { return };
        let Some(source) = detail.selected_source() else { return };
        if !source.parsed.playable {
            return;
        }

        let url = source.parsed.url.clone();
        let raw = source.raw.clone();
        self.remember();
        self.store.flush();

        if let Some(window) = self.window.upgrade() {
            window.set_detail_note(
                if on_kodi { "Kodi'ye aktarılıyor…" } else { "Oynatılıyor…" }.into(),
            );
        }

        spawn_play(on_kodi, url, raw);
    }

    fn trailer(&mut self) {
        let Some(detail) = self.detail.as_ref() else { return };
        let Some(id) = detail.meta.trailer.clone().filter(|id| !id.is_empty()) else { return };
        let url = format!("https://www.youtube.com/tv#/watch?v={id}");
        spawn_open(url);
    }

    fn remember(&mut self) {
        let snapshot = match self.detail.as_ref() {
            Some(detail) => session::Snapshot {
                screen: "detail".into(),
                home_row: self.home.row,
                home_col: self.home.column(),
                detail_kind: detail.kind.clone(),
                detail_id: detail.id.clone(),
            },
            None => session::Snapshot {
                screen: "home".into(),
                home_row: self.home.row,
                home_col: self.home.column(),
                detail_kind: String::new(),
                detail_id: String::new(),
            },
        };
        self.store.put(snapshot);
    }

    /// Both answers, or whichever of them arrived.
    ///
    /// The library is the appliance's own and is quick; the catalogues are a
    /// fan-out across third-party hosts. They are asked for together and
    /// whichever lands first is drawn, because a home screen that waits for the
    /// slowest addon is a home screen nobody sees.
    fn loaded(&mut self, home: Option<model::HomeRows>, library: Option<model::LibraryListing>) {
        self.meter.data_arrived();

        let rows = home.unwrap_or(model::HomeRows { rows: Vec::new() });
        let shelves = state::shelves_from(&rows, library.as_ref());

        if shelves.is_empty() {
            self.fail("Hiçbir raf getirilemedi.");
            return;
        }

        self.home.set_shelves(shelves);

        if let Some(window) = self.window.upgrade() {
            window.set_rails(slint::ModelRc::from(self.home.rails.clone()));
            window.set_screen("home".into());
        }
        self.paint();
        self.restore();
    }

    /// Puts the remote back where it was before the display changed hands.
    ///
    /// Done once, and only if the position still exists: shelves are third
    /// party and a title that was there an hour ago may not be now. A restore
    /// that cannot find its place is not an error, it is a home screen.
    fn restore(&mut self) {
        let Some(snapshot) = self.resume.take() else { return };

        if snapshot.home_row > 0 && snapshot.home_row < self.home.rows() {
            self.home.row = snapshot.home_row;
            self.home.step(snapshot.home_col as isize, 0);
        }

        if snapshot.screen == "detail" && !snapshot.detail_id.is_empty() {
            // Re-opened from the shelf item if it is still there, so the seed
            // is a real one and the first frame carries artwork.
            let seeded = self
                .home
                .focused()
                .filter(|item| item.id == snapshot.detail_id)
                .map(detail::Detail::seeded);

            if let Some(detail) = seeded {
                let (kind, id) = (detail.kind.clone(), detail.id.clone());
                self.detail = Some(detail);
                self.detail_backdrop = None;
                self.epoch += 1;
                if let Some(window) = self.window.upgrade() {
                    window.set_screen("detail".into());
                }
                self.paint_detail();
                spawn_detail_load(self.epoch, kind, id);
                return;
            }
        }

        self.paint();
    }

    fn fail(&mut self, why: &str) {
        if let Some(window) = self.window.upgrade() {
            window.set_failed(true);
            window.set_status(why.into());
        }
    }

    /// Everything the focus decides: which tiles carry a picture, what the hero
    /// says, and where the shelves sit. The Slint side animates between the
    /// values this writes; it does not decide any of them.
    fn paint(&mut self) {
        let Some(window) = self.window.upgrade() else { return };

        self.home.sync_artwork(&mut self.images);

        if let Some((art, second_layer)) = self.home.backdrop(&mut self.images) {
            if second_layer {
                window.set_art_b(art);
            } else {
                window.set_art_a(art);
            }
        }
        window.set_fade(self.home.fade);

        let (title, facts, summary) = self.home.hero();
        window.set_hero_title(title.into());
        window.set_hero_meta(facts.into());
        window.set_hero_summary(summary.into());

        window.set_focus_row(self.home.row as i32);
        window.set_focus_col(self.home.column() as i32);

        self.remember();
    }

    fn paint_detail(&mut self) {
        let Some(window) = self.window.upgrade() else { return };
        let Some(detail) = self.detail.as_ref() else { return };

        window.set_detail_title(detail.meta.name.clone().into());
        window.set_detail_facts(detail.facts_line().into());
        window.set_detail_summary(detail.meta.description.clone().unwrap_or_default().into());
        window.set_detail_genres(strings(detail.meta.genres.iter().take(4).cloned()));
        window.set_detail_people(strings(detail.people().into_iter()));

        window.set_detail_actions(strings(detail::ACTIONS.iter().map(|a| a.to_string())));
        window.set_detail_marks(strings(detail::MARKS.iter().map(|m| m.0.to_string())));
        window.set_detail_cuts(strings(detail::MARKS.iter().map(|m| m.1.to_string())));
        window.set_detail_enabled(slint::ModelRc::new(slint::VecModel::from(
            detail.enabled().to_vec(),
        )));

        window.set_detail_sources(slint::ModelRc::new(slint::VecModel::from(
            detail.rows_for_display(),
        )));
        window.set_detail_note(detail.note.clone().into());
        window.set_detail_technical(slint::ModelRc::new(slint::VecModel::from(detail.technical())));

        match detail.plan.as_ref().map(|plan| plan.verdict()) {
            Some((text, tone)) => {
                window.set_detail_verdict(text.into());
                window.set_detail_verdict_tone(tone.into());
            }
            None => {
                window.set_detail_verdict("".into());
                window.set_detail_verdict_tone("".into());
            }
        }

        window.set_detail_row(detail.row as i32);
        window.set_detail_col(detail.action as i32);

        // The poster is almost always a cache hit: the shelf this screen was
        // opened from decoded it at the same width a moment ago.
        if let Some(url) = detail.meta.poster.clone() {
            let key = images::Key::new(&url, state::POSTER_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                window.set_detail_poster(art);
            }
        }

        if let Some(url) = detail.meta.background.clone().or_else(|| detail.meta.poster.clone()) {
            if self.detail_backdrop.as_deref() != Some(url.as_str()) {
                self.detail_backdrop = Some(url.clone());
                self.detail_fade = if self.detail_fade > 0.5 { 0.0 } else { 1.0 };
            }
            let key = images::Key::new(&url, state::BACKDROP_WIDTH);
            self.images.want(&key);
            if let Some(art) = self.images.get(&key) {
                if self.detail_fade > 0.5 {
                    window.set_detail_art_b(art);
                } else {
                    window.set_detail_art_a(art);
                }
            }
            window.set_detail_fade(self.detail_fade);
        }

        self.remember();
    }
}

fn strings(values: impl Iterator<Item = String>) -> slint::ModelRc<slint::SharedString> {
    let items: Vec<slint::SharedString> = values.map(Into::into).collect();
    slint::ModelRc::new(slint::VecModel::from(items))
}

/// What a detail load came back with. The appliance's own library answers with
/// the record and its sources together; a catalogue title takes two calls.
enum DetailAnswer {
    Library(Box<model::LibraryItemEnvelope>),
    Catalogue {
        meta: Box<model::Meta>,
        listing: Box<model::StreamListing>,
        raw: Vec<serde_json::Value>,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let started = Instant::now();
    let trace_input = !std::env::args().any(|a| a == "--quiet-input");

    // Errors from winit, glutin and Slint itself go through `log`. Off unless
    // RUST_LOG says otherwise, so the journal is not filled on an ordinary run.
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("warn")).init();

    // The window, and nothing else. When the television showed nothing there
    // was no way to tell a broken interface from a broken window system, and
    // guessing at that cost an afternoon. This answers it in one run.
    if std::env::var_os("MEDIABOX_TV_SELFTEST").is_some() {
        let window = MediaBoxWindow::new()?;
        window.set_status("selftest".into());
        window.window().set_rendering_notifier(|state, _| {
            eprintln!("mediabox-tv.selftest rendering-state {state:?}");
        })?;
        eprintln!("mediabox-tv.selftest showing");
        window.run()?;
        return Ok(());
    }

    let window = MediaBoxWindow::new()?;

    let app = Rc::new(RefCell::new(App {
        window: window.as_weak(),
        home: state::Home::new(),
        images: images::ImageManager::new(cache_dir(), || {
            let _ = slint::invoke_from_event_loop(|| {
                with_app(|app| {
                    if app.images.collect() {
                        app.paint();
                    }
                })
            });
        }),
        meter: metrics::Metrics::new(started),
        dispatcher: input::Dispatcher::new(trace_input),
        store: session::Store::new(state_file()),

        detail: None,
        detail_backdrop: None,
        detail_fade: 0.0,
        epoch: 0,
        resume: session::read(state_file()),
    }));
    APP.with(|slot| *slot.borrow_mut() = Some(app.clone()));

    // The last position, written the moment the display changed hands. Coming
    // back from a film should not mean starting again at the top of the home
    // screen.
    session::on_shutdown(|| {
        with_app(|app| {
            app.remember();
            app.store.flush();
        });
        // The store writes on its own thread; give it the moment it needs
        // rather than racing the exit below.
        std::thread::sleep(Duration::from_millis(200));
    });

    // Frames are counted from the renderer rather than from a timer, so the
    // number is what was actually presented and not what we hoped for. It also
    // means the idle CPU figure this reports is not inflated by the thing
    // reporting it.
    {
        let handle = window.as_weak();
        let reported = std::cell::Cell::new(false);
        window
            .window()
            .set_rendering_notifier(move |state, _| {
                if !matches!(state, slint::RenderingState::AfterRendering) {
                    return;
                }
                with_app(|app| app.meter.frame());
                if !reported.replace(true) {
                    if let Some(window) = handle.upgrade() {
                        report_surface(&window);
                    }
                }
            })
            .expect("this renderer cannot report when it has drawn");
    }

    // Keys from the compositor: USB and Bluetooth keyboards. The remote does
    // not come this way — its input device is switched off in the compositor's
    // config, because the daemon is the authority for it.
    window.on_key_pressed(move |text| {
        let Some(action) = input::action_for_key(text.as_str()) else { return };
        with_app(|app| {
            if let Some(action) = app.dispatcher.accept(action, input::Origin::Keyboard) {
                app.act(action);
            }
        });
    });

    // Everything the daemon normalises — the CEC remote, the phone remote, an
    // injected action — arrives on its event stream.
    spawn_bus_listener();
    spawn_loader();

    let reporter = slint::Timer::default();
    reporter.start(slint::TimerMode::Repeated, Duration::from_secs(5), || {
        with_app(|app| {
            let held = app.images.held_mb();
            if let Some(line) = app.meter.report_due() {
                eprintln!("{line} art_cpu_mb={held}");
            }
        });
    });

    window.set_status("Raflar getiriliyor…".into());
    window.run()?;
    Ok(())
}

/// The panel, the buffer and the scale, as this process sees them.
///
/// This is half of the evidence that the interface is not being scaled twice:
/// the other half is the display controller's own state, which is read on the
/// appliance. A logical size smaller than the physical one is expected and
/// correct — that is the whole point of the integer scale — but a *buffer*
/// smaller than the panel's mode would mean wlroots is stretching us, and the
/// picture would be soft and the direct flip refused.
fn report_surface(window: &MediaBoxWindow) {
    let w = window.window();
    let physical = w.size();
    let scale = w.scale_factor();
    eprintln!(
        "mediabox-tv.surface buffer={}x{} scale={:.3} logical={:.0}x{:.0} env_scale={}",
        physical.width,
        physical.height,
        scale,
        physical.width as f32 / scale,
        physical.height as f32 / scale,
        std::env::var("SLINT_SCALE_FACTOR").unwrap_or_else(|_| "unset".into()),
    );
}

/// One short-lived runtime per request. The control plane answers in
/// milliseconds for everything local and seconds for anything that fans out to
/// addons; keeping a runtime alive between those would be keeping threads alive
/// for an interface that is usually doing nothing at all.
fn detached(name: &str, work: impl std::future::Future<Output = ()> + Send + 'static) {
    let name = name.to_string();
    std::thread::Builder::new()
        .name(name)
        .spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build()
            else {
                return;
            };
            runtime.block_on(work);
        })
        .expect("a control-plane thread could not be started");
}

fn spawn_detail_load(epoch: u64, kind: String, id: String) {
    detached("mediabox-tv-detail", async move {
        let client = rpc::Client::new(socket_path());

        let answer = if kind == "library" || id.starts_with("library:") {
            client
                .library_item(&id)
                .await
                .map(|envelope| DetailAnswer::Library(Box::new(envelope)))
                .map_err(|e| e.to_string())
        } else {
            // Together rather than one after the other: the record comes from
            // one addon and the sources from every addon that has any, and the
            // screen is already drawn from the shelf's seed either way.
            let (meta, streams) = tokio::join!(client.meta(&kind, &id), client.streams(&kind, &id));

            match (meta, streams) {
                (Ok(meta), Ok(streams)) => {
                    // The parsed listing is what the rows are laid out from;
                    // the raw array is what goes back when one is chosen.
                    let raw = streams
                        .get("streams")
                        .and_then(|value| value.as_array())
                        .cloned()
                        .unwrap_or_default();
                    match serde_json::from_value::<model::StreamListing>(streams) {
                        Ok(listing) => Ok(DetailAnswer::Catalogue {
                            meta: Box::new(meta.meta),
                            listing: Box::new(listing),
                            raw,
                        }),
                        Err(e) => Err(format!("kaynak listesi çözülemedi: {e}")),
                    }
                }
                (Err(e), _) | (_, Err(e)) => Err(e.to_string()),
            }
        };

        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.detail_loaded(epoch, answer));
        });
    });
}

fn spawn_plan(epoch: u64, url: Option<String>, raw: serde_json::Value) {
    detached("mediabox-tv-plan", async move {
        let client = rpc::Client::new(socket_path());
        let plan = match url.as_deref() {
            Some(url) => client.plan_for_url(url).await,
            None => client.plan_for_stream(&raw).await,
        };
        let plan = match plan {
            Ok(plan) => Some(plan),
            Err(e) => {
                eprintln!("mediabox-tv.plan failed: {e}");
                None
            }
        };
        let _ = slint::invoke_from_event_loop(move || {
            with_app(|app| app.planned(epoch, plan));
        });
    });
}

fn spawn_play(on_kodi: bool, url: Option<String>, raw: serde_json::Value) {
    detached("mediabox-tv-play", async move {
        let client = rpc::Client::new(socket_path());
        let stream = url.is_none().then_some(&raw);

        let answer = if on_kodi {
            client.play_on_kodi(url.as_deref(), stream, 0).await
        } else {
            client.play_here(url.as_deref(), stream, 0).await
        };

        // On the Kodi path this process is being stopped while the call is in
        // flight, so an error here is as likely to be the handover as a
        // failure. It is logged and not turned into a message nobody will see.
        match answer {
            Ok(_) => eprintln!("mediabox-tv.play started on_kodi={on_kodi}"),
            Err(e) => {
                eprintln!("mediabox-tv.play failed: {e}");
                let message = e.to_string();
                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| {
                        if let Some(window) = app.window.upgrade() {
                            window.set_detail_note(format!("Oynatılamadı — {message}").into());
                        }
                    });
                });
            }
        }
    });
}

fn spawn_open(url: String) {
    detached("mediabox-tv-open", async move {
        let client = rpc::Client::new(socket_path());
        if let Err(e) = client.browser_open(&url).await {
            eprintln!("mediabox-tv.trailer failed: {e}");
        }
    });
}

/// Asks the control plane for the home surface.
///
/// The two calls go out together on purpose: the library is this appliance's
/// own and answers in milliseconds, while the catalogues are a fan-out over
/// third-party hosts. Waiting for the second before drawing the first is time
/// the viewer spends looking at a name and a spinner.
fn spawn_loader() {
    std::thread::Builder::new()
        .name("mediabox-tv-data".into())
        .spawn(|| {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build()
            else {
                return;
            };

            runtime.block_on(async {
                let client = rpc::Client::new(socket_path());

                let at = Instant::now();
                let (home, library) = tokio::join!(client.home(), client.library());
                eprintln!("mediabox-tv.load media_home+library_ms={}", at.elapsed().as_millis());

                let home = match home {
                    Ok(home) => Some(home),
                    Err(e) => {
                        eprintln!("mediabox-tv.load home failed: {e}");
                        None
                    }
                };
                let library = match library {
                    Ok(library) => Some(library),
                    Err(e) => {
                        eprintln!("mediabox-tv.load library failed: {e}");
                        None
                    }
                };

                let _ = slint::invoke_from_event_loop(move || {
                    with_app(|app| app.loaded(home, library));
                });
            });
        })
        .expect("the loader thread could not be started");
}

/// Reads the daemon's event stream for as long as the process lives,
/// reconnecting when it ends. The daemon is restarted independently of this
/// interface, so a dropped stream is ordinary rather than fatal.
fn spawn_bus_listener() {
    std::thread::Builder::new()
        .name("mediabox-tv-bus".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(runtime) => runtime,
                Err(e) => {
                    eprintln!("mediabox-tv.bus no runtime: {e}");
                    return;
                }
            };

            runtime.block_on(async move {
                loop {
                    if let Err(e) = read_events().await {
                        eprintln!("mediabox-tv.bus disconnected: {e}");
                    }
                    tokio::time::sleep(Duration::from_secs(2)).await;
                }
            });
        })
        .expect("the input thread could not be started");
}

async fn read_events() -> Result<(), Box<dyn std::error::Error>> {
    let mut response = reqwest::Client::new().get(events_url()).send().await?.error_for_status()?;

    // Frames are newline-delimited and small; assembling them here avoids
    // pulling a stream adapter crate in for four lines of work.
    let mut pending = String::new();
    while let Some(chunk) = response.chunk().await? {
        pending.push_str(&String::from_utf8_lossy(&chunk));

        while let Some(at) = pending.find('\n') {
            let line: String = pending.drain(..=at).collect();
            deliver(line.trim_end());
        }
    }

    Ok(())
}

fn deliver(line: &str) {
    // Keepalives arrive as comment frames and carry no payload.
    let Some(payload) = line.strip_prefix("data: ") else { return };
    let Ok(event) = serde_json::from_str::<InputEvent>(payload) else { return };

    // Only the press edge. Through CEC the press, the hold and the release each
    // arrive as their own event.
    if !event.pressed {
        return;
    }

    let action = event.action;
    let _ = slint::invoke_from_event_loop(move || {
        with_app(|app| {
            if let Some(action) = app.dispatcher.accept(action, input::Origin::Bus) {
                app.act(action);
            }
        });
    });
}
