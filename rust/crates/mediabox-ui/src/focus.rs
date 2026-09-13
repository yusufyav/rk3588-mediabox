//! Directional focus and motion for a remote control.
//!
//! A television has four arrows and an OK button, so "what does Right mean
//! here" has to be answered by the layout itself rather than by DOM order —
//! DOM order is wrong the moment a rail wraps or a grid reflows. Every
//! candidate is scored against the focused element's rectangle: how far it is
//! in the direction asked for, plus a heavy penalty for drifting off the axis.
//! That is what makes Down from a poster land on the poster below it rather
//! than on whatever happens to be next in the document.
//!
//! Nothing here scrolls. A rail is moved by writing a `translate3d` on its
//! track and a screen by writing one on its column, both of which the
//! compositor animates on its own thread: no scroll animation on the main
//! thread, no repaint of the artwork that is moving, and nothing for a
//! half-drawn frame to tear. It also keeps the geometry honest, because a
//! transform does not change `offsetLeft`, so a rail in flight is scored from
//! exactly the same numbers as a rail at rest.
//!
//! Three invariants hold everywhere:
//!
//! * focus is never lost — if the focused element disappears, the screen's
//!   preferred element, or the first focusable one, takes it;
//! * an overlay traps focus — while one is open it is the scope, so no arrow
//!   press can reach the page behind it;
//! * where the remote was is remembered by key, so coming back from a title
//!   returns to the poster it was opened from rather than to the top.

use std::cell::RefCell;
use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
    Left,
    Right,
}

/// Marks an element as reachable by the remote.
pub const FOCUS_ATTR: &str = "data-focus";
/// Marks the container that currently owns focus, i.e. an open overlay.
pub const SCOPE_ATTR: &str = "data-focus-scope";
/// A stable name for a focusable element, so it can be found again after the
/// screen it lives on has been left and rebuilt.
pub const KEY_ATTR: &str = "data-focus-key";
/// A horizontal strip that moves as a unit: the row of posters inside a rail.
pub const TRACK_ATTR: &str = "data-track";
/// A vertical column that moves as a unit: everything on one screen.
pub const SCROLLER_ATTR: &str = "data-scroller";
/// A band within a column that is brought to the top when something in it has
/// focus — one rail, the hero, one block of settings.
pub const ROW_ATTR: &str = "data-row";

const FOCUSED_CLASS: &str = "is-focused";
/// How far below the top of the screen the focused band is parked.
const TOP_INSET: f64 = 92.0;

thread_local! {
    /// The element the remote is on. Kept here so moving focus does not have
    /// to sweep the document for whatever is wearing the class: on a screen of
    /// a hundred posters that sweep is the single most expensive thing a key
    /// press did.
    static FOCUSED: RefCell<Option<HtmlElement>> = const { RefCell::new(None) };
}

fn document() -> Option<web_sys::Document> {
    web_sys::window()?.document()
}

/// The container arrows may move within: the innermost open overlay, else the
/// whole page.
fn scope() -> Option<Element> {
    let document = document()?;
    document
        .query_selector(&format!("[{SCOPE_ATTR}]"))
        .ok()
        .flatten()
        .or_else(|| document.body().map(Into::into))
}

/// An element is reachable if it occupies space. `offsetWidth` answers that
/// without allocating a rectangle, and it is read from the same layout the
/// scoring below needs anyway.
fn is_visible(element: &HtmlElement) -> bool {
    element.offset_width() > 1 && element.offset_height() > 1
}

fn active() -> Option<HtmlElement> {
    document()?.active_element()?.dyn_into::<HtmlElement>().ok()
}

/// The focusable the remote is on, which is not always the element holding the
/// browser's focus.
///
/// A control may put the caret somewhere inside itself — the search field hands
/// focus to its `<input>` so that typing goes in — and that inner node carries
/// no `data-focus` of its own. Reading only `activeElement` then concludes that
/// focus has been lost, refocuses the wrapper, which hands focus back to the
/// input, which looks like focus being lost again: a loop that ran at the
/// refresh rate and held a core of this board flat. The remote is on the
/// nearest focusable ancestor, and that is what every question here means.
fn active_focusable() -> Option<HtmlElement> {
    let active = active()?;
    if active.has_attribute(FOCUS_ATTR) {
        return Some(active);
    }
    nearest(&active, FOCUS_ATTR)
}

