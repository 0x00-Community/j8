# j8 book

**English** · [简体中文](README.zh-CN.md)

The complete guide to [j8](https://github.com/0x00-Community/j8), built with [mdBook](https://rust-lang.github.io/mdBook/)
and served as a Cloudflare Worker (static assets). Scaffolded with `bun create cloudflare`.

| Path | |
|---|---|
| `en/` | the English book (`en/src/SUMMARY.md`), the site root |
| `zh-CN/` | the Chinese translation, served under `/zh-CN/` |
| `en/lang.js` | the language switch in the menu bar (`zh-CN/lang.js` links to it) |
| `wrangler.jsonc` | the Worker: serves `book/`, mdBook's `404.html` for unknown paths |

```sh
bun install                 # wrangler
bun run build               # mdbook build en && mdbook build zh-CN ⇒ book/
bun run serve               # live preview of the English book (serve:zh for the Chinese one)
bun run dev                 # build, then wrangler dev
bun run deploy              # build, then wrangler deploy
```

Pushes to `main` that touch `mdbook/` deploy automatically (Cloudflare Workers Builds: root `mdbook`, build `sh build.sh`, deploy `npx wrangler deploy`). `build.sh` uses `mdbook` from PATH or downloads the pinned 0.5.4 release. For a manual deploy with several Cloudflare accounts, pick one with `CLOUDFLARE_ACCOUNT_ID`.

English is the source: change a page in `en/src/` and the same page in `zh-CN/src/` together.
