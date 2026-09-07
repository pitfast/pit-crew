# PitFast web compatibility census

This is an engineering evidence record, not a claim that PitFast runs every
framework. The v0.12.2 frontend run used user-local Node `v24.20.0` and pnpm
`12.3.4`. The machine-readable source is
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

The static adapter was tested with real React, Vue, Angular, Svelte, Solid,
Preact, Next static export, Nuxt generate, SvelteKit adapter-static, and Astro
static output. It served `index.html`, nested CSS and JavaScript, returned MIME
types, returned 404 for missing files, and rejected `..` path segments. React,
Vue, Angular, and Svelte also booted in headless Chromium from PitFast with no
fatal console errors. It does not start Node, nginx, or a framework server at
runtime.

## Current census summary

The current JSON contains 91 explicit records. The mandatory frontend entries
are executed evidence; optional ecosystems remain explicitly marked
`UNTESTED_ENVIRONMENT` where no real fixture was built. No untested record is
counted as support.

| Area | Classification |
| --- | --- |
| React, Vue, Angular, Svelte, Solid, Preact | Real production builds and PitFast HTTP E2E pass through one `static-web` adapter; React/Vue/Angular/Svelte also pass browser boot |
| Next/Nuxt/SvelteKit/Astro static modes | Real static/export builds and PitFast HTTP E2E pass through the same generic `static-web` adapter |
| Next/Nuxt/SvelteKit/Astro SSR modes | Blocked by the current process/runtime model unless a future generic WASI server interface is implemented |
| Hono | Fetch-compatible in principle; existing generic Fetch adapter passes unknown/plain fixtures, but Hono package was not separately built in this run |
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
