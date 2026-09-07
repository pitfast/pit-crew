# PitFast web compatibility census

This is an engineering evidence record, not a claim that PitFast runs every
framework. The machine-readable source is
[`web-frameworks.json`](./web-frameworks.json). A framework is counted as a
pass only when its resulting artifact was executed through PitFast/PitBox.

## Stable boundaries

| Boundary | Result |
| --- | --- |
| Browser/static output | `static-web` generic adapter; assets are embedded in one `wasi:http/proxy` Component |
| JavaScript Fetch | `javascript/fetch`; existing plain and unknown Fetch fixtures pass |
| Python ASGI | `python/asgi`; Starlette 1.6.0, Falcon 4.3.1, and unknown ASGI fixtures pass |
| Go `net/http` | `go/net-http`; plain `net/http` and Chi fixtures pass |
| Raw WASI Component | bypasses framework and toolchain detection |

The static adapter was tested with a real `dist/` fixture. It served
`index.html`, nested CSS and JavaScript, returned MIME types, returned 404 for
missing files, and rejected `..` path segments. It does not start Node, nginx,
or a framework server at runtime.

## Current census summary

The current JSON contains 91 explicit records: 5 executed passes (one static,
four generic-adapter proofs), 1 confirmed native-extension blocker, 25
language-toolchain blockers, 8 process-model blockers, 7 runtime-API blockers,
14 fixture-specific investigations still untested, and 31 environment-unavailable
records. No environment-unavailable record is counted as support.

| Area | Classification |
| --- | --- |
| React, Vue, Angular, Svelte, Solid, Preact, Qwik, Lit, Alpine, HTMX | Environment unavailable in this run: no native Node package manager to create production fixtures |
| Next/Nuxt/SvelteKit/Astro static modes | Same generic `static-web` boundary; package builds unavailable in this run |
| Next/Nuxt/SvelteKit/Astro SSR modes | Blocked by the current process/runtime model unless a future generic WASI server interface is implemented |
| Hono | Fetch-compatible in principle; real package fixture unavailable in this run |
| Express/Fastify/Nest/Koa/Adonis | Node server APIs/process model; no host-Node fallback |
| Starlette/Falcon/unknown ASGI | Pass through one generic adapter |
| FastAPI | ASGI boundary is present, but `fastapi → pydantic → pydantic_core` requires an unavailable Linux CPython native extension |
| Go `net/http`/Chi | Pass through one generic bridge |
| Gin/Echo/Fiber/Gorilla/Buffalo | Not claimed; dedicated fixtures were not available in this run |
| PHP/Ruby/JVM/.NET/Elixir/Scala/Clojure/Dart | No corresponding PitCrew source toolchain; classified at the language boundary |
| Hugo/Zola | Build executables unavailable; generated static output is the intended generic boundary |

## How to read statuses

`PASS_GENERIC_ADAPTER` and `PASS_STATIC` are executed evidence. `BLOCKED_*`
means the architecture or current toolchain provides a concrete blocker.
`ENVIRONMENT_UNAVAILABLE` means the requested ecosystem could not be installed
or built in the controlled environment. `UNTESTED_ENVIRONMENT` means a
fixture-specific investigation remains; it is not a support claim.

The census deliberately distinguishes static export from SSR. A framework can
be compatible when it produces static assets while its server mode remains
incompatible with the current WASI execution contract.
