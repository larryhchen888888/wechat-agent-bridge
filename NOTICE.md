# Notices

This project is Copyright (c) 2026 Larry Chen, distributed under the MIT
License in [`LICENSE`](LICENSE).

It does not bundle or redistribute WeChat, GreenBubbles, or wx-cli binaries.
`moments-reader/` depends on `wx-db` and `wx-decrypt` as Cargo git
dependencies, resolved and compiled locally when you build this project;
`bridge/sync.py` expects you to build and provide the GreenBubbles CLI
yourself. The notices below cover that dependency relationship, not a
distributed copy.

## GreenBubbles

GreenBubbles (<https://github.com/bojieli/greenbubbles>) is the reference
implementation this project is built on and verified against (v0.10.0). Its
own documented path to reading Moments (`restore` / `restore-publish` /
`replica-cached-moments`) has no working manifest producer in that build
outside its test suite; `moments-reader/` reimplements just that one piece,
reusing GreenBubbles' own key-derivation dependency (`wx-db`) rather than any
original decryption code.

```text
MIT License

Copyright (c) 2026 Bojie Li

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

## wx-cli (wx-db, wx-decrypt, and related wx-* crates)

<https://github.com/pandorafuture/wx-cli>, pinned to commit
`2abe708f55bfe135539a385df856fdc58f97fc74` — the same commit GreenBubbles
v0.10.0 depends on. `moments-reader/src/main.rs` calls
`wx_db::open_readonly_connection` directly.

```text
MIT License

Copyright (c) 2026 pandorafuture

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

This project is an independent effort. It is not affiliated with, endorsed
by, or sponsored by Tencent, WeChat, GreenBubbles, or pandorafuture.