/// The element the remote is on, if it is still both present and focusable.
fn active_in_scope(scope: &Element) -> Option<HtmlElement> {
    let active = active_focusable()?;
    (scope.contains(Some(&active)) && is_visible(&active)).then_some(active)
}

fn nearest(element: &HtmlElement, attribute: &str) -> Option<HtmlElement> {
    element
        .closest(&format!("[{attribute}]"))
        .ok()
        .flatten()
        .and_then(|found| found.dyn_into::<HtmlElement>().ok())
}

// ------------------------------------------------------------------ motion

/// One element being carried towards where it should be.
struct Glide {
    element: HtmlElement,
    at: (f64, f64),
    to: (f64, f64),
}

thread_local! {
    static GLIDES: RefCell<Vec<Glide>> = const { RefCell::new(Vec::new()) };
    static DRIVER: RefCell<Option<wasm_bindgen::closure::Closure<dyn FnMut()>>> =
        const { RefCell::new(None) };
    static DRIVING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// How much of the remaining distance is covered each frame.
///
/// 0.26 settles a move of any size in about eight frames — fast enough that the
/// shelf is never behind the remote, slow enough to read as movement rather
/// than as a jump.
const FOLLOW: f64 = 0.26;
/// Below this, the remaining distance is not worth another frame.
const SETTLED: f64 = 0.4;

/// Send an element towards a position, and keep it moving there.
///
/// Not a CSS transition, and the difference is the whole feel of the product. A
/// transition is restarted from zero by every key press, so holding an arrow
/// down re-eases the same strip five times a second: it accelerates, is
/// interrupted, accelerates again. What you see is a shelf that shudders
/// instead of sliding. This keeps one velocity and simply changes where it is
/// heading, so a burst of presses is one continuous movement that ends where
/// the last press asked for.
fn glide(element: &HtmlElement, x: f64, y: f64) {
    GLIDES.with(|glides| {
        let mut glides = glides.borrow_mut();
        match glides.iter_mut().find(|glide| &glide.element == element) {
            Some(found) => found.to = (x, y),
            None => glides.push(Glide {
                element: element.clone(),
                at: (0.0, 0.0),
                to: (x, y),
            }),
        }
    });
    drive();
}

fn write_transform(element: &HtmlElement, x: f64, y: f64) {
    let style = element.style();
    if x.abs() < 0.5 && y.abs() < 0.5 {
        let _ = style.set_property("transform", "");
    } else {
        let _ = style.set_property("transform", &format!("translate3d({x:.1}px, {y:.1}px, 0)"));
    }
}

fn drive() {
    if DRIVING.with(|flag| flag.replace(true)) {
        return;
    }
    schedule();
}

fn schedule() {
    DRIVER.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.is_none() {
            *slot = Some(wasm_bindgen::closure::Closure::new(advance));
        }
        if let Some(driver) = slot.as_ref()
            && let Some(window) = web_sys::window()
        {
            let _ = window
                .request_animation_frame(wasm_bindgen::JsCast::unchecked_ref(driver.as_ref()));
        }
    });
}

fn advance() {
    let moving = GLIDES.with(|glides| {
        let mut glides = glides.borrow_mut();
        // An element whose screen has been replaced is dropped rather than
        // animated forever.
        glides.retain(|glide| glide.element.is_connected());
        let mut moving = false;
        for glide in glides.iter_mut() {
            let dx = glide.to.0 - glide.at.0;
            let dy = glide.to.1 - glide.at.1;
            if dx.abs() < SETTLED && dy.abs() < SETTLED {
                if glide.at != glide.to {
                    glide.at = glide.to;
                    write_transform(&glide.element, glide.at.0, glide.at.1);
                }
                continue;
            }
            glide.at.0 += dx * FOLLOW;
            glide.at.1 += dy * FOLLOW;
            write_transform(&glide.element, glide.at.0, glide.at.1);
            moving = true;
        }
        moving
    });
    if moving {
        schedule();
    } else {
        DRIVING.with(|flag| flag.set(false));
    }
}

/// Forget every element being carried, without moving anything.
fn clear_glides() {
    GLIDES.with(|glides| glides.borrow_mut().clear());
}

