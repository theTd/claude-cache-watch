"use strict";
(() => {
  const listeners = new Set();
  let active = false, checking = false, recheck = false;
  window.dashboardRefresh = {
    get active() { return active; },
    subscribe(listener) {
      listeners.add(listener);
      if (active) listener(true);
    },
  };
  async function check() {
    if (checking) { recheck = true; return; }
    checking = true;
    try {
      const visible = await window.__TAURI__.core.invoke("dashboard_active");
      const resumed = visible && !active;
      active = visible;
      if (active) for (const listener of listeners) listener(resumed);
    } catch (error) {
      active = false;
      console.error("Cannot check dashboard visibility", error);
    } finally {
      checking = false;
      if (recheck) { recheck = false; check(); }
    }
  }
  window.addEventListener("focus", check);
  document.addEventListener("visibilitychange", check);
  setInterval(check, 3000);
  check();
})();
