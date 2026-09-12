//! Directional focus for a remote control.
//!
//! A television has four arrows and an OK button, so "what does Right mean
//! here" has to be answered by the layout itself rather than by DOM order —
//! DOM order is wrong the moment a rail wraps or a grid reflows. Every
//! candidate is scored against the focused element's rectangle: how far it is
//! in the direction asked for, plus a heavy penalty for drifting off the axis.
//! That is what makes Down from a poster land on the poster below it rather
//! than on whatever happens to be next in the document.
//!
//! Two invariants hold everywhere:
//!
//! * focus is never lost — if the focused element disappears, the first
//!   focusable element of the current scope takes it;
//! * an overlay traps focus — while one is open it is the scope, so no arrow
//!   press can reach the page behind it.

use wasm_bindgen::JsCast;
use web_sys::{Element, HtmlElement, ScrollBehavior, ScrollIntoViewOptions, ScrollLogicalPosition};

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
const FOCUSED_CLASS: &str = "is-focused";

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

fn is_visible(element: &HtmlElement) -> bool {
    let rect = element.get_bounding_client_rect();
    rect.width() > 1.0 && rect.height() > 1.0
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

fn active() -> Option<HtmlElement> {
    document()?.active_element()?.dyn_into::<HtmlElement>().ok()
}

/// The element the remote is on, if it is still both present and focusable.
fn active_in_scope(scope: &Element) -> Option<HtmlElement> {
    let active = active()?;
    (active.has_attribute(FOCUS_ATTR) && scope.contains(Some(&active)) && is_visible(&active))
        .then_some(active)
}

/// Give one element focus, and make that unmistakable on a television.
pub fn focus(element: &HtmlElement) {
    if let Some(document) = document()
        && let Ok(previous) = document.query_selector_all(&format!(".{FOCUSED_CLASS}"))
    {
        for index in 0..previous.length() {
            if let Some(node) = previous.item(index).and_then(|n| n.dyn_into::<Element>().ok()) {
                let _ = node.class_list().remove_1(FOCUSED_CLASS);
            }
        }
    }
    let _ = element.class_list().add_1(FOCUSED_CLASS);
    let _ = element.focus();

    // `nearest` vertically keeps a rail from jumping the page; `center`
    // horizontally is what makes a rail scroll as you walk along it.
    //
    // Smooth, because focus is scored in layout space and no longer cares
    // where the scroll has got to. A gradual scroll also moves far less of the
    // screen per frame than a jump does, which is the difference between a
    // television keeping up and not.
    let options = ScrollIntoViewOptions::new();
    options.set_behavior(ScrollBehavior::Smooth);
    options.set_block(ScrollLogicalPosition::Nearest);
    options.set_inline(ScrollLogicalPosition::Center);
    element.scroll_into_view_with_scroll_into_view_options(&options);
}

/// Put focus somewhere sensible in the current scope.
///
/// `data-autofocus` marks the element a screen wants to start on; otherwise the
/// first focusable element wins. Called after every screen change and whenever
/// the focused element is found to have gone away.
pub fn focus_first() {
    let Some(scope) = scope() else { return };
    let preferred = scope
        .query_selector(&format!("[{FOCUS_ATTR}][data-autofocus]"))
        .ok()
        .flatten()
        .and_then(|element| element.dyn_into::<HtmlElement>().ok())
        .filter(is_visible);
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
    if active().is_some_and(|current| current == target) {
        return true;
    }
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

struct Rect {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
    cx: f64,
    cy: f64,
}

/// Where an element sits in the document, not in the viewport.
///
/// Viewport coordinates move while a rail or the page is scrolling, so scoring
/// against them lets a held-down arrow key be answered from rectangles that
/// are still in flight — and the focus lands somewhere nobody aimed at. Layout
/// offsets do not move, so the same press gives the same answer whether or not
/// an animation is running. Offsets ignore CSS transforms, which is why focus
/// is drawn with an outline rather than a scale.
fn rect_of(element: &HtmlElement) -> Rect {
    let (mut left, mut top) = (element.offset_left() as f64, element.offset_top() as f64);
    let mut parent = element.offset_parent();
    while let Some(node) = parent {
        let Ok(node) = node.dyn_into::<HtmlElement>() else {
            break;
        };
        left += node.offset_left() as f64;
        top += node.offset_top() as f64;
        parent = node.offset_parent();
    }
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
    let from = rect_of(&current);
    let best = candidates(&scope)
        .into_iter()
        .filter(|element| element != &current)
        .filter_map(|element| {
            let to = rect_of(&element);
            score(&from, &to, direction).map(|value| (value, element))
        })
        .min_by(|(left, _), (right, _)| left.total_cmp(right));
    match best {
        Some((_, element)) => {
            focus(&element);
            true
        }
        None => false,
    }
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
    if let Some(element) = active() {
        element.click();
    }
}
