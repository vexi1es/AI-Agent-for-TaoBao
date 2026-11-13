import fs from "fs";
const key = fs.readFileSync("../data/gemini.key", "utf8").split(/\r?\n/).map(s => s.trim()).find(l => l && !l.startsWith("#"));
const url = "https://generativelanguage.googleapis.com/v1beta/models/gemini-3.5-flash-lite:generateContent";
const base = () => ({ systemInstruction: { parts: [{ text: "Переведи на русский. Ответ: JSON-массив строк." }] },
  contents: [{ role: "user", parts: [{ text: JSON.stringify(["宝贝", "马丁靴男夏季薄款"]) }] }],
  generationConfig: { temperature: 0.2, responseMimeType: "application/json", responseSchema: { type: "ARRAY", items: { type: "STRING" } } } });
const variants = {
  "без thinking": b => b,
  "thinkingBudget 0": b => (b.generationConfig.thinkingConfig = { thinkingBudget: 0 }, b),
  "thinkingLevel minimal": b => (b.generationConfig.thinkingConfig = { thinkingLevel: "minimal" }, b),
  "thinkingLevel low": b => (b.generationConfig.thinkingConfig = { thinkingLevel: "low" }, b),
};
for (const [name, f] of Object.entries(variants)) {
  const t = performance.now();
  const r = await fetch(url, { method: "POST", headers: { "content-type": "application/json", "x-goog-api-key": key }, body: JSON.stringify(f(base())) });
  const j = await r.json();
  console.log(name, "→", r.status, Math.round(performance.now() - t) + "ms", r.ok ? j.candidates?.[0]?.content?.parts?.[0]?.text : j.error?.message?.slice(0, 160));
}
