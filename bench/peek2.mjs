const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const t = list.find(t => /browserLikeWindow/.test(t.url));
const ws = new WebSocket(t.webSocketDebuggerUrl); await new Promise(r => ws.onopen = r);
const v = await new Promise(r => { ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id === 1) r(m.result); };
  ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { returnByValue: true, expression:
  `({has:!!window.__tbtr, reset: typeof window.__tbtr?.reset, send: typeof window.__tbtr_send, side:[...document.querySelectorAll('span,div')].map(e=>e.childNodes.length===1&&e.firstChild.nodeType===3?e.textContent.trim():'').filter(s=>s&&s.length<30).slice(0,25)})` } })); });
ws.close(); console.log(JSON.stringify(v, null, 0));
