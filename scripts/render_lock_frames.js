const fs = require("fs");
const path = require("path");
const puppeteer = require("puppeteer-core");

const ROOT = path.resolve(__dirname, "..");
const FRAMES_DIR = path.join(ROOT, "assets", "lock_frames");
const LOTTIE_JS_PATH = path.join(ROOT, "assets", "lottie_light.min.js");
const LOCK_JSON_PATH = path.join(ROOT, "assets", "lock.json");

const WIDTH = 200;
const HEIGHT = 160;

const LOCK_COLOR = "#FFB000";

const RANGES = [
  { name: "close", start: 35, end: 75, step: 2 },
  { name: "open", start: 95, end: 117, step: 1 },
];

function flattenLockOpacity(data) {
  const walk = (layers) => {
    for (const L of layers) {
      if (L.nm === "Lock" && L.ks && L.ks.o) {
        L.ks.o = { a: 0, k: 100 };
      }
      if (L.refId) {
        const comp = data.assets && data.assets.find((a) => a.id === L.refId);
        if (comp && comp.layers) walk(comp.layers);
      }
    }
  };
  if (data.layers) walk(data.layers);
}

const CHROME_PATHS = [
  "C:\\Program Files\\Google\\Chrome\\Application\\chrome.exe",
  "C:\\Program Files (x86)\\Google\\Chrome\\Application\\chrome.exe",
];

(async () => {
  const exe = CHROME_PATHS.find((p) => fs.existsSync(p));
  if (!exe) throw new Error("No Chrome found");

  const lottieJs = fs.readFileSync(LOTTIE_JS_PATH, "utf8");
  const lockData = JSON.parse(fs.readFileSync(LOCK_JSON_PATH, "utf8"));
  flattenLockOpacity(lockData);
  const lockJson = JSON.stringify(lockData);

  const browser = await puppeteer.launch({
    executablePath: exe,
    headless: "new",
    args: ["--no-sandbox", "--disable-web-security"],
  });
  const page = await browser.newPage();
  await page.setViewport({
    width: WIDTH,
    height: HEIGHT,
    deviceScaleFactor: 1,
  });

  const html = `<!DOCTYPE html><html><head><style>
html, body { margin: 0; padding: 0; background: transparent; overflow: hidden; width: ${WIDTH}px; height: ${HEIGHT}px }
#c { width: ${WIDTH}px; height: ${HEIGHT}px;
  filter:
    drop-shadow(1px 0 0 #1a0d00)
    drop-shadow(-1px 0 0 #1a0d00)
    drop-shadow(0 1px 0 #1a0d00)
    drop-shadow(0 -1px 0 #1a0d00)
    drop-shadow(0 2px 5px rgba(0,0,0,0.55));
}
svg path, svg g { stroke: ${LOCK_COLOR} !important; fill: ${LOCK_COLOR} !important }
</style></head><body>
<div id="c"></div>
<script>${lottieJs}</script>
<script>
window.__anim = lottie.loadAnimation({
  container: document.getElementById("c"),
  renderer: "svg",
  loop: false,
  autoplay: false,
  animationData: ${lockJson}
});
window.__seek = (f) => window.__anim.goToAndStop(f, true);
window.__ready = true;
</script>
</body></html>`;

  await page.setContent(html);
  await page.waitForFunction(() => window.__ready);
  await new Promise((r) => setTimeout(r, 300));

  fs.mkdirSync(FRAMES_DIR, { recursive: true });

  for (const range of RANGES) {
    let idx = 0;
    for (let f = range.start; f <= range.end; f += range.step) {
      await page.evaluate((frame) => window.__seek(frame), f);
      await new Promise((r) => setTimeout(r, 30));
      const buf = await page.screenshot({
        omitBackground: true,
        type: "png",
        clip: { x: 0, y: 0, width: WIDTH, height: HEIGHT },
      });
      const out = path.join(FRAMES_DIR, `${range.name}_${String(idx).padStart(2, "0")}.png`);
      fs.writeFileSync(out, buf);
      idx++;
    }
    console.log(`rendered ${idx} frames for ${range.name}`);
  }

  await browser.close();
})();
