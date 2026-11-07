# Third-party notices

## monocode

The app (`crates/app`) and its core (`crates/core`) come from monocode's native preview,
https://github.com/hardbeat920/monocode at 1de99e7b, under the MIT License:

> Copyright (c) 2026 Nick
>
> Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated
> documentation files (the "Software"), to deal in the Software without restriction, including without limitation
> the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and
> to permit persons to whom the Software is furnished to do so, subject to the following conditions:
>
> The above copyright notice and this permission notice shall be included in all copies or substantial portions
> of the Software.
>
> THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO
> THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
> AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF
> CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER
> DEALINGS IN THE SOFTWARE.

monocode's own notices (GPUI under Apache-2.0, mermaid-rs-renderer under MIT, the embedded icon sets) are in
`docs/monocode/NOTICE` and `docs/monocode/native/LICENSE-*`, and their license files sit beside the assets in
`crates/app/assets/`. Its JavaScript bundles (CodeMirror analysis, Prettier, the arcade) are gone, so none of
their code ships. monocode's GPUI snapshot is no longer vendored: the app draws with
aislopware/gpui-fast (below).

## GPUI

GPUI comes from https://github.com/aislopware/gpui-fast, a fork of longbridge/gpui-fast and Zed Industries'
GPUI, under the Apache License 2.0 (its `LICENSE`); `Cargo.toml` pins the rev.

## zeron

Onicode began as a fork of https://github.com/zeronsh/zeron (MIT, Copyright (c) 2026 Wing; see `LICENSE`).
`crates/rpc` and parts of the build tooling come from it.
