# PitCrew

PitCrew is the PitFast source-to-WASM build system. It is a build orchestrator,
not a compiler:

Rust HTTP projects may declare the standard `wasi:http/proxy` world in
`pit.toml`; PitCrew writes that world to the generated artifact manifest and
does not execute it. Rust remains the only supported source language.

~~~text
source project → builder adapter → WASM → .pit artifact
~~~

Version 0.4 supports Rust projects with wasm32-wasip2 by default, with
wasm32-wasip1 as an explicit compatibility target. It uses
cargo metadata for project and binary detection, invokes the installed Cargo
toolchain, validates the resulting WebAssembly, and writes a SHA-256 identified
artifact plus .pit/artifact.json.

PitCrew never installs toolchains automatically. If the target is missing:

~~~bash
rustup target add wasm32-wasip2
~~~

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
    "language": "rust",
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

The pit-crew crate exposes BuildRequest, BuildArtifact, typed manifest handling,
and the BuilderAdapter registry abstraction. The Rust implementation is isolated
in pit-builder-rust; future language adapters can be added and registered
without changing PitBox or its scheduler.

## Fixture

The minimal fixtures/rust-hello project is used for integration checks. User
projects should normally add .pit/ to .gitignore because it contains generated
output.
