(function () {
  const BRIDGE_URL = "ws://127.0.0.1:7878/ws";
  const INSTALLER_URL = "__ID4PII_INSTALLER_URL__";
  const PROBE_TIMEOUT_MS = 1500;
  const POLL_INTERVAL_MS = 2500;

  const checkingEl = document.getElementById("checking");
  const missingEl = document.getElementById("missing");
  const connectedEl = document.getElementById("connected");
  const downloadEl = document.getElementById("download");
  const closeEl = document.getElementById("close-tab");

  if (downloadEl) downloadEl.href = INSTALLER_URL;
  if (closeEl) {
    closeEl.addEventListener("click", (e) => {
      e.preventDefault();
      window.close();
    });
  }

  let state = "checking";

  function setState(next) {
    if (state === next) return;
    state = next;
    checkingEl.classList.toggle("hidden", next !== "checking");
    missingEl.classList.toggle("hidden", next !== "missing");
    connectedEl.classList.toggle("hidden", next !== "connected");
  }

  function probe() {
    let settled = false;
    let socket;
    try {
      socket = new WebSocket(BRIDGE_URL);
    } catch (_err) {
      setState("missing");
      schedule();
      return;
    }
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      try {
        socket.close();
      } catch (_) {}
      setState("missing");
      schedule();
    }, PROBE_TIMEOUT_MS);
    socket.addEventListener("open", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      try {
        socket.close();
      } catch (_) {}
      setState("connected");
    });
    socket.addEventListener("error", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      setState("missing");
      schedule();
    });
    socket.addEventListener("close", () => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      setState("missing");
      schedule();
    });
  }

  function schedule() {
    if (state === "connected") return;
    setTimeout(probe, POLL_INTERVAL_MS);
  }

  probe();
})();
