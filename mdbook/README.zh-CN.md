# j8 之书

[English](README.md) · **简体中文**

[j8](https://github.com/0x00-Community/j8) 的完整讲解，用 [mdBook](https://rust-lang.github.io/mdBook/) 写，作为 Cloudflare
Worker（静态资源）发布。骨架由 `bun create cloudflare` 生成。

| 路径 | |
|---|---|
| `en/` | 英文版（`en/src/SUMMARY.md`），站点根 |
| `zh-CN/` | 中文译本，在 `/zh-CN/` 下 |
| `en/lang.js` | 菜单栏里的语言切换（`zh-CN/lang.js` 是指向它的链接） |
| `wrangler.jsonc` | Worker：发布 `book/`，未知路径给 mdBook 的 `404.html` |

```sh
bun install                 # wrangler
bun run build               # mdbook build en && mdbook build zh-CN ⇒ book/
bun run serve               # 实时预览英文版（中文版用 serve:zh）
bun run dev                 # 编好后 wrangler dev
bun run deploy              # 编好后 wrangler deploy
```

需要 `mdbook` 0.5（`cargo install mdbook`）。有多个 Cloudflare 账号时用 `CLOUDFLARE_ACCOUNT_ID` 指定。

英文是原文：改 `en/src/` 里的某页时，同时改 `zh-CN/src/` 里的同一页。
