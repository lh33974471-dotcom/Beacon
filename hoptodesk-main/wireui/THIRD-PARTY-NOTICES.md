# Third-Party Notices

The `wireui` crate contains source code derived from the third-party project
below. Its copyright notice and license are reproduced in full, as its license
requires for redistribution.

## sciter-rs (rust-sciter)

Upstream: https://github.com/open-trade/rust-sciter
License: MIT

Portions of this crate are derived from sciter-rs so that unmodified client code
compiles and runs against this engine:

- `src/macros.rs`: the `make_args!` and `dispatch_script_call!` macros
- `src/capi/mod.rs`, `src/capi/scbehavior.rs`, `src/capi/scdef.rs`,
  `src/capi/scdom.rs`, `src/capi/sctypes.rs`: the Sciter C-API type layer

------------------------------------------------------------------------------

The MIT License (MIT)

Copyright 2019 pravic

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
