# PitCrew

PitCrew is the PitFast source-to-Component build system. It is a build
orchestrator, not a compiler. Language adapters turn source projects into the
same validated PitFast artifact contract; PitBox never receives source-language
dispatch information.

~~~text
source project → LanguageBuilder → WASI Component → .pit artifact
~~~

Rust, Go, C, C++, JavaScript, TypeScript, and Python have passed real
`wasi:http/proxy` Component build and PitFast deployment tests in the current
development environment. C# and Java are probed as experimental adapters but
are not advertised as first-class: C# has no installed/maintained standalone
`wasm32-wasi` Component backend in the available .NET workload, and Java has
no installed JDK or validated Java-to-WASI-Component toolchain.

PitCrew probes existing toolchains; it does not silently install system
packages. Useful local adapter overrides include `PITFAST_GO`,
`PITFAST_COMPONENTIZE_GO`, `PITFAST_WASI_SDK`, `PITFAST_NODE`,
`PITFAST_TSC`, `PITFAST_COMPONENTIZE_JS_SCRIPT`, and
`PITFAST_COMPONENTIZE_PY`.

~~~bash
pit doctor languages
pit build                 # deterministic detection when unambiguous
pit build --language go   # explicit selection
~~~

`pit.toml` may set `build.language`. Detection rejects directories that contain
multiple plausible build roots unless the language is explicit.

## Artifact contract

The shared pit-artifact crate owns the versioned .pit/artifact.json contract:

~~~json
{
  "schema_version": 1,
  "artifact": {
    "name": "rust-hello",
    "path": "build/rust-hello.wasm",
    "sha256": "...",
    "size_bytes": 65151
  },
  "build": {
    "language": "go",
    "toolchain": "componentize-go",
    "toolchain_version": "...",
    "target": "wasm32-wasip2",
    "profile": "release",
    "fingerprint": "..."
  },
  "runtime": {
    "abi": "wasi-preview2",
    "entrypoint": "wasi:cli/command",
    "format": "component"
  },
  "execution": {
    "timeout_ms": null,
    "memory_bytes": null
  },
  "capabilities": ["stdio", "args", "env"]
}
~~~

The WASM path is relative to .pit. The manifest is integrity-checked before
project-managed execution. Repeated builds reuse the artifact when the
deterministic source/build fingerprint, size, and SHA-256 remain valid. Use
pit build --force to bypass the cache. User projects should normally ignore
.pit/.

## Library usage

The `pit-builder-core` crate owns `Language`, `BuildRequest`, `BuildOutput`,
`ToolchainInfo`, and the `LanguageBuilder` trait. Rust, Go, native C/C++,
JavaScript/TypeScript, Python, and experimental C#/Java adapters implement that
contract. `pit-crew` registers them and performs common fingerprint, cache,
manifest, digest, and integrity handling.

## Universal application adaptation

PitCrew separates four build-time concerns:

```text
language toolchain → application adapter → project detector → optional Kit
```

Toolchains compile source; adapters bridge an application interface to an
existing WASI contract; detectors only provide explainable convenience hints;
Kits are bundles of those build-time pieces. None of these concepts enter
PitBox or the request path. Framework names are metadata, not runtime identity.

The current built-in application-interface adapters are Python ASGI,
JavaScript/TypeScript Fetch, and Go net/http:

```toml
[build]
language = "python"
interface = "asgi"
entry = "main:app"
adapter = "python/asgi"
```

Fetch projects use `interface = "fetch"` and an entrypoint such as
`main:fetch`; Go projects use `interface = "net-http"` and an importable
`http.Handler` such as `app:Handler`. Both adapters generate a bridge below
`.pit/generated` and produce the same `wasi:http/proxy` Component. A Go
application must expose the handler from an importable package; a root
`package main` listener is rejected because it would require a service port or
an import cycle.

`pit init --dry-run` shows the evidence used for a proposed configuration.
When detection is not possible, `pit init --language python --interface asgi
--entry main:app` is sufficient; no framework detector is required. Generated
bridge files are kept below `.pit/generated` and user source is never rewritten.

Known frameworks and unknown applications that implement ASGI use the same
adapter and produce the same `wasi:http/proxy` Component contract. A local
declarative adapter can extend the build system without a PitCrew source
change:

```toml
schema = 1

[adapter]
id = "local/banana-http"
language = "python"
interface = "banana-http"
target = "wasi:http/proxy"
version = "1.0.0"

[generator]
kind = "python-template"
entrypoint = "app"
files = ["app.py"]
```

Select it with `build.adapter` in `pit.toml` or `--adapter
./pit-adapters/banana-http`. v0.11 local adapters are deliberately
declarative: they copy explicitly listed generated assets and reuse trusted
language tooling; they do not execute downloaded native plugins. Adapter
identity, version, entrypoint, and asset bytes participate in the build
fingerprint.

