window.id4pii = window.id4pii || {};
window.id4pii.ui = (() => {
  const SIZE = 40;
  const GAP = 6;
  const PLAY_MS = 540;
  const HOLD_MS = 160;
  const FADE_MS = 220;

  let mouseX = -1;
  let mouseY = -1;
  let active = null;

  function trackPointer(e) {
    mouseX = e.clientX;
    mouseY = e.clientY;
    if (active) place(active.host);
  }
  window.addEventListener("pointermove", trackPointer, true);
  window.addEventListener("mousemove", trackPointer, true);

  function place(host) {
    let left = mouseX < 0 ? window.innerWidth - SIZE - 16 : mouseX + GAP;
    let top = mouseY < 0 ? 16 : mouseY - SIZE + 4;
    left = Math.max(4, Math.min(window.innerWidth - SIZE - 4, left));
    top = Math.max(4, Math.min(window.innerHeight - SIZE - 4, top));
    host.style.left = `${left}px`;
    host.style.top = `${top}px`;
  }

  function markup(kind) {
    const blocked = kind === "blocked";
    const closing = kind !== "restore";
    const accent = blocked ? "#E5484D" : "#F5A524";
    const hole = blocked ? "#5C1A1C" : "#7A4E06";
    const shackleKeyframes = closing
      ? "@keyframes id4-shackle{0%{transform:rotate(-46deg)}58%{transform:rotate(7deg)}100%{transform:rotate(0)}}"
      : "@keyframes id4-shackle{0%{transform:rotate(0)}100%{transform:rotate(-46deg)}}";
    return `
<style>
  .wrap{width:100%;height:100%;animation:id4-pop ${PLAY_MS}ms cubic-bezier(.2,.85,.25,1.15) both}
  svg{width:100%;height:100%;display:block;overflow:visible;filter:drop-shadow(0 1px 3px rgba(0,0,0,.4))}
  .body{fill:${accent}}
  .hole{fill:${hole}}
  .shackle{fill:none;stroke:${accent};stroke-width:2.4;stroke-linecap:round;transform-box:fill-box;transform-origin:100% 100%;animation:id4-shackle ${PLAY_MS}ms cubic-bezier(.34,1.35,.5,1) both}
  @keyframes id4-pop{0%{transform:scale(.55);opacity:0}40%{opacity:1}100%{transform:scale(1);opacity:1}}
  ${shackleKeyframes}
</style>
<div class="wrap">
  <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
    <path class="shackle" d="M7.5 11 V8 a4.5 4.5 0 0 1 9 0 V11" />
    <rect class="body" x="4.5" y="10.5" width="15" height="11" rx="2.6" />
    <circle class="hole" cx="12" cy="15" r="1.7" />
    <rect class="hole" x="11.2" y="15" width="1.6" height="3.4" rx="0.8" />
  </svg>
</div>`;
  }

  function signal(kind) {
    if (active) return;

    const host = document.createElement("div");
    host.dataset.id4pii = "lock";
    host.style.cssText = `all:initial;position:fixed;width:${SIZE}px;height:${SIZE}px;z-index:2147483647;pointer-events:none;opacity:1;transition:opacity ${FADE_MS}ms ease`;
    const shadow = host.attachShadow({ mode: "open" });
    shadow.innerHTML = markup(kind);
    (document.body || document.documentElement).appendChild(host);
    place(host);

    let removed = false;
    const cleanup = () => {
      if (removed) return;
      removed = true;
      host.remove();
      if (active && active.host === host) active = null;
    };
    active = { host, cleanup };

    setTimeout(() => {
      host.style.opacity = "0";
      setTimeout(cleanup, FADE_MS);
    }, PLAY_MS + HOLD_MS);
  }

  return { signal };
})();
