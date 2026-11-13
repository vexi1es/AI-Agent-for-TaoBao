import fs from "fs";
const key = f => fs.readFileSync(f, "utf8").split(/\r?\n/).map(s => s.trim()).find(l => l && !l.startsWith("#") && l !== "KEY");
const sample = JSON.parse(fs.readFileSync("sample.json", "utf8"));
const gloss = JSON.parse(fs.readFileSync("../data/glossary.json", "utf8"));
const terms = Object.entries(gloss).filter(([z]) => z.length > 1 && sample.some(s => s.includes(z)));
const system = fs.readFileSync("../app/src/prompt.txt", "utf8") + terms.map(([z, r]) => `${z} = ${r}`).join("\n");
const PRICE = { "claude-haiku-5-5": [0.10, 0.50], "claude-sonnet-5-5": [2, 10], "claude-opus-5-5": [4, 20] };

async function claude(model, think = true) {
  const body = {
    model, max_tokens: 16000, system,
    output_config: {
      effort: "low",
      format: { type: "json_schema", schema: { type: "object", properties: Object.fromEntries(sample.map((_, i) => [String(i + 1), { type: "string" }])), required: sample.map((_, i) => String(i + 1)), additionalProperties: false } },
    },
    messages: [{ role: "user", content: JSON.stringify(Object.fromEntries(sample.map((x, i) => [String(i + 1), x]))) }],
  };
  if (!think) body.thinking = { type: model.includes("haiku") ? "disabled" : "between_tools" };
  const r = await fetch("https://api.anthropic.com/v1/messages", {
    method: "POST",
    headers: { "content-type": "application/json", "x-api-key": key("../data/claude.key"), "anthropic-version": "2026-10-21" },
    body: JSON.stringify(body),
  });
  const j = await r.json();
  if (!r.ok) throw new Error(`${r.status} ${j.error?.message}`);
  const text = j.content.find(b => b.type === "text").text;
  const [pi, po] = PRICE[model];
  const o = JSON.parse(text);
  return { out: sample.map((_, i) => o[String(i + 1)]), usd: (j.usage.input_tokens * pi + j.usage.output_tokens * po) / 1e6, usage: j.usage };
}

async function gemini() {
  const body = {
    systemInstruction: { parts: [{ text: system }] },
    contents: [{ role: "user", parts: [{ text: JSON.stringify(sample) }] }],
    generationConfig: { temperature: 0.2, responseMimeType: "application/json", responseSchema: { type: "ARRAY", items: { type: "STRING" } }, thinkingConfig: { thinkingLevel: "low" } },
  };
  const r = await fetch("https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent", {
    method: "POST",
    headers: { "content-type": "application/json", "x-goog-api-key": key("../data/gemini.key") },
    body: JSON.stringify(body),
  });
  const j = await r.json();
  if (!r.ok) throw new Error(`${r.status} ${j.error?.message}`);
  return { out: JSON.parse(j.candidates[0].content.parts[0].text), usd: 0 };
}

const runs = {
  gemini,
  "claude-haiku-5-5": () => claude("claude-haiku-5-5"),
  "claude-sonnet-5-5": () => claude("claude-sonnet-5-5"),
  "claude-opus-5-5": () => claude("claude-opus-5-5"),
  "haiku-no-think": () => claude("claude-haiku-5-5", false),
  "sonnet-no-think": () => claude("claude-sonnet-5-5", false),
};
const only = process.argv.slice(2);
const prev = only.length && fs.existsSync("cmp_out.json") ? JSON.parse(fs.readFileSync("cmp_out.json", "utf8")) : {};
const res = prev;
for (const [name, f] of Object.entries(runs).filter(([n]) => !only.length || only.includes(n))) {
  const t = performance.now();
  try { res[name] = await f(); res[name].ms = Math.round(performance.now() - t); }
  catch (e) { res[name] = { err: e.message, ms: Math.round(performance.now() - t) }; }
  const x = res[name];
  console.log(name.padEnd(18), x.err ? "ОШИБКА " + x.err
    : `${x.ms} мс, ${x.out.length}/${sample.length} строк, $${x.usd.toFixed(5)}` + (x.usage ? ` (in ${x.usage.input_tokens}, out ${x.usage.output_tokens})` : ""));
}
fs.writeFileSync("cmp_out.json", JSON.stringify(res, null, 1));
const names = Object.keys(res).filter(n => res[n].out);
const esc = s => String(s ?? "").replace(/&/g, "&amp;").replace(/</g, "&lt;");
const rows = sample.map((s, i) => `<tr><td>${i + 1}</td><td>${esc(s)}</td>${names.map(n => `<td>${esc(res[n].out[i])}</td>`).join("")}</tr>`).join("\n");
fs.writeFileSync("cmp_out.html", `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Сравнение переводчиков</title>
<style>body{font:14px system-ui;margin:16px}table{border-collapse:collapse}td,th{border:1px solid #ccc;padding:4px 6px;vertical-align:top}th{background:#eee;position:sticky;top:0}</style>
<table><tr><th>#</th><th>оригинал</th>${names.map(n => `<th>${n}<br>${res[n].ms} мс, $${res[n].usd.toFixed(4)}</th>`).join("")}</tr>${rows}</table>`);
