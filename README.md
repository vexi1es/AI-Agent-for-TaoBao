# AI-Agent for TaoBao

Taobao Desktop in Russian on your own computer. Your account, orders and returns stay in Chinese.

- Translates the app in place through Chrome DevTools Protocol: menus, product titles, cart.
- Translation chain: dictionary and cache, then Claude Haiku 5.5, Gemini as a reserve, Hunyuan-MT-7B on a local NVIDIA GPU as the last step.
- Pages with personal data (orders, addresses, chat, payment) are translated only on the local GPU.
- Russian search: type in Russian, the query becomes Chinese keywords and the search runs.
- One launcher: Cloudflare WARP on, Taobao, translator; everything off again on exit.
- Alt+T in Taobao shows the original.

## Layout

| Path | What |
|---|---|
| `app/` | Rust translator (`cargo build --release`) |
| `app/src/inject.js` | script injected into every Taobao page |
| `data/glossary.json` | fixed translations of Taobao terms |
| `bench/` | model comparisons and test scripts |

## Setup

1. Taobao Desktop, Cloudflare WARP, an NVIDIA GPU.
2. `bin/llama/` with a CUDA build of llama.cpp, `models/Hunyuan-MT-7B.Q4_K_M.gguf`.
3. API keys, one per file: `data/claude.key`, `data/gemini.key` (both optional).
4. Run `Запуск Taobao на русском.bat` (cloud only, GPU stays free; personal pages use the dictionary only) or `Запуск Taobao на русском + RTX.bat` (adds Hunyuan-MT on the GPU).