/// Bring the focused element into view by moving its rail and its column.
///
/// The rail parks the focused poster at the left gutter, which is what makes
/// walking right feel like walking along a shelf rather than like a page
/// turning. The column parks the focused band just under the top of the
/// screen. Both are clamped at the ends so the last poster does not drag empty
/// space onto the screen behind it.
/// True only on the appliance's own kiosk, which says so in its URL.
pub fn television() -> bool {
    thread_local! {
        static TV: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
    }
    TV.with(|flag| match flag.get() {
        Some(known) => known,
        None => {
            let known = document()
                .and_then(|document| document.body())
                .is_some_and(|body| body.class_list().contains("tv"));
            flag.set(Some(known));
            known
        }
    })
}

fn reveal(element: &HtmlElement) {
    if !television() {
        // An ordinary page, driven by a pointer or a touch: the browser's own
        // scrolling is what a person expects, and it is already there. Moving
        // it with transforms instead is what left the interface unscrollable on
        // a phone, where nothing ever takes focus in the first place.
        let options = web_sys::ScrollIntoViewOptions::new();
        options.set_behavior(web_sys::ScrollBehavior::Smooth);
        options.set_block(web_sys::ScrollLogicalPosition::Nearest);
        options.set_inline(web_sys::ScrollLogicalPosition::Nearest);
        element.scroll_into_view_with_scroll_into_view_options(&options);
        return;
    }
    if let Some(track) = nearest(element, TRACK_ATTR) {
        // The card's offsetParent is the track, so its own `offsetLeft` is
        // already its position inside the strip. The first card sits at the
        // gutter, which is the resting position, so subtracting the gutter
        // makes an untouched rail translate by exactly zero.
        let lead = track.client_left() as f64
            + track
                .first_element_child()
                .and_then(|child| child.dyn_into::<HtmlElement>().ok())
                .map_or(0.0, |child| child.offset_left() as f64);
        // How far the strip may travel is its own content against the window
        // it is seen through, which is its parent — the element that clips.
        let window = track
            .parent_element()
            .map_or(track.client_width(), |parent| parent.client_width());
        let overflow = (track.scroll_width() - window).max(0) as f64;
        let wanted = (element.offset_left() as f64 - lead).clamp(0.0, overflow);
        glide(&track, -wanted, 0.0);
        prefetch(&track, element);
    }

    if let Some(column) = nearest(element, SCROLLER_ATTR) {
        let row = nearest(element, ROW_ATTR).unwrap_or_else(|| element.clone());
        // The column is not a scroll container — it is as tall as its content —
        // so how far it may travel is its own height against the window it is
        // seen through, which is its parent. Asking the column for its own
        // scroll height would answer zero and nothing would ever move.
        let visible = column
            .parent_element()
            .map_or(0, |parent| parent.client_height()) as f64;
        let overflow = (column.offset_height() as f64 - visible).max(0.0);
        let wanted = (row.offset_top() as f64 - TOP_INSET).clamp(0.0, overflow);
        glide(&column, 0.0, -wanted);
    }
}

/// Put every rail and column back to its resting position.
///
/// Called when the screen changes. A transform belongs to the screen that
/// asked for it, and the next screen is shorter: leaving the column pushed up
/// by the distance the last rail needed means the new screen is rendered
/// entirely above the top of the picture, and the television shows nothing at
/// all. Nothing here reads layout, so it costs one style write per strip.
pub fn reset_motion() {
    clear_glides();
    invalidate();
    let Some(document) = document() else { return };
    for attribute in [SCROLLER_ATTR, TRACK_ATTR] {
        let Ok(nodes) = document.query_selector_all(&format!("[{attribute}]")) else {
            continue;
        };
        for index in 0..nodes.length() {
            if let Some(node) = nodes
                .item(index)
                .and_then(|n| n.dyn_into::<HtmlElement>().ok())
            {
                let _ = node.style().set_property("transform", "");
            }
        }
    }
}

