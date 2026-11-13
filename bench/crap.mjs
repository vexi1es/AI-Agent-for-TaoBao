import fs from "fs";
import path from "path";

const [rcaFile, covFile, limit = "10"] = process.argv.slice(2);
const rca = fs.readFileSync(rcaFile, "utf8").replace(/^﻿/, "").split(/\r?\n/).filter(Boolean).map(l => JSON.parse(l));
const cov = JSON.parse(fs.readFileSync(covFile, "utf8")).data[0];

const regionsByFile = new Map();
for (const f of cov.functions) {
  for (const r of f.regions) {
    const [l1, c1, l2, c2, count, fileId, , kind] = r;
    if (kind !== 0) continue;
    const file = path.basename(f.filenames[fileId]);
    const key = `${l1}:${c1}:${l2}:${c2}`;
    const m = regionsByFile.get(file) || new Map();
    m.set(key, Math.max(m.get(key) || 0, count));
    regionsByFile.set(file, m);
  }
}

function functions(space, file, out, inTests) {
  const tests = inTests || (space.kind === "unit" ? false : /^tests$/.test(space.name || ""));
  if (space.kind === "function" && !tests && space.name !== "<anonymous>") {
    out.push({ file, name: space.name, start: space.start_line, end: space.end_line, cc: space.metrics.cyclomatic.sum });
  }
  for (const s of space.spaces || []) functions(s, file, out, tests);
  return out;
}

const rows = [];
for (const unit of rca) {
  const file = path.basename(unit.name);
  if (!file.endsWith(".rs")) continue;
  const regions = regionsByFile.get(file) || new Map();
  for (const fn of functions(unit, file, [], false)) {
    let total = 0, hit = 0;
    for (const [k, count] of regions) {
      const l1 = +k.split(":")[0];
      if (l1 >= fn.start && l1 <= fn.end) { total++; if (count > 0) hit++; }
    }
    const c = total ? hit / total : 0;
    const crap = fn.cc * fn.cc * Math.pow(1 - c, 3) + fn.cc;
    rows.push({ ...fn, cov: c, crap });
  }
}
rows.sort((a, b) => b.crap - a.crap);
const bad = rows.filter(r => r.crap >= +limit);
for (const r of bad) {
  console.log(`${r.crap.toFixed(1).padStart(7)}  cc ${String(r.cc).padStart(3)}  cov ${(r.cov * 100).toFixed(0).padStart(3)}%  ${r.file}:${r.start} ${r.name}`);
}
console.log(`${bad.length} of ${rows.length} functions have CRAP >= ${limit}`);
