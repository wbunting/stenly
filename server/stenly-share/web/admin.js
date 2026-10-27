// Admin pages: render unix timestamps (`<time class="ts" data-ts>`) as local
// dates, including rows the hearth runtime adds or updates later.
const fmt = new Intl.DateTimeFormat(undefined, { dateStyle: "medium", timeStyle: "short" });

function paint(root) {
  for (const t of root.querySelectorAll("time.ts[data-ts]")) {
    const ts = Number(t.dataset.ts);
    if (!Number.isFinite(ts) || ts <= 0) continue;
    const text = fmt.format(new Date(ts * 1000));
    if (t.textContent !== text) t.textContent = text;
    t.dateTime = new Date(ts * 1000).toISOString();
  }
}

function start() {
  paint(document);
  let queued = false;
  new MutationObserver(() => {
    if (queued) return;
    queued = true;
    requestAnimationFrame(() => {
      queued = false;
      paint(document);
    });
  }).observe(document.body, { subtree: true, childList: true, characterData: true, attributes: true, attributeFilter: ["data-ts"] });
}

if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start);
else start();