/// Ask for the artwork just beyond the edge of the picture.
///
/// Every poster is lazy, which is what stops a hundred of them being fetched
/// at once. But a clipped strip has no "nearly visible" for the browser to act
/// on — a card two places to the right is outside the clip and so is never
/// near the viewport — and walking a rail therefore arrived at one blank tile
/// after another. A small window either side of the remote is promoted to an
/// ordinary fetch, so the next few are already there when they are reached and
/// the twentieth is still not asked for until somebody goes that far.
fn prefetch(track: &HtmlElement, focused: &HtmlElement) {
    const BEHIND: usize = 2;
    const AHEAD: usize = 8;
    let cards = track.children();
    let count = cards.length() as usize;
    let mut index = 0usize;
    for position in 0..count {
        if cards
            .item(position as u32)
            .is_some_and(|card| card.contains(Some(focused.unchecked_ref())))
        {
            index = position;
            break;
        }
    }
    let start = index.saturating_sub(BEHIND);
    let end = (index + AHEAD + 1).min(count);
    for position in start..end {
        if let Some(card) = cards.item(position as u32)
            && let Ok(Some(image)) = card.query_selector("img")
            && image.get_attribute("loading").as_deref() != Some("eager")
        {
            let _ = image.set_attribute("loading", "eager");
        }
    }
}

// ------------------------------------------------------------------- focus

/// Give one element focus, and make that unmistakable on a television.
pub fn focus(element: &HtmlElement) {
    FOCUSED.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(previous) = slot.as_ref()
            && previous != element
        {
            let _ = previous.class_list().remove_1(FOCUSED_CLASS);
        }
        let _ = element.class_list().add_1(FOCUSED_CLASS);
        *slot = Some(element.clone());
    });
    // A screen rebuilt underneath us can leave the class on a node this module
    // never saw. The sweep is over the elements wearing the class — at most one
    // in normal operation — rather than over everything focusable, which is the
    // scan this cache exists to avoid.
    if let Some(document) = document()
        && let Ok(stale) = document.query_selector_all(&format!(".{FOCUSED_CLASS}"))
    {
        let focused = element.unchecked_ref::<Element>();
        for index in 0..stale.length() {
            if let Some(node) = stale.item(index).and_then(|n| n.dyn_into::<Element>().ok())
                && &node != focused
            {
                let _ = node.class_list().remove_1(FOCUSED_CLASS);
            }
        }
    }
    // `preventScroll`, because the movement is this module's job. Without it
    // the browser scrolls the element into view itself — on the main thread,
    // against the transforms below, and repainting everything it passes.
    let options = web_sys::FocusOptions::new();
    // Only the television takes the browser's scrolling away from it.
    options.set_prevent_scroll(television());
    let _ = element.focus_with_options(&options);
    reveal(element);
}

/// The name of the element the remote is on, if it has one.
///
/// This is what a screen records before it is left, so that coming back can
/// put the remote where it was rather than at the top of the page.
pub fn current_key() -> Option<String> {
    let scope = scope()?;
    active_in_scope(&scope)
        .or_else(|| FOCUSED.with(|slot| slot.borrow().clone()))
        .and_then(|element| element.get_attribute(KEY_ATTR))
}

/// Put the remote back on a named element. False when it is not on screen.
pub fn focus_key(key: &str) -> bool {
    let Some(scope) = scope() else { return false };
    let selector = format!("[{FOCUS_ATTR}][{KEY_ATTR}=\"{}\"]", css_escape(key));
    let Some(element) = scope
        .query_selector(&selector)
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
        .filter(is_visible)
    else {
        return false;
    };
    focus(&element);
    true
}

/// Keys come from catalogue identifiers, which are third-party strings. Only
/// the characters that could end the attribute selector need handling.
fn css_escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Put focus somewhere sensible in the current scope.
///
/// `data-autofocus` marks the element a screen wants to start on; otherwise the
/// first focusable element wins. Called after every screen change and whenever
/// the focused element is found to have gone away.
pub fn focus_first() {
    let Some(scope) = scope() else { return };
    let find = |selector: String| {
        scope
            .query_selector(&selector)
            .ok()
            .flatten()
            .and_then(|element| element.dyn_into::<HtmlElement>().ok())
            .filter(is_visible)
    };
    // In order: what the screen asked for, then the first thing on the screen
    // itself, and only then anything at all. Without the middle step the first
    // focusable element in the document is the navigation down the left edge,
    // so every screen that did not name a starting point opened with the
    // remote parked on the chrome instead of on the content.
    let preferred = find(format!("[{FOCUS_ATTR}][data-autofocus]"))
        .or_else(|| find(format!("[{SCROLLER_ATTR}] [{FOCUS_ATTR}]")));
    if let Some(element) = preferred.or_else(|| candidates(&scope).into_iter().next()) {
        focus(&element);
    }
}

