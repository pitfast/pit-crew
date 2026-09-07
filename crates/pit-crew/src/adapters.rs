//! Build-time application-interface adapters.
//!
//! Adapters generate a small, deterministic bridge under `.pit/generated`.
//! They do not execute applications and they are never visible to PitBox.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use pit_artifact::ComponentWorld;
use pit_builder_core::{
    AdapterId, AdapterOutput, AdapterWorkspace, ApplicationAdapter, ApplicationInterface,
    CompatibilityReport, CompatibilityStatus, DetectionCandidate, DetectionEvidence, Language,
    ProjectInspection, ProjectModel,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const ASGI_ADAPTER_VERSION: &str = "python/asgi-v1";

#[derive(Debug, Clone)]
pub struct PythonAsgiAdapter {
    id: AdapterId,
    interface: ApplicationInterface,
}

impl PythonAsgiAdapter {
    pub fn new() -> Self {
        Self {
            id: AdapterId::new("python/asgi").expect("built-in adapter id is valid"),
            interface: ApplicationInterface::new("asgi").expect("built-in interface id is valid"),
        }
    }
}

impl Default for PythonAsgiAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationAdapter for PythonAsgiAdapter {
    fn id(&self) -> AdapterId {
        self.id.clone()
    }

    fn language(&self) -> Language {
        Language::Python
    }

    fn interface(&self) -> &ApplicationInterface {
        &self.interface
    }

    fn runtime_world(&self) -> ComponentWorld {
        ComponentWorld::WasiHttpProxy
    }

    fn detect(&self, project: &ProjectInspection) -> Option<DetectionCandidate> {
        let main = project.has_file("main.py");
        let app = project.has_file("app.py");
        let metadata = project.has_file("pyproject.toml")
            || project.has_file("requirements.txt")
            || project.has_file("setup.py");
        if !metadata || (!main && !app) {
            return None;
        }
        let entrypoint_file = if main { "main.py" } else { "app.py" };
        let entrypoint_source = std::fs::read_to_string(project.root.join(entrypoint_file)).ok()?;
        let has_asgi_shape = entrypoint_source.contains("def app")
            || entrypoint_source.contains("app =")
            || entrypoint_source.contains("async def app");
        if !has_asgi_shape {
            return None;
        }
        let mut evidence = vec![DetectionEvidence {
            source: if project.has_file("pyproject.toml") {
                "pyproject.toml".into()
            } else if project.has_file("requirements.txt") {
                "requirements.txt".into()
            } else {
                "setup.py".into()
            },
            detail: "Python project metadata".into(),
        }];
        let framework_hint = project
            .files
            .iter()
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| matches!(name, "pyproject.toml" | "requirements.txt"))
            })
            .filter_map(|path| std::fs::read_to_string(project.root.join(path)).ok())
            .find_map(|contents| {
                if contents.to_ascii_lowercase().contains("fastapi") {
                    Some("FastAPI".to_owned())
                } else if contents.to_ascii_lowercase().contains("starlette") {
                    Some("Starlette".to_owned())
                } else {
                    None
                }
            });
        if let Some(framework) = &framework_hint {
            evidence.push(DetectionEvidence {
                source: "Python dependency metadata".into(),
                detail: format!("framework hint: {framework}"),
            });
        }
        let entrypoint = if main { "main:app" } else { "app:app" };
        evidence.push(DetectionEvidence {
            source: if main { "main.py" } else { "app.py" }.into(),
            detail: format!("ASGI entrypoint candidate {entrypoint}"),
        });
        Some(DetectionCandidate {
            language: Language::Python,
            application_interface: Some(self.interface.clone()),
            entrypoint: Some(entrypoint.into()),
            framework_hint,
            confidence: 90,
            evidence,
        })
    }

    fn validate(&self, model: &ProjectModel) -> Result<CompatibilityReport> {
        if model.language != Language::Python {
            bail!("adapter '{}' requires Python", self.id);
        }
        if model.application_interface.as_ref() != Some(&self.interface) {
            bail!("adapter '{}' requires the ASGI interface", self.id);
        }
        if model.entrypoint.as_deref().unwrap_or_default().is_empty() {
            bail!("ASGI adapter requires an entrypoint such as main:app");
        }
        Ok(CompatibilityReport {
            status: CompatibilityStatus::Supported,
            messages: vec!["ASGI is bridged to wasi:http/proxy".into()],
        })
    }

    fn prepare(&self, project: &ProjectInspection, model: &ProjectModel) -> Result<AdapterOutput> {
        self.validate(model)?;
        let entrypoint = model
            .entrypoint
            .as_deref()
            .context("ASGI entrypoint is missing")?;
        let (module, attribute) = split_entrypoint(entrypoint)?;
        let root = project.root.join(".pit/generated/adapters/python-asgi");
        let template =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/python-http/wit");
        std::fs::create_dir_all(&root)?;
        let wit = root.join("wit");
        std::fs::create_dir_all(&wit)?;
        copy_tree(&template, &wit)?;
        let bridge = asgi_bridge(module, attribute);
        let bridge_path = root.join("app.py");
        std::fs::write(&bridge_path, bridge.as_bytes())?;
        let mut digest_files = Vec::new();
        collect_files(&root, &root, &mut digest_files)?;
        let digest = adapter_digest_bytes(
            ASGI_ADAPTER_VERSION,
            &self.id,
            &self.interface,
            entrypoint,
            &digest_files,
        );
        Ok(AdapterOutput {
            workspace: AdapterWorkspace {
                root: root.clone(),
                source_roots: vec![root.clone(), project.root.clone()],
                wit_path: Some(wit),
                entrypoint: "app".into(),
                adapter: self.id.clone(),
                digest,
                generated_files: digest_files.into_iter().map(|(path, _)| path).collect(),
            },
            runtime_world: self.runtime_world(),
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalAdapterManifest {
    schema: u32,
    adapter: ExternalAdapterIdentity,
    generator: ExternalGenerator,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalAdapterIdentity {
    id: String,
    language: Language,
    interface: String,
    target: String,
    version: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ExternalGenerator {
    kind: String,
    entrypoint: String,
    #[serde(default)]
    files: Vec<String>,
}

/// A local declarative adapter. It only copies explicitly listed source
/// assets into `.pit/generated`; it never executes adapter code.
#[derive(Debug, Clone)]
pub struct ExternalAdapter {
    root: PathBuf,
    manifest: ExternalAdapterManifest,
    id: AdapterId,
    interface: ApplicationInterface,
}

impl ExternalAdapter {
    pub fn load(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().canonicalize().with_context(|| {
            format!(
                "failed to access external adapter {}",
                root.as_ref().display()
            )
        })?;
        let manifest_path = root.join("adapter.toml");
        let manifest: ExternalAdapterManifest =
            toml::from_str(&std::fs::read_to_string(&manifest_path)?)
                .with_context(|| format!("invalid adapter manifest {}", manifest_path.display()))?;
        if manifest.schema != 1 {
            bail!("unsupported external adapter schema {}", manifest.schema);
        }
        let id = AdapterId::new(manifest.adapter.id.clone())?;
        let interface = ApplicationInterface::new(manifest.adapter.interface.clone())?;
        if manifest.adapter.target != "wasi:http/proxy" {
            bail!("external adapter target must be wasi:http/proxy in v0.11");
        }
        if manifest.generator.kind != "python-template" {
            bail!(
                "unsupported external adapter generator kind '{}'; expected python-template",
                manifest.generator.kind
            );
        }
        if manifest.generator.files.is_empty() {
            bail!("external adapter must list at least one generated file");
        }
        Ok(Self {
            root,
            manifest,
            id,
            interface,
        })
    }
}

impl ApplicationAdapter for ExternalAdapter {
    fn id(&self) -> AdapterId {
        self.id.clone()
    }
    fn language(&self) -> Language {
        self.manifest.adapter.language
    }
    fn interface(&self) -> &ApplicationInterface {
        &self.interface
    }
    fn runtime_world(&self) -> ComponentWorld {
        ComponentWorld::WasiHttpProxy
    }
    fn detect(&self, _project: &ProjectInspection) -> Option<DetectionCandidate> {
        None
    }
    fn validate(&self, model: &ProjectModel) -> Result<CompatibilityReport> {
        if model.language != self.language() {
            bail!(
                "adapter '{}' requires language {}",
                self.id,
                self.language()
            );
        }
        if model.application_interface.as_ref() != Some(&self.interface) {
            bail!(
                "adapter '{}' requires interface {}",
                self.id,
                self.interface
            );
        }
        Ok(CompatibilityReport {
            status: CompatibilityStatus::Supported,
            messages: vec![format!(
                "external adapter version {}",
                self.manifest.adapter.version
            )],
        })
    }
    fn prepare(&self, project: &ProjectInspection, model: &ProjectModel) -> Result<AdapterOutput> {
        self.validate(model)?;
        let generated_name = self.id.as_str().replace(['/', '\\'], "_");
        let root = project
            .root
            .join(".pit/generated/adapters/external")
            .join(generated_name);
        std::fs::create_dir_all(&root)?;
        let mut generated_files = Vec::new();
        let mut digest_inputs = Vec::new();
        for relative in &self.manifest.generator.files {
            let source = safe_join(&self.root, relative)?;
            let destination = safe_join(&root, relative)?;
            if !source.is_file() {
                bail!(
                    "external adapter asset does not exist: {}",
                    source.display()
                );
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            let bytes = std::fs::read(&source)?;
            std::fs::write(&destination, &bytes)?;
            generated_files.push(destination.clone());
            digest_inputs.push((destination, bytes));
        }
        let digest = adapter_digest_bytes(
            &format!(
                "{}:{}:{}:{}",
                self.manifest.adapter.version,
                self.manifest.adapter.language,
                self.manifest.adapter.target,
                self.manifest.generator.kind
            ),
            &self.id,
            &self.interface,
            model.entrypoint.as_deref().unwrap_or(""),
            &digest_inputs,
        );
        let wit_path = project
            .root
            .join("wit")
            .is_dir()
            .then(|| project.root.join("wit"))
            .or_else(|| {
                let template = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                    .join("../../fixtures/python-http/wit");
                template.is_dir().then_some(template)
            });
        Ok(AdapterOutput {
            workspace: AdapterWorkspace {
                root: root.clone(),
                source_roots: vec![root, project.root.clone()],
                wit_path,
                entrypoint: self.manifest.generator.entrypoint.clone(),
                adapter: self.id.clone(),
                digest,
                generated_files,
            },
            runtime_world: self.runtime_world(),
        })
    }
}

fn split_entrypoint(entrypoint: &str) -> Result<(&str, &str)> {
    let (module, attribute) = entrypoint
        .split_once(':')
        .ok_or_else(|| anyhow::anyhow!("entrypoint must use module:attribute syntax"))?;
    if module.is_empty() || attribute.is_empty() || module.contains('/') || attribute.contains('/')
    {
        bail!("entrypoint contains an invalid module or attribute");
    }
    Ok((module, attribute))
}

fn safe_join(root: &Path, relative: &str) -> Result<PathBuf> {
    let path = Path::new(relative);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        bail!("adapter asset path escapes its adapter root: {relative}");
    }
    Ok(root.join(path))
}

fn copy_tree(source: &Path, destination: &Path) -> Result<()> {
    for entry in std::fs::read_dir(source).with_context(|| {
        format!(
            "failed to read adapter asset directory {}",
            source.display()
        )
    })? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if source_path.is_dir() {
            std::fs::create_dir_all(&destination_path)?;
            copy_tree(&source_path, &destination_path)?;
        } else {
            std::fs::copy(&source_path, &destination_path)?;
        }
    }
    Ok(())
}

fn collect_files(root: &Path, current: &Path, files: &mut Vec<(PathBuf, Vec<u8>)>) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if path.is_file() {
            files.push((path.strip_prefix(root)?.to_path_buf(), std::fs::read(path)?));
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(())
}

fn adapter_digest_bytes(
    version: &str,
    id: &AdapterId,
    interface: &ApplicationInterface,
    entrypoint: &str,
    files: &[(PathBuf, Vec<u8>)],
) -> String {
    let mut hasher = Sha256::new();
    for value in [version, id.as_str(), interface.as_str(), entrypoint] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }
    for (path, bytes) in files {
        hasher.update(path.to_string_lossy().as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

fn asgi_bridge(module: &str, attribute: &str) -> String {
    format!(
        r#"# Generated by PitFast's generic ASGI adapter. Do not edit.
import asyncio
import importlib
import inspect
from componentize_py_types import Ok
from poll_loop import PollLoop, Sink, Stream
from wit.exports.wasi.http_v0_2 import IncomingHandler, incoming_handler
from wit.imports.wasi.http_v0_2.types import Fields, IncomingRequest, OutgoingResponse, ResponseOutparam

_application = getattr(importlib.import_module({module:?}), {attribute:?})

def _method(value):
    name = value.__class__.__name__.removeprefix("Method_")
    return {{"Get":"GET", "Head":"HEAD", "Post":"POST", "Put":"PUT", "Delete":"DELETE", "Connect":"CONNECT", "Options":"OPTIONS", "Trace":"TRACE", "Patch":"PATCH"}}.get(name, getattr(value, "value", "GET"))

async def _read_body(request):
    source = Stream(request.consume())
    chunks = []
    while True:
        chunk = await source.next()
        if chunk is None:
            break
        chunks.append(chunk)
    return b"".join(chunks)

async def _handle(request, response_out):
    path = request.path_with_query() or "/"
    method = _method(request.method())
    body = b"" if method in ("GET", "HEAD") else await _read_body(request)
    headers = [(name.lower().encode(), value) for name, value in request.headers().entries()]
    query = path.split("?", 1)[1] if "?" in path else ""
    scope = {{
        "type": "http", "asgi": {{"version": "3.0", "spec_version": "2.3"}},
        "http_version": "1.1", "method": method,
        "scheme": "http", "path": path.split("?", 1)[0],
        "raw_path": path.split("?", 1)[0].encode(), "query_string": query.encode(),
        "headers": headers, "server": ("pitfast", 80), "client": ("pitfast", 0),
    }}
    messages = [{{"type": "http.request", "body": body, "more_body": False}}]
    response = {{"status": 200, "headers": [], "body": bytearray()}}
    async def receive():
        if messages:
            return messages.pop(0)
        return {{"type": "http.disconnect"}}
    async def send(message):
        if message["type"] == "http.response.start":
            response["status"] = message["status"]
            response["headers"] = message.get("headers", [])
        elif message["type"] == "http.response.body":
            response["body"].extend(message.get("body", b""))
    result = _application(scope, receive, send)
    if inspect.isawaitable(result):
        await result
    out_headers = [(name.decode("latin1"), value) for name, value in response["headers"]]
    outgoing = OutgoingResponse(Fields.from_list(out_headers))
    outgoing.set_status_code(response["status"])
    response_body = outgoing.body()
    ResponseOutparam.set(response_out, Ok(outgoing))
    sink = Sink(response_body)
    await sink.send(bytes(response["body"]))
    sink.close()

@incoming_handler.guest
class Handler(IncomingHandler):
    def handle(self, request: IncomingRequest, response_out: ResponseOutparam) -> None:
        loop = PollLoop()
        asyncio.set_event_loop(loop)
        loop.run_until_complete(_handle(request, response_out))
"#
    )
}