An already-built compatible Component is the final escape hatch:

```bash
pit build --artifact ./dist/app.wasm --abi wasi-preview2 --world wasi:http/proxy
```

It is validated and wrapped in the normal artifact manifest without language
or framework detection. The portable `.wasm` remains authoritative.

### Adapter author guide

1. Choose a stable lower-case `ApplicationInterface` identifier.
2. Declare a versioned local `adapter.toml` with language, target, and
   generator assets.
3. Generate only under `.pit/generated`; never patch user source.
4. Reuse an existing trusted language builder and emit one self-contained
   WASI Component.
5. Include adapter version/digest and generated inputs in cache identity.
6. Validate unsupported capabilities explicitly and preserve the underlying
   compiler error in verbose output.

### Detector author guide

A detector returns language, interface/entrypoint suggestions, confidence, and
evidence. It may recognize a framework dependency as a hint, but it must map
that hint to an application interface. It must not contain runtime behavior or
make a detector-known framework a compatibility prerequisite.

The v0.11 adoption fixtures have been exercised through the real PitBox HTTP
dispatcher with `componentize-py 0.25.0`: Starlette 1.6.0 and Falcon 4.3.1
both use the same `python/asgi` adapter, and the detector does not need to know
Falcon. A plain mystery ASGI application also passes with no framework hint.
FastAPI 0.141.1 was attempted; its `pydantic_core` native CPython extension is
not available inside the embedded componentize-py runtime, so that application
is reported as a toolchain incompatibility rather than advertised as supported.

### Real-application compatibility

`pit doctor` is the early compatibility boundary. It reports interface,
adapter, evidence, dependency findings, certainty, and actionable remedies;
`pit doctor --verbose` adds transitive dependency paths and package evidence,
while `pit doctor --json` emits schema version 1 for CI. A confirmed native
extension blocks `pit build`; a static capability hint such as `subprocess` is
reported as a potential issue instead of a false incompatibility.

For FastAPI 0.141.1 the observed dependency path is:

```text
fastapi → pydantic → pydantic_core
         → pydantic_core/_pydantic_core.cpython-313-x86_64-linux-gnu.so
         → cp313 manylinux wheel
```

That shared object is a CPython/Linux native extension, not a WASI-targeted
module that the current componentize-py embedded runtime can load. PitFast
does not add a FastAPI runtime or host-Python fallback; it diagnoses the
generic native-extension boundary before componentization.

The current real compatibility corpus includes Starlette, Falcon, an unknown
ASGI application, a plain Fetch handler, an unknown Fetch application, plain
Go `net/http`, and Go Chi through the same generic `go/net-http` bridge.

### Python dependency compatibility taxonomy

The Python inspector is deliberately conservative. It reads declared
dependencies and locally visible `dist-info` metadata, follows
`Requires-Dist` edges, and reports evidence rather than pretending that a
source scan is a proof of runtime compatibility:

| Dependency shape | Doctor result in the current toolchain |
| --- | --- |
| Pure-Python package | supported when its metadata is available and no other finding blocks the build |
| Package data | supported by the embedded runtime when the package is included by the componentizer; missing data is reported by the build |
| `ctypes`/`cffi` or raw sockets | potential issue unless a compatible guest capability is explicitly proven |
| CPython, PyO3, or other native extension | confirmed blocker when a native shared object is present and no WASI-targeted replacement is available |
| System shared library, subprocess, thread, or filesystem assumptions | potential or unknown finding until the package/toolchain proves the required capability |
| Platform-specific wheel | confirmed blocker when its wheel contains a host-native extension; otherwise unknown until the build resolves it |

This taxonomy is generic: `pydantic_core` is one observed example of the
native-extension category, not a FastAPI-specific rule. Native Python
extension support was not achieved in this release because the current
`componentize-py` embedded runtime has no safe loader for CPython/Linux
shared objects and no validated WASI wheel/source path was available.
The appropriate outcome is an early diagnostic, not a host-Python fallback.

The resulting artifact remains a generic WASI Preview 1 core module or WASI
Preview 2 Component (`wasi:cli/command` or `wasi:http/proxy`). Embedded JS and
Python runtimes are part of their Components; PitBox never starts Node, Python,
the JVM, or .NET for a request.

The current upstream `componentize-py` pre-initialization snapshot is not
byte-for-byte deterministic across forced rebuilds (the same behavior remains
with `PYTHONHASHSEED=0` and `--stub-wasi`). PitCrew therefore relies on the
stable source/toolchain fingerprint for normal cache reuse and records this as
a Python toolchain limitation; it does not claim forced Python rebuilds have
stable digests.

## Fixture

The fixtures directory contains real command/HTTP projects for the supported
languages, including the HTTP contract `/hello`, `/language`, `/echo`, and
`/cpu`. User projects should normally add `.pit/` to `.gitignore` because it
contains generated output.