/// Move to the element the current screen wants focus on, if there is one and
/// it is not already focused.
///
/// A screen is mounted before its data arrives, so the element worth starting
/// on — the hero's play button, a dialog's confirm — usually does not exist at
/// the moment the screen appears. This is called again as content lands, and
/// stops mattering the instant somebody presses a key.
pub fn claim_autofocus() -> bool {
    let Some(scope) = scope() else { return false };
    let Some(target) = scope
        .query_selector(&format!("[{FOCUS_ATTR}][data-autofocus]"))
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
        .filter(is_visible)
    else {
        return false;
    };
    if active_focusable().is_some_and(|current| current == target) {
        return true;
    }
    focus(&target);
    true
}

/// Pull the remote off the navigation bar and onto the screen, once the screen
/// has something to stand on.
///
/// A screen is mounted before its data arrives, so for the first frames it has
/// no focusable element at all and the only one in the document is the
/// navigation. That is where the remote landed and, because the element the
/// screen wanted — a play button that is disabled until a playable source is
/// found — never became focusable, that is where it stayed: opening a title
/// left the highlight sitting on "Ana Sayfa" while the title's own buttons
/// were right there. Answers true once the remote is on the screen itself.
pub fn settle_into_screen() -> bool {
    let Some(scope) = scope() else { return false };
    let Some(column) = scope
        .query_selector(&format!("[{SCROLLER_ATTR}]"))
        .ok()
        .flatten()
    else {
        return false;
    };
    if active_focusable().is_some_and(|current| column.contains(Some(current.as_ref()))) {
        return true;
    }
    let Some(target) = column
        .query_selector(&format!("[{FOCUS_ATTR}]"))
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
        .filter(is_visible)
    else {
        return false;
    };
    focus(&target);
    true
}

/// Restore focus if it was lost — the screen changed under it, or an overlay
/// closed and took the focused node with it.
pub fn ensure_focus() {
    let Some(scope) = scope() else { return };
    if active_in_scope(&scope).is_none() {
        focus_first();
    }
}

// ---------------------------------------------------------------- geometry

#[derive(Clone, Copy)]
struct Rect {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    cx: f64,
    cy: f64,
}

/// One element's place in the document, measured once per key press.
///
/// Offsets rather than client rectangles, for two reasons. A client rectangle
/// is in viewport coordinates, which move while a rail or a column is in
/// flight, so a held-down arrow key would be answered from rectangles that are
/// still travelling. And a transform — which is how everything here moves —
/// changes the client rectangle but never the offset, so the same press gives
/// the same answer whether or not an animation is running.
///
/// The walk up the `offsetParent` chain is the expensive part, so each chain
/// is resolved once and reused by every element that shares it. On a screen of
/// six rails that turns a hundred walks into six.
struct Geometry {
    origins: Vec<(HtmlElement, (f64, f64))>,
}

impl Geometry {
    fn new() -> Self {
        Self {
            origins: Vec::with_capacity(8),
        }
    }

    fn origin(&mut self, parent: Option<web_sys::Element>) -> (f64, f64) {
        let Some(parent) = parent.and_then(|node| node.dyn_into::<HtmlElement>().ok()) else {
            return (0.0, 0.0);
        };
        if let Some((_, found)) = self.origins.iter().find(|(node, _)| node == &parent) {
            return *found;
        }
        let own = (parent.offset_left() as f64, parent.offset_top() as f64);
        let up = self.origin(parent.offset_parent());
        let resolved = (own.0 + up.0, own.1 + up.1);
        self.origins.push((parent, resolved));
        resolved
    }

    fn rect(&mut self, element: &HtmlElement) -> Rect {
        let (ox, oy) = self.origin(element.offset_parent());
        let left = element.offset_left() as f64 + ox;
        let top = element.offset_top() as f64 + oy;
        let width = element.offset_width() as f64;
        let height = element.offset_height() as f64;
        Rect {
            left,
            right: left + width,
            top,
            bottom: top + height,
            cx: left + width / 2.0,
            cy: top + height / 2.0,
        }
    }
}

