const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const expr = `JSON.stringify([...document.querySelectorAll("input,textarea,[contenteditable=true]")].filter(e => !["hidden","checkbox","radio","password"].includes(e.type)).map(e => {
  const r = e.getBoundingClientRect();
  return { tag: e.tagName, type: e.type, ph: e.getAttribute("placeholder"), cls: String(e.className).slice(0, 60), id: e.id, name: e.name,
    form: !!e.form, action: e.form && e.form.action, vis: r.width > 0 && r.height > 0, val: e.value };
}))`;
for (const t of list.filter(t => t.type === "page" && t.webSocketDebuggerUrl)) {
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  const timeout = new Promise(r => setTimeout(() => r(null), 3000));
  const res = await Promise.race([timeout, (async () => {
    await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
    return new Promise(r => { ws.onmessage = m => r(JSON.parse(m.data)); ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { expression: expr } })); });
  })()]).catch(() => null);
  ws.close();
  if (!res) { console.log("-- no answer:", t.url.slice(0, 80)); continue; }
  const v = JSON.parse(res.result?.result?.value || "[]");
  if (v.length) { console.log("==", t.url.slice(0, 110)); for (const x of v) console.log("  ", JSON.stringify(x)); }
}
