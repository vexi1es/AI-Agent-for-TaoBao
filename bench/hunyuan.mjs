import fs from "fs";
const [, , inp, outp, port = "8089"] = process.argv;
const src = JSON.parse(fs.readFileSync(inp, "utf8"));
async function tr(s) {
  const r = await fetch(`http://127.0.0.1:${port}/v1/chat/completions`, { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.2, top_k: 20, top_p: 0.6, repeat_penalty: 1.05, max_tokens: 256,
      messages: [{ role: "user", content: "把下面的文本翻译成俄语，不要额外解释。\n\n" + s }] }) });
  return (await r.json()).choices[0].message.content.trim();
}
await tr("删除");
const out = [], ms = []; const t0 = performance.now();
for (const s of src) { const t = performance.now(); out.push(await tr(s)); ms.push(performance.now() - t); }
const total = (performance.now() - t0) / 1000;
const shortAvg = ms.filter((_, i) => src[i].length <= 6).reduce((a, b, _, x) => a + b / x.length, 0);
const longAvg = ms.filter((_, i) => src[i].length > 20).reduce((a, b, _, x) => a + b / x.length, 0);
fs.writeFileSync(outp, JSON.stringify({ out, ms, total_s: total }, null, 1));
const cjk = out.filter(x => /[一-鿿]/.test(x)).length;
console.log(`Hunyuan: ${src.length} phrases ${total.toFixed(1)}s; short avg ${shortAvg.toFixed(0)}ms; long avg ${longAvg.toFixed(0)}ms; left Chinese in ${cjk}`);
