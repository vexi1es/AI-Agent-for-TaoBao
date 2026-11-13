import fs from "fs";
const [model, chunk = "10", off] = process.argv.slice(2);
const key = fs.readFileSync("../data/claude.key", "utf8").split(/\r?\n/).map(s => s.trim()).find(l => l && !l.startsWith("#") && l !== "KEY");
const sample = JSON.parse(fs.readFileSync("sample.json", "utf8"));
const gloss = JSON.parse(fs.readFileSync("../data/glossary.json", "utf8"));
const PRICE = { "claude-haiku-5-5": [0.10, 0.50], "claude-sonnet-5-5": [2, 10], "claude-opus-5-5": [4, 20] };

async function one(items) {
  const terms = Object.entries(gloss).filter(([z]) => z.length > 1 && items.some(s => s.includes(z)));
  const ids = items.map((_, i) => String(i + 1));
  const body = {
    model, max_tokens: 8000,
    system: fs.readFileSync("../app/src/prompt.txt", "utf8") + terms.map(([z, r]) => `${z} = ${r}`).join("\n"),
    output_config: { effort: "low", format: { type: "json_schema", schema: { type: "object", properties: Object.fromEntries(ids.map(k => [k, { type: "string" }])), required: ids, additionalProperties: false } } },
    messages: [{ role: "user", content: JSON.stringify(Object.fromEntries(items.map((x, i) => [ids[i], x]))) }],
  };
  if (off) body.thinking = { type: model.includes("haiku") ? "disabled" : "between_tools" };
  const t = performance.now();
  const r = await fetch("https://api.anthropic.com/v1/messages", { method: "POST", headers: { "content-type": "application/json", "x-api-key": key, "anthropic-version": "2026-10-21" }, body: JSON.stringify(body) });
  const j = await r.json();
  if (!r.ok) throw new Error(`${r.status} ${j.error?.message}`);
  const [pi, po] = PRICE[model];
  return { ms: performance.now() - t, usd: (j.usage.input_tokens * pi + j.usage.output_tokens * po) / 1e6 };
}
const n = +chunk, parts = [];
for (let i = 0; i < sample.length; i += n) parts.push(sample.slice(i, i + n));
const t = performance.now();
const res = await Promise.all(parts.map(one));
const ms = res.map(r => Math.round(r.ms)).sort((a, b) => a - b);
console.log(`${model}${off ? " no-think" : ""} chunk ${n}: всё за ${Math.round(performance.now() - t)} мс; пачки ${ms[0]}..${ms.at(-1)} мс; $${res.reduce((a, r) => a + r.usd, 0).toFixed(4)}`);
