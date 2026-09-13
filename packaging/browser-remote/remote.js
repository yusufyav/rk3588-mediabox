// A remote control for a web application that was written for a mouse.
//
// Stremio's interface has no keyboard navigation worth the name — upstream
// tracks it as an open problem — so on a television it is a wall of posters
// that nothing can reach. This adds the missing half: every clickable thing
// becomes a place the four arrows can land, the one the remote is on is
// ringed, OK clicks it and Back goes back.
//
// Geometry decides what each arrow means, not document order: document order
// is wrong the moment a shelf wraps. Each candidate is scored by how far it is
// in the direction asked for, plus a heavy penalty for drifting off that axis.
(() => {
  "use strict";

  const CLICKABLE = [
    "a[href]",
    "button",
    "input",
    "textarea",
    "select",
    "[role='button']",
    "[role='link']",
    "[role='tab']",
    "[tabindex]:not([tabindex='-1'])",
    // Stremio builds everything out of divs, and its class names are hashed.
    // The singular is a poster, the plural is the shelf that holds ten of
    // them: `a.meta-item-QFHCh` inside `div.meta-items-container-qcuUA`. The
    // shelf must never be a place to stand, so it is excluded by name rather
    // than guessed at by counting children.
    "[class*='meta-item-']:not([class*='meta-items-'])",
    "[class*='button-container']",
    "[class*='nav-tab']",
    "[class*='option-container']",
    "[class*='stream-container']",
    "[class*='video-container']",
  ].join(",");

  const seen = new WeakSet();
  let current = null;

  const visible = (element) => {
    const box = element.getBoundingClientRect();
    if (box.width < 8 || box.height < 8) return false;
    if (box.bottom < 0 || box.top > innerHeight) return false;
    if (box.right < 0 || box.left > innerWidth) return false;
    const style = getComputedStyle(element);
    return style.visibility !== "hidden" && style.display !== "none" && style.opacity !== "0";
  };

  // A shelf is not a place to stand. These pages give a row of posters a class
  // that matches the same patterns its posters do, so without this the remote
  // lands on the whole row — every title in it at once, ringed as one enormous
  // box. An element that holds two or more other candidates is a container;
  // one that holds at most its own inner anchor is the thing itself.
  const candidates = () => {
    const all = Array.from(document.querySelectorAll(CLICKABLE)).filter(visible);
    // Two rules, because these pages break either one alone. The names tell a
    // shelf from a poster (`meta-items-` from `meta-item-`), and the count
    // catches everything else that wraps several places to stand — a row given
    // a tabindex of its own, for instance.
    return all.filter((element) => {
      let inside = 0;
      for (const other of all) {
        if (other !== element && element.contains(other)) inside += 1;
        if (inside > 1) return false;
      }
      return true;
    });
  };

  const rect = (element) => {
    const box = element.getBoundingClientRect();
    return { ...box.toJSON(), cx: box.left + box.width / 2, cy: box.top + box.height / 2 };
  };

  const gap = (fromLow, fromHigh, toLow, toHigh) =>
    toHigh < fromLow ? fromLow - toHigh : toLow > fromHigh ? toLow - fromHigh : 0;

  const score = (from, to, direction) => {
    let primary, cross;
    if (direction === "ArrowRight") {
      primary = to.cx - from.cx;
      cross = gap(from.top, from.bottom, to.top, to.bottom);
    } else if (direction === "ArrowLeft") {
      primary = from.cx - to.cx;
      cross = gap(from.top, from.bottom, to.top, to.bottom);
    } else if (direction === "ArrowDown") {
      primary = to.cy - from.cy;
      cross = gap(from.left, from.right, to.left, to.right);
    } else {
      primary = from.cy - to.cy;
      cross = gap(from.left, from.right, to.left, to.right);
    }
    if (primary <= 2) return null;
    return primary + cross * 4;
  };

  const mark = (element) => {
    if (current === element) return;
    if (current) current.classList.remove("mbx-focus");
    current = element;
    if (!element) return;
    element.classList.add("mbx-focus");
    if (!seen.has(element)) {
      seen.add(element);
      if (!element.hasAttribute("tabindex")) element.setAttribute("tabindex", "-1");
    }
    try {
      element.focus({ preventScroll: true });
    } catch (_) {}
    element.scrollIntoView({ block: "nearest", inline: "nearest" });
  };

  const first = () => {
    const list = candidates();
    if (!list.length) return;
    // The top-left thing on screen, which on every one of these pages is where
    // a person would start reading.
    list.sort((a, b) => {
      const ra = rect(a);
      const rb = rect(b);
      return ra.cy - rb.cy || ra.cx - rb.cx;
    });
    mark(list[0]);
  };

  const step = (direction) => {
    if (!current || !document.contains(current) || !visible(current)) {
      first();
      return;
    }
    const from = rect(current);
    let best = null;
    let bestScore = Infinity;
    for (const element of candidates()) {
      if (element === current) continue;
      const value = score(from, rect(element), direction);
      if (value !== null && value < bestScore) {
        bestScore = value;
        best = element;
      }
    }
    if (best) mark(best);
  };

  const press = () => {
    if (!current) return;
    // A real click: these pages listen for mouse events, not for Enter.
    const options = { bubbles: true, cancelable: true, view: window };
    current.dispatchEvent(new PointerEvent("pointerdown", options));
    current.dispatchEvent(new MouseEvent("mousedown", options));
    current.dispatchEvent(new PointerEvent("pointerup", options));
    current.dispatchEvent(new MouseEvent("mouseup", options));
    current.click();
  };

  addEventListener(
    "keydown",
    (event) => {
      const typing =
        document.activeElement &&
        ["INPUT", "TEXTAREA"].includes(document.activeElement.tagName);
      switch (event.key) {
        case "ArrowUp":
        case "ArrowDown":
          if (typing) return;
          event.preventDefault();
          step(event.key);
          return;
        case "ArrowLeft":
        case "ArrowRight":
          if (typing) return;
          event.preventDefault();
          step(event.key);
          return;
        case "Enter":
          if (typing) return;
          event.preventDefault();
          press();
          return;
        case "Escape":
        case "Backspace":
          if (typing) return;
          event.preventDefault();
          history.back();
          return;
        default:
      }
    },
    true,
  );

  // The page builds itself after load and rebuilds on every navigation, so the
  // starting point is claimed once there is something to claim.
  const settle = setInterval(() => {
    if (!current || !document.contains(current)) {
      if (candidates().length) first();
    }
  }, 800);
  addEventListener("pagehide", () => clearInterval(settle));
})();
