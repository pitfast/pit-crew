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
