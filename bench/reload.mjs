const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const t = list.find(t => /new-home/.test(t.url));
const ws = new WebSocket(t.webSocketDebuggerUrl); await new Promise(r => ws.onopen = r);
ws.send(JSON.stringify({ id: 1, method: "Page.reload", params: {} })); await new Promise(r => setTimeout(r, 300)); ws.close(); console.log("reloaded");