thread_local! {
    /// The measured geometry of every focusable on screen, with the signature
    /// of the layout it was taken from.
    static TABLE: RefCell<Option<(Signature, Vec<(HtmlElement, Rect)>)>> =
        const { RefCell::new(None) };
}

/// Cheap evidence that the layout has not moved since it was measured.
///
/// Reading a hundred elements' offsets on every press cost eleven milliseconds
/// of a sixteen-millisecond frame, so the press landed a frame late and the
/// movement it started was already behind the remote. Between presses on a
/// settled screen none of that geometry changes — a card's box is fixed by its
/// width and aspect ratio, so even the artwork arriving leaves it alone. What
/// does change it is a rail appearing, an overlay opening or the window
/// resizing, and each of those changes one of these three numbers.
#[derive(PartialEq, Clone, Copy)]
struct Signature {
    count: u32,
    width: i32,
}

/// Measure the screen now, so the first press does not have to.
///
/// Building the table is one forced layout of everything focusable, and with
/// shelves that are skipped until needed that means laying out every shelf on
/// the screen. Measured on the appliance it is a 110 ms task — and it landed
/// on the first press of the remote, because that is when the table was first
/// asked for. Nothing about it has to happen then: the screen is standing
/// still and the viewer is still reading the top of it.
///
/// Called a few times after a screen arrives rather than once, because the
/// catalogue fills in over seconds and each shelf that appears changes what
/// there is to measure.
pub fn warm() {
    let Some(scope) = scope() else { return };
    let _ = measured(&scope);
}

/// Drop the measured layout. Called whenever a screen is replaced.
pub fn invalidate() {
    TABLE.with(|table| *table.borrow_mut() = None);
}

/// Every focusable on screen with its place in the document, measured at most
/// once per layout.
fn measured(scope: &Element) -> Vec<(HtmlElement, Rect)> {
    let found = candidates(scope);
    // The scroller's height is deliberately not part of this.
    //
    // It used to be, as a way of noticing that the layout had changed. With
    // shelves that are skipped until they are needed, the column's height
    // changes every time one of them is drawn for the first time — so walking
    // down the screen invalidated the table on almost every press and measured
    // all two hundred and eighty-three focusable things again. Measured on the
    // appliance: a 98 ms script inside one frame, 28 ms of it forced layout,
    // while the remote waited.
    //
    // What the height was there to catch is a screen being replaced, and that
    // already drops the table by hand; a resize changes the width, which is
    // still here.
    let now = Signature {
        count: found.len() as u32,
        width: scope.client_width(),
    };
    if let Some(cached) = TABLE.with(|table| {
        table
            .borrow()
            .as_ref()
            .filter(|(seen, _)| *seen == now)
            .map(|(_, rows)| rows.clone())
    }) {
        return cached;
    }
    let mut geometry = Geometry::new();
    let rows: Vec<(HtmlElement, Rect)> = found
        .into_iter()
        .map(|element| {
            let rect = geometry.rect(&element);
            (element, rect)
        })
        .collect();
    TABLE.with(|table| *table.borrow_mut() = Some((now, rows.clone())));
    rows
}

fn candidates(scope: &Element) -> Vec<HtmlElement> {
    let Ok(nodes) = scope.query_selector_all(&format!("[{FOCUS_ATTR}]")) else {
        return Vec::new();
    };
    (0..nodes.length())
        .filter_map(|index| nodes.item(index)?.dyn_into::<HtmlElement>().ok())
        .filter(|element| {
            is_visible(element) && !element.has_attribute("disabled") && !element.hidden()
        })
        .collect()
}

fn gap(a_start: f64, a_end: f64, b_start: f64, b_end: f64) -> f64 {
    (a_start.max(b_start) - a_end.min(b_end)).max(0.0)
}

