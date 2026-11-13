import fs from "fs";
const [, , inp, outp] = process.argv;
const src = JSON.parse(fs.readFileSync(inp, "utf8"));
const SYS = "Ты переводчик интерфейса китайского маркетплейса Taobao на русский. Переводи кратко и естественно, как в русском интернет-магазине. Бренды и латиницу не трогай. 包邮=бесплатная доставка, 宝贝=товар, 亲=(опусти обращение), 店铺=магазин, 已售=продано, 淘金币=Таобао-монеты. Отвечай ТОЛЬКО JSON-массивом строк той же длины и порядка.";
async function ask(batch) {
  const r = await fetch("http://127.0.0.1:8088/v1/chat/completions", { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.1, max_tokens: 2048, messages: [{ role: "system", content: SYS }, { role: "user", content: JSON.stringify(batch, null, 0) }] }) });
  const j = await r.json(); const txt = j.choices[0].message.content.replace(/^```(json)?|```$/gm, "").trim();
  try { const a = JSON.parse(txt); if (a.length === batch.length) return a; } catch {}
  return Promise.all(batch.map(async s => (await ask1(s))));
}
async function ask1(s) {
  const r = await fetch("http://127.0.0.1:8088/v1/chat/completions", { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.1, max_tokens: 200, messages: [{ role: "system", content: SYS.replace(/Отвечай.*$/, "Отвечай только переводом.") }, { role: "user", content: s }] }) });
  return (await r.json()).choices[0].message.content.trim();
}
await ask1("删除");
let t = performance.now(); const out = [];
for (let i = 0; i < src.length; i += 10) out.push(...await ask(src.slice(i, i + 10)));
const total = (performance.now() - t) / 1000;
t = performance.now(); for (const s of src.slice(0, 20)) await ask1(s); const per = (performance.now() - t) / 20 / 1000;
fs.writeFileSync(outp, JSON.stringify({ out, batch_total_s: total, single_avg_s: per }, null, 1));
console.log(`Qwen: ${src.length} in batches of 10 = ${total.toFixed(2)}s, single avg = ${(per*1000).toFixed(0)}ms`);
