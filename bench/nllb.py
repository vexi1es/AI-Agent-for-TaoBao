import json, sys, time, ctranslate2, sentencepiece as spm
import os, site
for p in site.getsitepackages():
    d = os.path.join(p, "nvidia", "cublas", "bin")
    if os.path.isdir(d): os.add_dll_directory(d); os.environ["PATH"] = d + os.pathsep + os.environ["PATH"]
root = sys.argv[1]
src = json.load(open(sys.argv[2], encoding="utf-8"))
sp = spm.SentencePieceProcessor(model_file=root + "/sentencepiece.bpe.model")
tr = ctranslate2.Translator(root, device="cuda", compute_type="int8_float16")
def run(batch):
    toks = [["zho_Hans"] + sp.encode(s, out_type=str) + ["</s>"] for s in batch]
    res = tr.translate_batch(toks, target_prefix=[["rus_Cyrl"]] * len(batch), beam_size=2, max_decoding_length=200)
    return [sp.decode([t for t in r.hypotheses[0] if t != "rus_Cyrl"]) for r in res]
run(src[:4])
t = time.perf_counter(); out = run(src); total = time.perf_counter() - t
t = time.perf_counter(); single = [run([s])[0] for s in src[:20]]; per = (time.perf_counter() - t) / 20
json.dump({"out": out, "batch_total_s": total, "single_avg_s": per}, open(sys.argv[3], "w", encoding="utf-8"), ensure_ascii=False, indent=1)
print(f"NLLB: batch of {len(src)} = {total:.2f}s, single avg = {per*1000:.0f}ms")