/// How wrong a candidate is for this direction, or `None` if it is not in it.
///
/// Distance along the axis asked for, plus four times the drift off it. The
/// weighting is what expresses "directly below" as better than "closer, but
/// two columns over".
fn score(from: &Rect, to: &Rect, direction: Direction) -> Option<f64> {
    const THRESHOLD: f64 = 2.0;
    const CROSS_WEIGHT: f64 = 4.0;
    let (primary, cross) = match direction {
        Direction::Right => (
            to.cx - from.cx,
            gap(from.top, from.bottom, to.top, to.bottom),
        ),
        Direction::Left => (
            from.cx - to.cx,
            gap(from.top, from.bottom, to.top, to.bottom),
        ),
        Direction::Down => (
            to.cy - from.cy,
            gap(from.left, from.right, to.left, to.right),
        ),
        Direction::Up => (
            from.cy - to.cy,
            gap(from.left, from.right, to.left, to.right),
        ),
    };
    (primary > THRESHOLD).then_some(primary + CROSS_WEIGHT * cross)
}

/// Move the remote one step. Returns false when there is nothing that way, so
/// the caller can decide whether the press means something else.
pub fn step(direction: Direction) -> bool {
    let Some(scope) = scope() else { return false };
    let Some(current) = active_in_scope(&scope) else {
        focus_first();
        return true;
    };
    let rows = measured(&scope);
    let Some((_, from)) = rows.iter().find(|(element, _)| element == &current) else {
        // The remote is on something the table has never seen, so the screen
        // changed under it. Measure again rather than answer from stale numbers.
        invalidate();
        return false;
    };
    let from = *from;
    let mut best: Option<(f64, HtmlElement)> = None;
    for (element, to) in &rows {
        if element == &current {
            continue;
        }
        if let Some(value) = score(&from, to, direction)
            && best.as_ref().is_none_or(|(seen, _)| value < *seen)
        {
            best = Some((value, element.clone()));
        }
    }
    if let Some((_, element)) = best {
        focus(&element);
        return true;
    }

    // The end of a shelf, and the way out of it.
    //
    // "Tümünü gör" sits at the right-hand end of the shelf's title line, which
    // is where it reads correctly and where geometry can never find it: from
    // the first poster it is up and far across, so every Up lands on the shelf
    // above instead. Walking off the right-hand end of the shelf is the motion
    // that means "this row keeps going", so that is what opens it — and it
    // costs nothing to the vertical travel through the page, which putting it
    // in the Up path would have doubled.
    match direction {
        Direction::Right => rail_action(&current).inspect(|button| focus(button)).is_some(),
        // And back again, because a button you can enter and not leave is a
        // trap on a remote with no pointer.
        Direction::Left => current
            .class_list()
            .contains(RAIL_ACTION_CLASS)
            .then(|| last_in_rail(&current))
            .flatten()
            .inspect(|poster| focus(poster))
            .is_some(),
        _ => false,
    }
}

/// The class the rail's own action carries. Named here because the focus
/// engine has to recognise it without knowing what a rail is.
const RAIL_ACTION_CLASS: &str = "rail-all";

/// The action in the head of the shelf `element` sits in, if there is one and
/// the remote is not already on it.
fn rail_action(element: &HtmlElement) -> Option<HtmlElement> {
    if element.class_list().contains(RAIL_ACTION_CLASS) {
        return None;
    }
    nearest(element, ROW_ATTR)?
        .query_selector(&format!(".{RAIL_ACTION_CLASS}[{FOCUS_ATTR}]"))
        .ok()
        .flatten()
        .and_then(|found| found.dyn_into::<HtmlElement>().ok())
}

/// The last poster of the shelf `element` heads.
fn last_in_rail(element: &HtmlElement) -> Option<HtmlElement> {
    let row = nearest(element, ROW_ATTR)?;
    let nodes = row
        .query_selector_all(&format!("[{TRACK_ATTR}] [{FOCUS_ATTR}]"))
        .ok()?;
    (nodes.length() > 0)
        .then(|| nodes.item(nodes.length() - 1))
        .flatten()
        .and_then(|node| node.dyn_into::<HtmlElement>().ok())
}

/// True when the caret is in a text field, where Left and Right belong to the
/// field rather than to navigation.
pub fn editing() -> bool {
    active().is_some_and(|element| {
        element
            .tag_name()
            .eq_ignore_ascii_case("input")
            .then(|| {
                element
                    .get_attribute("type")
                    .is_none_or(|kind| matches!(kind.as_str(), "text" | "search" | "password"))
            })
            .unwrap_or(false)
    })
}

/// Activate whatever the remote is on.
pub fn activate() {
    if let Some(element) = active_focusable() {
        element.click();
    }
}
