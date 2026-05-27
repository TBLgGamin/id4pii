(function () {
  const BRIDGE_URL = "ws://127.0.0.1:7878/ws";
  const INSTALLER_URL = "__ID4PII_INSTALLER_URL__";
  const PROBE_TIMEOUT_MS = 1500;
  const POLL_INTERVAL_MS = 2500;

  const steps = {
    install:   document.querySelector('[data-step="install"]'),
    waiting:   document.querySelector('[data-step="waiting"]'),
    connected: document.querySelector('[data-step="connected"]'),
  };

  const downloadEl = document.getElementById("download");
  const haveItEl = document.getElementById("have-it");
  const backEl = document.getElementById("back-to-install");
  const closeEl = document.getElementById("close-tab");
  const confettiCanvas = document.getElementById("confetti");

  if (downloadEl) downloadEl.href = INSTALLER_URL;

  let currentStep = "install";
  let userAdvanced = false;
  document.body.classList.add("layout-split");

  function show(name) {
    if (currentStep === name) return;
    const prev = currentStep;
    currentStep = name;
    for (const [k, el] of Object.entries(steps)) {
      el.classList.toggle("hidden", k !== name);
    }
    document.body.classList.toggle("layout-split", name === "install" || name === "waiting");
    if (name === "connected" && prev !== "connected") {
      fireConfetti();
    }
  }

  if (downloadEl) {
    downloadEl.addEventListener("click", () => {
      userAdvanced = true;
      setTimeout(() => show("waiting"), 350);
    });
  }
  if (haveItEl) {
    haveItEl.addEventListener("click", () => {
      userAdvanced = true;
      show("waiting");
    });
  }
  if (backEl) {
    backEl.addEventListener("click", (e) => {
      e.preventDefault();
      userAdvanced = false;
      show("install");
    });
  }
  if (closeEl) {
    closeEl.addEventListener("click", () => window.close());
  }

  function probe() {
    let settled = false;
    let socket;
    try {
      socket = new WebSocket(BRIDGE_URL);
    } catch (_err) {
      onDown();
      return;
    }
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try { socket.close(); } catch (_) {}
      onDown();
    }, PROBE_TIMEOUT_MS);
    socket.addEventListener("open", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try { socket.close(); } catch (_) {}
      onUp();
    });
    socket.addEventListener("error", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      onDown();
    });
    socket.addEventListener("close", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      onDown();
    });
  }

  function onUp() {
    show("connected");
  }

  function onDown() {
    if (currentStep === "connected") {
      show(userAdvanced ? "waiting" : "install");
    }
    setTimeout(probe, POLL_INTERVAL_MS);
  }

  probe();

  function fireConfetti() {
    if (window.matchMedia && window.matchMedia("(prefers-reduced-motion: reduce)").matches) return;
    if (typeof confetti !== "function" || !confettiCanvas) return;
    const shoot = confetti.create(confettiCanvas, { resize: true, useWorker: false });
    const palette = ["#fafafa", "#e4e4e7", "#a1a1aa", "#2eb88a", "#52525b"];
    const defaults = {
      spread: 70,
      ticks: 240,
      gravity: 0.9,
      decay: 0.94,
      startVelocity: 32,
      colors: palette,
      scalar: 0.95,
      shapes: ["square", "circle"],
    };
    function burst(opts) { shoot(Object.assign({}, defaults, opts)); }
    burst({ particleCount: 60, origin: { x: 0.5, y: 0.55 } });
    setTimeout(() => burst({ particleCount: 40, angle: 60,  spread: 55, origin: { x: 0, y: 0.7 } }), 120);
    setTimeout(() => burst({ particleCount: 40, angle: 120, spread: 55, origin: { x: 1, y: 0.7 } }), 240);
    setTimeout(() => burst({ particleCount: 30, origin: { x: 0.5, y: 0.5 }, scalar: 1.1, startVelocity: 25 }), 380);
  }
})();
