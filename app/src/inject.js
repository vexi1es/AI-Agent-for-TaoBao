(() => {
  if (window.__tbtr) { try { window.__tbtr.reset(); } catch (e) {} return; }
  const CJK = /[㐀-鿿]/;
  const SKIP = new Set(["SCRIPT", "STYLE", "NOSCRIPT", "TEXTAREA", "INPUT", "CODE", "PRE", "SELECT", "OPTION"]);
  const ATTRS = ["placeholder", "title", "aria-label"];

  const T = new Map();
  let origText = new WeakMap();
  let shownText = new WeakMap();
  let origAttr = new WeakMap();
  const waiting = new Map();
  const tracked = new Set();
  let hi = new Set(), lo = new Set(), timer = 0, on = true;
  const CFG = window.__tbtr_cfg || {};
  let groupOrig = new WeakMap();
  let priced = new WeakMap();

  const keyOf = (s) => s.trim();
  const own = (el) => !!(el && el.closest && el.closest("[data-tbtr]"));

  function send(obj) {
    try { window.__tbtr_send(JSON.stringify(obj)); } catch (e) {}
  }

  const PRICE = /^[¥￥]\s*(\d[\d\s,]*(?:\.\s*\d+)?)$/;

  function fmtSum(n) {
    const v = Math.max(0, Math.round(n / 100) * 100);
    return v.toString().replace(/\B(?=(\d{3})+(?!\d))/g, " ") + " сум";
  }

  function priceOf(el) {
    const m = el.textContent.replace(/\s+/g, " ").trim().match(PRICE);
    if (!m) return null;
    const v = parseFloat(m[1].replace(/[\s,]/g, ""));
    return v > 0 ? v : null;
  }

  const DIGITS = /^[\d.,\s]+$/;
  const YUAN = /^\s*[¥￥]\s*$/;

  function splitPrice(e) {
    if (!e || own(e)) return null;
    let start = e;
    while (start && DIGITS.test(start.textContent)) start = start.previousElementSibling;
    if (!start || !YUAN.test(start.textContent)) return null;
    let digits = "", last = null;
    for (let x = start.nextElementSibling; x && !own(x) && DIGITS.test(x.textContent); x = x.nextElementSibling) {
      digits += x.textContent;
      last = x;
    }
    const v = last && parseFloat(digits.replace(/[\s,]/g, ""));
    return v > 0 ? { el: last, v } : null;
  }

  function annotate(n) {
    if (!CFG.uzsPerCny) return;
    let el = null, v = null;
    for (let p = n.parentElement, i = 0; p && i < 3; p = p.parentElement, i++) {
      if (own(p) || p.textContent.length > 24) break;
      const x = priceOf(p);
      if (x != null) { el = p; v = x; break; }
    }
    if (!el) {
      const split = splitPrice(n.parentElement);
      if (!split) return;
      el = split.el; v = split.v;
    }
    const text = "· " + fmtSum(v * CFG.uzsPerCny);
    let s = priced.get(el);
    if (s && s.isConnected) { if (s.textContent !== text) s.textContent = text; return; }
    s = document.createElement("span");
    s.dataset.tbtr = "uzs";
    s.style.cssText = "margin-left:4px;font-size:11px;color:#888;font-weight:normal;white-space:nowrap";
    s.textContent = text;
    el.after(s);
    priced.set(el, s);
  }

  const ITEM = /item\.htm|detail\.tmall\.com|detail\.taobao\.com/.test(location.href) && !/s\.taobao\.com\/search/.test(location.href);
  let itemSent = 0, itemLen = 0, itemTimer = 0;

  function originalText() {
    const out = [], w = document.createTreeWalker(document.body || document.documentElement, NodeFilter.SHOW_TEXT);
    let x;
    while ((x = w.nextNode())) {
      const p = x.parentElement;
      if (!p || SKIP.has(p.tagName) || own(p)) continue;
      const v = (groupOrig.has(x) ? groupOrig.get(x) : origText.has(x) ? origText.get(x) : x.nodeValue).trim();
      if (v) out.push(v);
    }
    return out.join("\n");
  }

  function itemCheck() {
    if (!ITEM || itemSent >= 3) return;
    clearTimeout(itemTimer);
    itemTimer = setTimeout(() => {
      const t = originalText();
      if (t.length < 10 || (itemSent && t.length < itemLen * 1.5)) return;
      itemSent++;
      itemLen = t.length;
      if (itemSent === 1) itemPanel([["🔎 Оцениваю товар…", "color:#666"]]);
      send({ item: { url: location.href, text: t } });
    }, CFG.itemQuietMs || 2500);
  }

  function itemPanel(rows) {
    let p = document.querySelector("[data-tbtr=item]");
    if (!p) {
      p = document.createElement("div");
      p.dataset.tbtr = "item";
      p.style.cssText = "position:fixed;top:72px;right:16px;z-index:2147483646;width:300px;background:#fff;color:#222;" +
        "border:1px solid #ddd;border-radius:6px;padding:10px 12px;font:13px/1.45 system-ui,sans-serif;box-shadow:0 2px 8px rgba(0,0,0,.12)";
      (document.body || document.documentElement).appendChild(p);
    }
    p.replaceChildren();
    const x = document.createElement("span");
    x.textContent = "×";
    x.style.cssText = "float:right;cursor:pointer;color:#999;font-size:16px;line-height:1";
    x.onclick = () => p.remove();
    p.appendChild(x);
    for (const [text, css] of rows) {
      if (!text) continue;
      const d = document.createElement("div");
      d.textContent = text;
      if (css) d.style.cssText = css;
      p.appendChild(d);
    }
  }

  window.__tbtr_item_done = (c) => {
    if (!c) { itemPanel([["Не удалось оценить товар", "color:#666"]]); return; }
    const color = { good: "#1a7f37", ok: "#9a6b00", bad: "#c62828" }[c.grade] || "#222";
    itemPanel([
      [c.label, `font-weight:600;font-size:14px;color:${color}`],
      [c.why],
      [c.complaints && "Жалуются: " + c.complaints, "color:#666"],
      [c.banned, "color:#c62828;font-weight:600"],
      [c.price && "Цена: " + c.price],
      [c.weight && "Вес: " + c.weight],
      [c.cargo && "Карго: " + c.cargo],
      [c.total, "font-weight:600"],
    ]);
  };

  function isVisible(el) {
    if (!el || !el.getBoundingClientRect) return false;
    const r = el.getBoundingClientRect();
    return r.width > 0 && r.height > 0 && r.bottom > 0 && r.top < innerHeight && r.right > 0 && r.left < innerWidth;
  }

  function wait(k, ref) {
    let a = waiting.get(k);
    if (!a) waiting.set(k, (a = []));
    a.push(ref);
  }

  function request(k, visible) {
    if (visible) { hi.add(k); lo.delete(k); } else if (!hi.has(k)) lo.add(k);
    if (!timer) timer = setTimeout(flush, 80);
  }

  function flush() {
    timer = 0;
    if (typeof window.__tbtr_send !== "function") { timer = setTimeout(flush, 300); return; }
    if (!hi.size && !lo.size) return;
    const msg = JSON.stringify({ hi: [...hi], lo: [...lo] });
    hi = new Set(); lo = new Set();
    try { window.__tbtr_send(msg); } catch (e) {}
  }

  function applyText(n, k) {
    const o = origText.get(n);
    if (o == null) return;
    const ru = arguments.length > 2 ? arguments[2] : T.get(k);
    const nv = on ? o.replace(k, ru) : o;
    shownText.set(n, nv);
    if (n.nodeValue !== nv) n.nodeValue = nv;
  }

  function applyAttr(el, attr, k, draft) {
    const m = origAttr.get(el);
    if (!m || m[attr] == null) return;
    const nv = on ? m[attr].replace(k, draft != null ? draft : T.get(k)) : m[attr];
    m["__shown_" + attr] = nv;
    if (el.getAttribute(attr) !== nv) el.setAttribute(attr, nv);
  }

  const INLINE = new Set(["SPAN", "EM", "B", "STRONG", "I", "FONT", "MARK", "U", "SMALL"]);
  let groups = new WeakMap();

  function textNodes(el) {
    const out = [], w = document.createTreeWalker(el, NodeFilter.SHOW_TEXT);
    let x;
    while ((x = w.nextNode())) out.push(x);
    return out;
  }

  function allInline(el) {
    if (!el || SKIP.has(el.tagName) || el === document.body || el === document.documentElement) return false;
    const inner = el.getElementsByTagName("*");
    if (inner.length > 30) return false;
    for (const x of inner) if (!INLINE.has(x.tagName)) return false;
    return true;
  }

  function groupOf(n) {
    let cand = null;
    for (let p = n.parentElement, i = 0; p && i < 3; p = p.parentElement, i++) {
      if (!allInline(p) || /[¥￥]/.test(p.textContent)) break;
      if (textNodes(p).filter(t => t.nodeValue.trim()).length >= 2) cand = p;
    }
    return cand;
  }

  function applyGroup(el, k, draft) {
    const g = groups.get(el);
    if (!g || g.key !== k) return;
    const ru = draft != null ? draft : T.get(k);
    g.nodes.forEach((t, i) => {
      const nv = !on ? g.orig[i] : i === 0 ? ru : "";
      shownText.set(t, nv);
      if (t.nodeValue !== nv) t.nodeValue = nv;
    });
  }

  function onGroup(el) {
    const nodes = textNodes(el);
    const joined = nodes.map(t => t.nodeValue).join("");
    const old = groups.get(el);
    if (old && nodes.length === old.nodes.length && nodes.every((t, i) => t === old.nodes[i] && shownText.get(t) === t.nodeValue)) return;
    if (!CJK.test(joined)) return;
    const k = keyOf(joined);
    if (!k || k.length > 400) return;
    groups.set(el, { key: k, nodes, orig: nodes.map(t => t.nodeValue) });
    nodes.forEach(t => groupOrig.set(t, t.nodeValue));
    tracked.add(new WeakRef(el));
    if (T.has(k)) { applyGroup(el, k); return; }
    wait(k, { deref: () => ({ group: el }) });
    request(k, isVisible(el));
  }

  function onText(n) {
    const p = n.parentElement;
    if (!p || SKIP.has(p.tagName) || p.isContentEditable || own(p)) return;
    annotate(n);
    const v = n.nodeValue;
    if (!v || shownText.get(n) === v) return;
    const g = groupOf(n);
    if (g) { onGroup(g); return; }
    if (!CJK.test(v)) return;
    const k = keyOf(v);
    if (!k || k.length > 400) return;
    origText.set(n, v);
    const ref = new WeakRef(n);
    tracked.add(ref);
    if (T.has(k)) { applyText(n, k); return; }
    wait(k, ref);
    request(k, isVisible(p));
  }

  function onAttr(el, attr) {
    const v = el.getAttribute(attr);
    if (!v || !CJK.test(v)) return;
    let m = origAttr.get(el);
    if (m && m["__shown_" + attr] === v) return;
    if (!m) origAttr.set(el, (m = {}));
    m[attr] = v;
    const k = keyOf(v);
    const ref = new WeakRef({ el, attr });
    tracked.add(new WeakRef(el));
    if (T.has(k)) { applyAttr(el, attr, k); return; }
    wait(k, { deref: () => ({ el, attr }) });
    request(k, isVisible(el));
  }

  function scan(root) {
    if (!root) return;
    if (root.nodeType === 3) { onText(root); return; }
    if (root.nodeType !== 1 && root.nodeType !== 9 && root.nodeType !== 11) return;
    if (root.nodeType === 1) {
      if (SKIP.has(root.tagName) && root.tagName !== "INPUT" && root.tagName !== "TEXTAREA") return;
      for (const a of ATTRS) if (root.hasAttribute && root.hasAttribute(a)) onAttr(root, a);
    }
    const w = document.createTreeWalker(root, NodeFilter.SHOW_TEXT | NodeFilter.SHOW_ELEMENT);
    let x;
    while ((x = w.nextNode())) {
      if (x.nodeType === 3) onText(x);
      else for (const a of ATTRS) if (x.hasAttribute(a)) onAttr(x, a);
    }
  }

  window.__tbtr_apply = (obj) => {
    for (const k in obj) {
      T.set(k, obj[k]);
      const refs = waiting.get(k);
      if (!refs) continue;
      waiting.delete(k);
      for (const r of refs) {
        const t = r.deref();
        if (!t) continue;
        if (t.nodeType === 3) { if (origText.get(t) != null && keyOf(origText.get(t)) === k) applyText(t, k); }
        else if (t.group) applyGroup(t.group, k);
        else if (t.el) applyAttr(t.el, t.attr, k);
      }
    }
  };

  window.__tbtr_draft = (obj) => {
    for (const k in obj) {
      if (T.has(k)) continue;
      for (const r of waiting.get(k) || []) {
        const t = r.deref();
        if (!t) continue;
        if (t.nodeType === 3) { if (origText.get(t) != null && keyOf(origText.get(t)) === k) applyText(t, k, obj[k]); }
        else if (t.group) applyGroup(t.group, k, obj[k]);
        else if (t.el) applyAttr(t.el, t.attr, k, obj[k]);
      }
    }
  };

  function refreshAll() {
    for (const r of tracked) {
      const t = r.deref();
      if (!t) { tracked.delete(r); continue; }
      if (t.nodeType === 3) { const o = origText.get(t); if (o != null && T.has(keyOf(o))) applyText(t, keyOf(o)); }
      else if (t.nodeType === 1) {
        const g = groups.get(t);
        if (g && T.has(g.key)) applyGroup(t, g.key);
        const m = origAttr.get(t);
        if (m) for (const a of ATTRS) if (m[a] != null && T.has(keyOf(m[a]))) applyAttr(t, a, keyOf(m[a]));
      }
    }
  }

  addEventListener("keydown", (e) => {
    if (e.altKey && (e.code === "KeyT")) { on = !on; refreshAll(); e.preventDefault(); }
  }, true);

  const CYR = /[А-Яа-яЁё]/;
  const pendingSearch = new Map();
  let searchSeq = 0, passThrough = false, hintEl = null, hintT = 0;

  function isSearchBox(el) {
    return !!el && el.tagName === "INPUT" && el.type !== "hidden" && el.type !== "password" &&
      (el.name === "q" || el.id === "q" || el.type === "search" || /search/i.test(String(el.className)) ||
       (el.form && /search/i.test(el.form.action || "")));
  }

  function wantsRussianSearch(el) {
    return isSearchBox(el) && CYR.test(el.value) && typeof window.__tbtr_send === "function";
  }

  function hint(el, text, ms) {
    clearTimeout(hintT);
    if (!hintEl) {
      hintEl = document.createElement("div");
      hintEl.dataset.tbtr = "hint";
      hintEl.style.cssText = "position:fixed;z-index:2147483647;pointer-events:none;background:#222;color:#fff;" +
        "font:12px/1.4 system-ui,sans-serif;padding:2px 8px;border-radius:4px;opacity:.92;white-space:nowrap;" +
        "max-width:70vw;overflow:hidden;text-overflow:ellipsis";
    }
    if (!text) { hintEl.remove(); return; }
    const r = el.getBoundingClientRect();
    hintEl.textContent = text;
    (document.body || document.documentElement).appendChild(hintEl);
    const below = r.bottom + 24 < innerHeight;
    hintEl.style.top = (below ? r.bottom + 4 : r.top + Math.max(0, (r.height - 20) / 2)) + "px";
    hintEl.style.left = (below ? r.left : Math.max(r.left, r.right - hintEl.offsetWidth - 8)) + "px";
    if (ms) hintT = setTimeout(() => hintEl.remove(), ms);
  }

  function setValue(el, v) {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value").set.call(el, v);
    el.dispatchEvent(new Event("input", { bubbles: true }));
    el.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function submit(p) {
    passThrough = true;
    try {
      if (p.btn && p.btn.isConnected) { p.btn.click(); return; }
      const o = { key: "Enter", code: "Enter", keyCode: 13, which: 13, bubbles: true, cancelable: true };
      const free = p.el.dispatchEvent(new KeyboardEvent("keydown", o));
      p.el.dispatchEvent(new KeyboardEvent("keypress", o));
      p.el.dispatchEvent(new KeyboardEvent("keyup", o));
      if (free && p.el.form) { if (p.el.form.requestSubmit) p.el.form.requestSubmit(); else p.el.form.submit(); }
    } finally { passThrough = false; }
  }

  function startSearch(el, btn) {
    const id = ++searchSeq;
    pendingSearch.set(id, { el, btn });
    hint(el, "перевожу запрос…");
    try { window.__tbtr_send(JSON.stringify({ search: el.value.trim(), id })); } catch (e) {}
    setTimeout(() => { if (pendingSearch.has(id)) window.__tbtr_search_done(id, null, null); }, 15000);
  }

  window.__tbtr_search_done = (id, zh, meaning) => {
    const p = pendingSearch.get(id);
    if (!p) return;
    pendingSearch.delete(id);
    if (zh) { setValue(p.el, zh); hint(p.el, "ищу: " + zh + (meaning ? " — " + meaning : ""), 6000); }
    else hint(p.el, "");
    submit(p);
  };

  for (const type of ["keydown", "keypress", "keyup"]) {
    addEventListener(type, (e) => {
      if (passThrough || e.key !== "Enter" || e.isComposing || !wantsRussianSearch(e.target)) return;
      e.preventDefault(); e.stopImmediatePropagation();
      if (type === "keydown") startSearch(e.target, null);
    }, true);
  }

  const BTN = /search|btn|button|submit/i;
  for (const type of ["mousedown", "click"]) {
    addEventListener(type, (e) => {
      if (passThrough || !(e.target instanceof Element) || e.target.tagName === "INPUT" && e.target.type !== "submit") return;
      let btn = null;
      for (let x = e.target, i = 0; x && i < 4; x = x.parentElement, i++) {
        const label = (x.textContent || x.value || "").trim();
        if (x.tagName === "BUTTON" || x.type === "submit" || BTN.test(String(x.className)) || /^(搜索|Поиск|Найти)$/.test(label)) { btn = x; break; }
      }
      if (!btn) return;
      let box = null;
      for (let x = btn.parentElement, i = 0; x && i < 5 && !box; x = x.parentElement, i++) {
        box = [...x.querySelectorAll("input")].find(wantsRussianSearch) || null;
      }
      if (!box) return;
      e.preventDefault(); e.stopImmediatePropagation();
      if (type === "click") startSearch(box, btn);
    }, true);
  }

  let scrollT = 0;
  addEventListener("scroll", () => {
    clearTimeout(scrollT);
    scrollT = setTimeout(() => {
      for (const [k, refs] of waiting) {
        for (const r of refs) {
          const t = r.deref && r.deref();
          const el = t && (t.nodeType === 3 ? t.parentElement : t.el);
          if (el && isVisible(el)) { request(k, true); break; }
        }
      }
    }, 150);
  }, true);

  const mo = new MutationObserver((list) => {
    for (const m of list) {
      if (m.type === "characterData") onText(m.target);
      else if (m.type === "attributes") onAttr(m.target, m.attributeName);
      else for (const n of m.addedNodes) scan(n);
    }
    itemCheck();
  });

  function start() {
    scan(document.body || document.documentElement);
    mo.observe(document.documentElement, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS });
    itemCheck();
  }

  function reset() {
    const wasOn = on;
    on = false; refreshAll();
    T.clear(); waiting.clear(); tracked.clear();
    origText = new WeakMap(); shownText = new WeakMap(); origAttr = new WeakMap(); groups = new WeakMap();
    groupOrig = new WeakMap(); priced = new WeakMap();
    document.querySelectorAll("[data-tbtr=uzs]").forEach(s => s.remove());
    on = wasOn;
    scan(document.body || document.documentElement);
  }

  window.__tbtr = { version: 3, toggle: () => { on = !on; refreshAll(); }, reset };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
