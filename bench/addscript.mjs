const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const t = list.find(t => /new-home/.test(t.url));
const ws = new WebSocket(t.webSocketDebuggerUrl); await new Promise(r => ws.onopen = r);
let id = 0; const call = (method, params = {}) => new Promise(r => { const i = ++id; const h = e => { const m = JSON.parse(e.data); if (m.id === i) { ws.removeEventListener("message", h); r(m); } }; ws.addEventListener("message", h); ws.send(JSON.stringify({ id: i, method, params })); });
console.log("addScript без Page.enable:", JSON.stringify(await call("Page.addScriptToEvaluateOnNewDocument", { source: "window.__probe=1" })).slice(0, 200));
ws.close();
