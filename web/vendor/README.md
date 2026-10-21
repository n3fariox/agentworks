These browser assets are vendored from `@xterm/xterm` **6.0.0** (MIT; see `xterm.LICENSE`) and served locally, without a CDN. The matching package and integrity are pinned in `package-lock.json` as a dev dependency; workers omit dev dependencies.

To refresh after an intentional dependency update, copy `node_modules/@xterm/xterm/lib/xterm.mjs` and `node_modules/@xterm/xterm/css/xterm.css` here, along with the package LICENSE. The Rust server embeds the assets at build time.
