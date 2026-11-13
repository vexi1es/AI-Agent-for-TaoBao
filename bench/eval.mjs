const [part, expr] = process.argv.slice(2);
const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const t = list.find(t => t.type === "page" && t.url.includes(part));
if (!t) { console.log("нет вкладки", part); process.exit(1); }
const ws = new WebSocket(t.webSocketDebuggerUrl);
await new Promise(r => (ws.onopen = r));
const v = await new Promise(r => {
  ws.onmessage = m => r(JSON.parse(m.data));
  ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { expression: expr, returnByValue: true, awaitPromise: true } }));
});
ws.close();
const x = v.result?.result;
console.log(x?.value ?? x?.description ?? JSON.stringify(v));
