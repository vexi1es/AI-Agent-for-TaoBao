const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
for (const t of list.filter(t => t.type === "page" && /browserLikeWindow|new-home|cart\.taobao|desktop-search/.test(t.url))) {
  const ws = new WebSocket(t.webSocketDebuggerUrl); await new Promise(r => ws.onopen = r);
  const v = await new Promise(r => { ws.onmessage = e => { const m = JSON.parse(e.data); if (m.id === 1) r(m.result?.result?.value); };
    ws.send(JSON.stringify({ id: 1, method: "Runtime.evaluate", params: { returnByValue: true, expression:
      `(()=>{const w=document.createTreeWalker(document.body,4);let x,ru=0,zh=0,s=[];while(x=w.nextNode()){const v=x.nodeValue.trim();if(!v)continue;if(/[\u4e00-\u9fff]/.test(v))zh++;else if(/[А-я]/.test(v)){ru++;if(s.length<14)s.push(v)}}return {tbtr:!!window.__tbtr,ru,zh,s}})()` } })); });
  ws.close(); console.log(t.url.split("?")[0].slice(0, 70), JSON.stringify(v));
}
