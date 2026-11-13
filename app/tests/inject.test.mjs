import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { JSDOM } from "jsdom";

const SCRIPT = readFileSync(new URL("../src/inject.js", import.meta.url), "utf8");

function page(html, { url = "https://s.taobao.com/search?q=x", cfg = { uzsPerCny: 1795.58, itemQuietMs: 50 } } = {}) {
  const dom = new JSDOM(`<!doctype html><body>${html}</body>`, { url, runScripts: "outside-only", pretendToBeVisual: true });
  const w = dom.window;
  const sent = [];
  w.__tbtr_send = (m) => sent.push(JSON.parse(m));
  w.__tbtr_cfg = cfg;
  w.eval(SCRIPT);
  return { w, doc: w.document, sent };
}

const tick = (ms) => new Promise((r) => setTimeout(r, ms));

async function until(cond, ms = 3000) {
  const end = Date.now() + ms;
  while (!cond()) {
    if (Date.now() > end) throw new Error("timed out waiting");
    await tick(10);
  }
}

const keysOf = (sent) => sent.flatMap((m) => [...(m.hi || []), ...(m.lo || [])]);

test("chinese text is reported and replaced by the translation", async () => {
  const { w, doc, sent } = page("<div id=a>购物车</div>");
  await until(() => keysOf(sent).includes("购物车"));
  w.__tbtr_apply({ 购物车: "Корзина" });
  assert.equal(doc.getElementById("a").textContent, "Корзина");
});

test("a title split by highlight tags is translated as one piece", async () => {
  const { w, doc, sent } = page("<span id=t>马丁<span>靴</span>男</span>");
  await until(() => keysOf(sent).length > 0);
  await tick(100);
  assert.deepEqual(keysOf(sent), ["马丁靴男"]);
  w.__tbtr_apply({ 马丁靴男: "Мужские мартинсы" });
  assert.equal(doc.getElementById("t").textContent, "Мужские мартинсы");
});

test("a yuan price gets its price in soums next to it", async () => {
  const { doc } = page('<div class=p id=p><span>¥</span><span>72</span><span>.6</span></div><div id=q>¥ 1 200</div>');
  await tick(50);
  assert.match(doc.getElementById("p").parentElement.textContent, /130 400 сум/);
  assert.match(doc.body.textContent, /2 154 700 сум/);
});

test("a price is annotated once even when the page changes around it", async () => {
  const { doc } = page("<div id=p>¥22</div>");
  await tick(50);
  doc.body.appendChild(doc.createElement("div")).textContent = "新";
  await tick(50);
  assert.equal(doc.body.textContent.match(/сум/g).length, 1);
});

test("without an exchange rate no soums are shown", async () => {
  const { doc } = page("<div>¥22</div>", { cfg: {} });
  await tick(50);
  assert.doesNotMatch(doc.body.textContent, /сум/);
});

test("a product page sends its original chinese text once it settles", async () => {
  const { w, sent } = page("<h1>马丁靴男</h1><div>¥72.6</div><div>已售2000+</div>", { url: "https://item.taobao.com/item.htm?id=1" });
  w.__tbtr_apply({ 马丁靴男: "Мартинсы" });
  await until(() => sent.some((m) => m.item));
  const item = sent.find((m) => m.item);
  assert.equal(item.item.url, "https://item.taobao.com/item.htm?id=1");
  assert.match(item.item.text, /马丁靴男/);
  assert.match(item.item.text, /已售2000\+/);
  assert.doesNotMatch(item.item.text, /сум/);
});

test("a search page is not treated as a product page", async () => {
  const { sent } = page("<h1>马丁靴男</h1>");
  await tick(300);
  assert.ok(!sent.some((m) => m.item));
});

test("the verdict panel shows grade, money and the cargo ban", async () => {
  const { w, doc } = page("<h1>马丁靴男</h1>", { url: "https://detail.tmall.com/item.htm?id=2" });
  w.__tbtr_item_done({
    label: "✅ Хорошо", grade: "good", why: "98% хороших", complaints: "маломерит",
    price: "¥72.6 · 130 400 сум", weight: "~850 г (ботинки)", cargo: "≈ 112 000 сум (Top Cargo авиа, $11/кг)",
    total: "≈ 242 300 сум с доставкой", banned: "⛔ Нельзя авиа: магнит",
  });
  const p = doc.querySelector("[data-tbtr=item]");
  assert.ok(p, "panel exists");
  for (const s of ["✅ Хорошо", "98% хороших", "маломерит", "130 400 сум", "~850 г", "112 000 сум", "242 300 сум", "Нельзя авиа: магнит"]) {
    assert.ok(p.textContent.includes(s), s);
  }
  await tick(150);
});

test("russian text in the search box is sent for a chinese query instead of searching", async () => {
  const { w, doc, sent } = page('<form action="https://s.taobao.com/search"><input id=q name=q></form>');
  const q = doc.getElementById("q");
  q.value = "ботинки челси";
  const ev = new w.KeyboardEvent("keydown", { key: "Enter", bubbles: true, cancelable: true });
  q.dispatchEvent(ev);
  assert.ok(ev.defaultPrevented);
  assert.deepEqual(sent.find((m) => m.search), { search: "ботинки челси", id: 1 });
});
