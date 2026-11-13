for (const s of ["删除", "7天无理由退货", "马丁靴男夏季薄款中高帮英伦风男士休闲内增高复古切尔西短靴子潮"]) {
  const t = performance.now();
  const r = await fetch("http://127.0.0.1:8089/v1/chat/completions", { method: "POST", headers: { "content-type": "application/json" },
    body: JSON.stringify({ temperature: 0.2, top_k: 20, top_p: 0.6, repeat_penalty: 1.05, max_tokens: 200, messages: [{ role: "user", content: "把下面的文本翻译成俄语，不要额外解释。\n\n" + s }] }) });
  const j = await r.json();
  console.log(Math.round(performance.now() - t) + "ms", s, "→", j.choices[0].message.content, "|", j.timings?.predicted_per_second?.toFixed(1), "tok/s");
}
