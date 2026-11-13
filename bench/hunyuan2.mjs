import fs from "fs";
const [, , inp, outp, port = "8089"] = process.argv;
const src = JSON.parse(fs.readFileSync(inp, "utf8"));
const G = JSON.parse(fs.readFileSync("glossary.json", "utf8"));
const terms = Object.keys(G).filter(k => k.length >= 2 && G[k]).sort((a, b) => b.length - a.length);

const RULES = [
  [/^已售\s*([\d.]+万?\+?)\s*件?$/, (_, n) => `Продано ${n.replace("万", "0 000").replace(".", "")}`],
  [/^([\d.]+)万\+$/, (_, n) => `${Math.round(parseFloat(n) * 10)} тыс.+`],
  [/^满(\d+)减(\d+)$/, (_, a, b) => `−${b} при покупке от ${a}`],
  [/^再赚(\d+)淘金币可低至([\d.]+)元$/, (_, a, b) => `Ещё ${a} Таобао-монет — цена от ¥${b}`],
  [/^官方立减(\d+)%$/, (_, a) => `Скидка Taobao ${a}%`],
  [/^逛(\d+)个商品\((\d+)\/(\d+)\)$/, (_, a, b, c) => `Посмотреть ${a} товара (${b}/${c})`],
];

function viaRules(s) {
  if (s in G) return { t: G[s] || s, how: "dict" };
  for (const [re, f] of RULES) { const m = s.match(re); if (m) return { t: f(...m), how: "rule" }; }
  return null;
}

const isTitle = s => s.length > 20 && !/证|许可|备案|：/.test(s);
async function model(s) {
  const used = []; let rest = s;
  for (const k of terms) if (rest.includes(k)) { used.push(k); rest = rest.split(k).join(" "); }
  const termBlock = used.length ? "参考下面的翻译：\n" + used.map(k => `${k} 翻译成 ${G[k]}`).join("\n") + "\n\n" : "";
  const content = isTitle(s)
    ? termBlock + "上文是参考信息。下面是淘宝商品标题，译文要像俄罗斯网店的商品名：简短，只译原文信息，不要扩写，不要添加原文没有的内容。\n参考上面的信息，把下面的文本翻译成俄语，注意不需要翻译上文，也不要额外解释：\n" + s
    : termBlock + "把下面的文本翻译成俄语，不要额外解释。\n\n" + s;
  const r = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.2, top_k: 20, top_p: 0.6, repeat_penalty: 1.05, max_tokens: 220,
      messages: [{ role: "user", content }] }) });
  return (await r.json()).choices[0].message.content.trim().replace(/\.$/, "");
}

await model("删除");
const out = [], ms = [], how = []; const t0 = performance.now();
for (const s of src) {
  const t = performance.now(); const r = viaRules(s);
  if (r) { out.push(r.t); how.push(r.how); } else { out.push(await model(s)); how.push("model"); }
  ms.push(performance.now() - t);
}
const total = (performance.now() - t0) / 1000;
fs.writeFileSync(outp, JSON.stringify({ out, ms, how, total_s: total }, null, 1));
const avg = f => { const m = ms.filter((_, i) => f(src[i])); return Math.round(m.reduce((a, b) => a + b, 0) / m.length); };
console.log(`total ${total.toFixed(0)}s; dict ${how.filter(h=>h=="dict").length}, rules ${how.filter(h=>h=="rule").length}, model ${how.filter(h=>h=="model").length}; <=6: ${avg(x=>x.length<=6)}ms; 7-20: ${avg(x=>x.length>6&&x.length<=20)}ms; >20: ${avg(x=>x.length>20)}ms; chinese left: ${out.filter(x=>/[一-鿿]/.test(x)).length}`);
