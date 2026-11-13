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

  const keyOf = (s) => s.trim();

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
      if (!allInline(p)) break;
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
    tracked.add(new WeakRef(el));
    if (T.has(k)) { applyGroup(el, k); return; }
    wait(k, { deref: () => ({ group: el }) });
    request(k, isVisible(el));
  }

  function onText(n) {
    const p = n.parentElement;
    if (!p || SKIP.has(p.tagName) || p.isContentEditable || p === hintEl) return;
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
  });

  function start() {
    scan(document.body || document.documentElement);
    mo.observe(document.documentElement, { childList: true, subtree: true, characterData: true, attributes: true, attributeFilter: ATTRS });
  }

  function reset() {
    const wasOn = on;
    on = false; refreshAll();
    T.clear(); waiting.clear(); tracked.clear();
    origText = new WeakMap(); shownText = new WeakMap(); origAttr = new WeakMap(); groups = new WeakMap();
    on = wasOn;
    scan(document.body || document.documentElement);
  }

  window.__tbtr = { version: 3, toggle: () => { on = !on; refreshAll(); }, reset };
  if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start, { once: true });
  else start();
})();
