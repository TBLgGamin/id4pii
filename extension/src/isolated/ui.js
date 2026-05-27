window.id4pii = window.id4pii || {};
window.id4pii.ui = (() => {
  const SIZE_W = 200;
  const SIZE_H = 160;
  const MARGIN = 12;
  const HOLD_MS = 120;
  const FADE_MS = 200;
  const FRAME_MS = 16;
  const CLOSE_COUNT = 21;
  const OPEN_COUNT = 23;

  function frameUrls(kind) {
    const prefix = kind === "anonymize" ? "close_" : "open_";
    const count = kind === "anonymize" ? CLOSE_COUNT : OPEN_COUNT;
    const urls = [];
    for (let i = 0; i < count; i++) {
      const idx = String(i).padStart(2, "0");
      urls.push(chrome.runtime.getURL(`assets/lock_frames/${prefix}${idx}.png`));
    }
    return urls;
  }

  function resolveRect(anchor) {
    if (!anchor) return null;
    if (typeof anchor.getBoundingClientRect === "function") return anchor.getBoundingClientRect();
    if (typeof anchor === "object" && "left" in anchor) return anchor;
    return null;
  }

  function placement(anchor) {
    const rect = resolveRect(anchor);
    if (!rect) {
      return {
        left: Math.max(MARGIN, window.innerWidth - SIZE_W - MARGIN),
        top: MARGIN,
      };
    }
    let desiredLeft;
    let desiredTop;
    if (anchor && anchor.cursor) {
      desiredLeft = rect.left - 70;
      desiredTop = rect.top - 50;
    } else {
      desiredLeft = rect.left + rect.width - SIZE_W;
      desiredTop = rect.top - SIZE_H - 8;
    }
    return {
      left: Math.max(MARGIN, Math.min(window.innerWidth - SIZE_W - MARGIN, desiredLeft)),
      top: Math.max(MARGIN, Math.min(window.innerHeight - SIZE_H - MARGIN, desiredTop)),
    };
  }

  function show(kind, anchor) {
    const { left, top } = placement(anchor);
    const overlay = document.createElement("div");
    overlay.dataset.id4pii = "overlay";
    overlay.style.cssText = `position:fixed;left:${left}px;top:${top}px;width:${SIZE_W}px;height:${SIZE_H}px;z-index:2147483647;pointer-events:none;opacity:1;transition:opacity ${FADE_MS}ms linear;`;
    const img = document.createElement("img");
    img.alt = "";
    img.style.cssText = "width:100%;height:100%;object-fit:contain;display:block;";
    overlay.appendChild(img);
    (document.body || document.documentElement).appendChild(overlay);

    const urls = frameUrls(kind);
    let i = 0;
    const tick = () => {
      if (i >= urls.length) {
        setTimeout(() => {
          overlay.style.opacity = "0";
          setTimeout(() => overlay.remove(), FADE_MS);
        }, HOLD_MS);
        return;
      }
      img.src = urls[i++];
      setTimeout(tick, FRAME_MS);
    };
    tick();
  }

  return { show };
})();
