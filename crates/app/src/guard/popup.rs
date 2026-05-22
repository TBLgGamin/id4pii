const TEMPLATE: &str = r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<style>
:root { color-scheme: dark; }
* { box-sizing: border-box; margin: 0; }
html, body { height: 100%; }
body {
  background: hsl(240 10% 3.9%);
  color: hsl(0 0% 98%);
  font-family: "Segoe UI", system-ui, sans-serif;
  display: flex;
  flex-direction: column;
  user-select: none;
}
header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 14px 16px;
  border-bottom: 1px solid hsl(240 3.7% 15.9%);
}
header h1 { font-size: 13px; font-weight: 600; }
header .badge {
  font-size: 11px;
  color: hsl(160 60% 55%);
  border: 1px solid hsl(240 3.7% 15.9%);
  border-radius: 999px;
  padding: 2px 9px;
}
main { flex: 1; overflow: auto; padding: 16px; }
.text {
  white-space: pre-wrap;
  word-break: break-word;
  user-select: text;
  font-size: 13px;
  line-height: 1.55;
  background: hsl(240 3.7% 11%);
  border: 1px solid hsl(240 3.7% 15.9%);
  border-radius: 8px;
  padding: 12px 14px;
}
footer {
  display: flex;
  gap: 8px;
  justify-content: flex-end;
  padding: 12px 16px;
  border-top: 1px solid hsl(240 3.7% 15.9%);
}
button {
  font: inherit;
  font-size: 12px;
  font-weight: 500;
  padding: 7px 14px;
  border-radius: 6px;
  cursor: pointer;
  border: 1px solid transparent;
  transition: opacity .12s;
}
button:hover { opacity: .85; }
.primary { background: hsl(0 0% 98%); color: hsl(240 5.9% 10%); }
.ghost { background: transparent; color: hsl(0 0% 98%); border-color: hsl(240 3.7% 15.9%); }
</style>
</head>
<body>
<header>
  <h1>id4pii — restored text</h1>
  <span class="badge">PII unmasked</span>
</header>
<main><div class="text" id="content"></div></main>
<footer>
  <button class="ghost" id="copy">Copy</button>
  <button class="primary" id="close">Close</button>
</footer>
<script>
  const restored = __TEXT__;
  document.getElementById("content").textContent = restored;
  document.getElementById("close").addEventListener("click", () => window.ipc.postMessage("close"));
  document.getElementById("copy").addEventListener("click", () => window.ipc.postMessage("copy"));
  document.addEventListener("keydown", (e) => {
    if (e.key === "Escape") window.ipc.postMessage("close");
  });
</script>
</body>
</html>
"#;

pub(crate) fn page(restored: &str) -> String {
    let literal = serde_json::to_string(restored).unwrap_or_else(|_| String::from("\"\""));
    TEMPLATE.replace("__TEXT__", &literal)
}
