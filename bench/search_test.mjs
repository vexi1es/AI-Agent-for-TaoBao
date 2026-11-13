const [part, query] = process.argv.slice(2);
const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const t = list.find(t => t.type === "page" && t.url.includes(part));
if (!t) { console.log("нет вкладки", part); process.exit(1); }
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => (ws.onopen = r));
let n = 0; const waiters = new Map();
ws.onmessage = m => { const v = JSON.parse(m.data); if (waiters.has(v.id)) { waiters.get(v.id)(v); waiters.delete(v.id); } };
const call = (method, params = {}) => new Promise(r => { const id = ++n; waiters.set(id, r); ws.send(JSON.stringify({ id, method, params })); });
const val = async e => (await call("Runtime.evaluate", { expression: e, returnByValue: true })).result?.result?.value;

await val(`(() => { const e = document.querySelector("input#q, input[name=q]"); e.focus(); e.select(); return !!e })()`);
await call("Input.insertText", { text: query });
console.log("в поле:", await val(`document.activeElement.value`));
const key = type => call("Input.dispatchKeyEvent", { type, key: "Enter", code: "Enter", windowsVirtualKeyCode: 13, text: type === "keyDown" ? "\r" : undefined });
await key("keyDown"); await key("keyUp");
for (let i = 0; i < 20; i++) {
  await new Promise(r => setTimeout(r, 500));
  const v = await val(`(document.querySelector("input#q, input[name=q]")||{}).value`);
  if (v && v !== query) { console.log(`через ${(i + 1) * 0.5} с в поле:`, v); break; }
}
await new Promise(r => setTimeout(r, 4000));
ws.close();
const after = await (await fetch("http://127.0.0.1:9333/json/list")).json();
for (const x of after.filter(x => x.type === "page" && /search|s\.taobao|q=/.test(x.url))) console.log("страница:", decodeURIComponent(x.url).slice(0, 160));
