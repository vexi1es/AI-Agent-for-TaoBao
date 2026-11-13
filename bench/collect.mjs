const list = await (await fetch("http://127.0.0.1:9333/json/list")).json();
const pages = list.filter(t => t.type==="page" && /^https:\/\/.*(taobao|tmall)\.com/.test(t.url) && !/login/.test(t.url));
async function evalIn(t, expr){
  const ws = new WebSocket(t.webSocketDebuggerUrl);
  await new Promise((r,j)=>{ws.onopen=r;ws.onerror=j});
  const res = await new Promise(r=>{ ws.onmessage=e=>{const m=JSON.parse(e.data); if(m.id===1) r(m);}; ws.send(JSON.stringify({id:1,method:"Runtime.evaluate",params:{expression:expr,returnByValue:true}})); });
  ws.close(); return res.result?.result?.value ?? [];
}
const code = `(()=>{const out=new Set();const w=document.createTreeWalker(document.body,NodeFilter.SHOW_TEXT);let x;
while(x=w.nextNode()){const s=x.nodeValue.trim(); if(/[\u4e00-\u9fff]/.test(s)&&s.length<=80)out.add(s);}
document.querySelectorAll('[placeholder]').forEach(e=>{const p=e.getAttribute('placeholder');if(/[\u4e00-\u9fff]/.test(p))out.add(p)});
return [...out];})()`;
const all = new Set();
for (const p of pages) { try { for (const s of await evalIn(p, code)) all.add(s); console.error(p.url.slice(0,80)); } catch(e){ console.error("skip", p.url.slice(0,60)); } }
const fs = await import("fs"); fs.writeFileSync("phrases.json", JSON.stringify([...all], null, 1));
console.log("total", all.size);
