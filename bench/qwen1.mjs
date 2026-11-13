import fs from "fs";
const [, , inp, outp, port="8088"] = process.argv;
const src = JSON.parse(fs.readFileSync(inp, "utf8"));
const SYS = "Переведи с китайского на русский текст из интерфейса маркетплейса Taobao. Пиши только по-русски, без китайских иероглифов. Бренды латиницей оставляй как есть. Коротко и естественно, как в русском интернет-магазине. 宝贝=товар, 包邮=бесплатная доставка, 已售=продано, 淘金币=Таобао-монеты, 亲=не переводи. Ответь только переводом.";
async function ask1(s) {
  const r = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.1, max_tokens: 200, messages: [{ role: "system", content: SYS }, { role: "user", content: s }] }) });
  return (await r.json()).choices[0].message.content.trim();
}
await ask1("删除");
const out = []; const t = performance.now();
for (const s of src) out.push(await ask1(s));
const total = (performance.now() - t) / 1000;
fs.writeFileSync(outp, JSON.stringify({ out, total_s: total, avg_ms: total * 1000 / src.length }, null, 1));
const cjk = out.filter(x => /[一-鿿]/.test(x)).length;
console.log(`${src.length} phrases one-by-one: ${total.toFixed(1)}s, avg ${(total*1000/src.length).toFixed(0)}ms, left Chinese in ${cjk}`);
