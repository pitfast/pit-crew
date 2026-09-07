//! Build-time application-interface adapters.
//!
//! Adapters generate a small, deterministic bridge under `.pit/generated`.
//! They do not execute applications and they are never visible to PitBox.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use pit_artifact::ComponentWorld;
use pit_builder_core::{
    AdapterId, AdapterOutput, AdapterWorkspace, ApplicationAdapter, ApplicationInterface,
    CompatibilityCertainty, CompatibilityFinding, CompatibilityReport, CompatibilitySeverity,
    CompatibilityStatus, DetectionCandidate, DetectionEvidence, Language, ProjectInspection,
    ProjectModel,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

const ASGI_ADAPTER_VERSION: &str = "python/asgi-v1";
const FETCH_ADAPTER_VERSION: &str = "javascript-fetch-v1";
const GO_NET_HTTP_ADAPTER_VERSION: &str = "go/net-http-v1";

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

#[derive(Debug, Clone)]
pub struct JavaScriptFetchAdapter {
    language: Language,
    id: AdapterId,
    interface: ApplicationInterface,
}

impl JavaScriptFetchAdapter {
    pub fn new(language: Language) -> Self {
        assert!(matches!(
            language,
            Language::JavaScript | Language::TypeScript
        ));
        Self {
            language,
            id: AdapterId::new("javascript/fetch").expect("built-in adapter id is valid"),
            interface: ApplicationInterface::new("fetch").expect("built-in interface id is valid"),
        }
    }
}

impl ApplicationAdapter for JavaScriptFetchAdapter {
    fn id(&self) -> AdapterId {
        self.id.clone()
    }

    fn language(&self) -> Language {
        self.language
    }

    fn interface(&self) -> &ApplicationInterface {
        &self.interface
    }

    fn runtime_world(&self) -> ComponentWorld {
        ComponentWorld::WasiHttpProxy
    }

    fn detect(&self, project: &ProjectInspection) -> Option<DetectionCandidate> {
        let source_name = if self.language == Language::TypeScript {
            "main.ts"
        } else {
            "main.js"
        };
        if !project.has_file("package.json") || !project.has_file(source_name) {
            return None;
        }
        let source = std::fs::read_to_string(project.root.join(source_name)).ok()?;
        if !source.contains("fetch") && !source.contains("addEventListener") {
            return None;
        }
        Some(DetectionCandidate {
            language: self.language,
            application_interface: Some(self.interface.clone()),
            entrypoint: Some("main:fetch".into()),
            framework_hint: None,
            confidence: 70,
            evidence: vec![DetectionEvidence {
                source: source_name.into(),
                detail: "Fetch-style handler shape".into(),
            }],
        })
    }

    fn validate(&self, model: &ProjectModel) -> Result<CompatibilityReport> {
        if model.language != self.language {
            bail!("adapter '{}' requires {}", self.id, self.language);
        }
        if model.application_interface.as_ref() != Some(&self.interface) {
            bail!("adapter '{}' requires the Fetch interface", self.id);
        }
        if model.entrypoint.as_deref().unwrap_or_default().is_empty() {
            bail!("Fetch adapter requires an entrypoint such as main:fetch");
        }
        Ok(CompatibilityReport::supported(
            "Fetch handlers are bridged through ComponentizeJS Web Fetch semantics to wasi:http/proxy",
        ))
    }

    fn prepare(&self, project: &ProjectInspection, model: &ProjectModel) -> Result<AdapterOutput> {
        self.validate(model)?;
        let (module, attribute) = split_entrypoint(
            model
                .entrypoint
                .as_deref()
                .context("Fetch entrypoint is missing")?,
        )?;
        let root = project
            .root
            .join(".pit/generated/adapters/javascript-fetch");
        std::fs::create_dir_all(&root)?;
        let source_file = if self.language == Language::TypeScript && module == "main" {
            "../../main.js".to_owned()
        } else {
            format!("../../../../{module}.js")
        };
        let bridge = fetch_bridge(module, attribute, &source_file);
        let bridge_path = root.join("main.js");
        std::fs::write(&bridge_path, bridge.as_bytes())?;
        let mut digest_files = Vec::new();
        collect_files(&root, &root, &mut digest_files)?;
        let digest = adapter_digest_bytes(
            FETCH_ADAPTER_VERSION,
            &self.id,
            &self.interface,
            &format!("{module}:{attribute}"),
            &digest_files,
        );
        Ok(AdapterOutput {
            workspace: AdapterWorkspace {
                root: root.clone(),
                source_roots: vec![root.clone(), project.root.clone()],
                wit_path: None,
                entrypoint: "main.js".into(),
                adapter: self.id.clone(),
                digest,
                generated_files: digest_files
                    .into_iter()
                    .map(|(path, _)| root.join(path))
                    .collect(),
            },
            runtime_world: self.runtime_world(),
        })
    }
}

#[derive(Debug, Clone)]
pub struct GoNetHttpAdapter {
    id: AdapterId,
    interface: ApplicationInterface,
}

impl GoNetHttpAdapter {
    pub fn new() -> Self {
        Self {
            id: AdapterId::new("go/net-http").expect("built-in adapter id is valid"),
            interface: ApplicationInterface::new("net-http")
                .expect("built-in interface id is valid"),
        }
    }
}

impl Default for GoNetHttpAdapter {
    fn default() -> Self {
        Self::new()
    }
}

impl ApplicationAdapter for GoNetHttpAdapter {
    fn id(&self) -> AdapterId {
        self.id.clone()
    }

    fn language(&self) -> Language {
        Language::Go
    }

    fn interface(&self) -> &ApplicationInterface {
        &self.interface
    }

    fn runtime_world(&self) -> ComponentWorld {
        ComponentWorld::WasiHttpProxy
    }

    fn detect(&self, project: &ProjectInspection) -> Option<DetectionCandidate> {
        if !project.has_file("go.mod") {
            return None;
        }
        let has_http_shape = project
            .files
            .iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "go"))
            .any(|path| {
                std::fs::read_to_string(project.root.join(path))
                    .map(|source| source.contains("http.Handler") || source.contains("ServeHTTP"))
                    .unwrap_or(false)
            });
        has_http_shape.then(|| DetectionCandidate {
            language: Language::Go,
            application_interface: Some(self.interface.clone()),
            entrypoint: Some("app:Handler".into()),
            framework_hint: None,
            confidence: 65,
            evidence: vec![DetectionEvidence {
                source: "Go source inspection".into(),
                detail: "net/http Handler shape".into(),
            }],
        })
    }

    fn validate(&self, model: &ProjectModel) -> Result<CompatibilityReport> {
        if model.language != Language::Go {
            bail!("adapter '{}' requires Go", self.id);
        }
        if model.application_interface.as_ref() != Some(&self.interface) {
            bail!("adapter '{}' requires the net/http interface", self.id);
        }
        let entrypoint = model.entrypoint.as_deref().unwrap_or_default();
        if !entrypoint.contains(':') {
            bail!("Go net/http adapter requires an entrypoint such as app:Handler");
        }
        let (package, _) = split_entrypoint(entrypoint)?;
        if package == "main" || package == "." {
            bail!(
                "Go net/http adapter requires the Handler in an importable package (for example app:Handler); a root package main would create an import cycle"
            );
        }
        Ok(CompatibilityReport::supported(
            "Go http.Handler is bridged to wasi:http/proxy without a service listener",
        ))
    }

    fn prepare(&self, project: &ProjectInspection, model: &ProjectModel) -> Result<AdapterOutput> {
        self.validate(model)?;
        let (package, symbol) = split_entrypoint(
            model
                .entrypoint
                .as_deref()
                .context("Go net/http entrypoint is missing")?,
        )?;
        if !project.root.join("go.mod").is_file() {
            bail!("go.mod must exist for the net/http adapter");
        }
        // componentize-go generates bindings in a staging module named
        // `wit_component`; the bridge follows that generated module identity
        // instead of importing the user's original module from the network.
        let import_path = format!("wit_component/{package}");
        let root = project.root.join(".pit/generated/adapters/go-net-http");
        let package_dir = root.join("export_wasi_http_incoming_handler");
        std::fs::create_dir_all(&package_dir)?;
        let bridge = go_net_http_bridge("wit_component", &import_path, symbol);
        let bridge_path = package_dir.join("handler.go");
        std::fs::write(&bridge_path, bridge.as_bytes())?;
        let mut digest_files = Vec::new();
        collect_files(&root, &root, &mut digest_files)?;
        let digest = adapter_digest_bytes(
            GO_NET_HTTP_ADAPTER_VERSION,
            &self.id,
            &self.interface,
            model.entrypoint.as_deref().unwrap_or_default(),
            &digest_files,
        );
        Ok(AdapterOutput {
            workspace: AdapterWorkspace {
                root: root.clone(),
                source_roots: vec![root.clone(), project.root.clone()],
                wit_path: None,
                entrypoint: "main".into(),
                adapter: self.id.clone(),
                digest,
                generated_files: digest_files
                    .into_iter()
                    .map(|(path, _)| root.join(path))
                    .collect(),
            },
            runtime_world: self.runtime_world(),
        })
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
            findings: Vec::new(),
        })
    }

    fn inspect_compatibility(
        &self,
        project: &ProjectInspection,
        model: &ProjectModel,
    ) -> Result<CompatibilityReport> {
        let mut report = self.validate(model)?;
        let dependency_report = inspect_python_dependencies(project);
        report.status = dependency_report.status;
        report.messages.extend(dependency_report.messages);
        report.findings.extend(dependency_report.findings);
        Ok(report)
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
            findings: Vec::new(),
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
    if module.is_empty()
        || attribute.is_empty()
        || module.starts_with('.')
        || module.contains('\\')
        || module
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
        || attribute.contains('/')
    {
        bail!("entrypoint contains an invalid module or attribute");
    }
    Ok((module, attribute))
}

fn fetch_bridge(_module: &str, attribute: &str, source_file: &str) -> String {
    format!(
        r#"import {{ {attribute} as handler }} from "{source_file}";
import {{ IncomingBody, OutgoingBody, OutgoingResponse, Fields, ResponseOutparam }} from 'wasi:http/types@0.2.0';

const encoder = new TextEncoder();

function methodName(method) {{
  return method.tag === 'other' ? method.val : method.tag.toUpperCase();
}}

function readBody(request) {{
  const body = request.consume();
  const input = body.stream();
  const pollable = input.subscribe();
  const chunks = [];
  while (true) {{
    if (!pollable.ready()) pollable.block();
    try {{
      const chunk = input.read(65536n);
      if (chunk.length === 0) break;
      chunks.push(chunk);
    }} catch (error) {{
      if (error?.payload?.tag === 'closed') break;
      throw error;
    }}
  }}
  pollable[Symbol.dispose]?.();
  input[Symbol.dispose]?.();
  IncomingBody.finish(body);
  const total = chunks.reduce((size, chunk) => size + chunk.length, 0);
  const result = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {{ result.set(chunk, offset); offset += chunk.length; }}
  return result;
}}

export const incomingHandler = {{
  async handle(request, responseOutparam) {{
    const path = request.pathWithQuery() ?? '/';
    const headers = new Headers();
    const requestHeaders = request.headers();
    for (const [name, values] of requestHeaders.entries()) {{
      headers.append(name, new TextDecoder().decode(values));
    }}
    requestHeaders[Symbol.dispose]?.();
    const init = {{ method: methodName(request.method()), headers }};
    if (init.method !== 'GET' && init.method !== 'HEAD') init.body = readBody(request);
    const authority = request.authority() ?? 'pitfast.local';
    const response = await handler(new Request(`http://${{authority}}${{path}}`, init));
    const responseFields = [];
    for (const [name, value] of response.headers) {{
      responseFields.push([name.toString(), encoder.encode(value)]);
    }}
    const outgoing = new OutgoingResponse(Fields.fromList(responseFields));
    outgoing.setStatusCode(response.status);
    const body = outgoing.body();
    const stream = body.write();
    stream.blockingWriteAndFlush(new Uint8Array(await response.arrayBuffer()));
    stream[Symbol.dispose]();
    OutgoingBody.finish(body, undefined);
    ResponseOutparam.set(responseOutparam, {{ tag: 'ok', val: outgoing }});
  }},
}};
"#
    )
}

fn go_net_http_bridge(module: &str, import_path: &str, symbol: &str) -> String {
    format!(
        r#"package export_wasi_http_incoming_handler

import (
    "bytes"
    "net/http"
    "net/http/httptest"
    "strings"

    wit_types "go.bytecodealliance.org/pkg/wit/types"
    user "{import_path}"
    . "{module}/wasi_http_types"
)

var pitfastHandler http.Handler = user.{symbol}

func Handle(request *IncomingRequest, out *ResponseOutparam) {{
    path := request.PathWithQuery().SomeOr("/")
    method := methodName(request.Method())
    body := readBody(request)
    incoming, err := http.NewRequest(method, "http://pitfast.local"+path, bytes.NewReader(body))
    if err != nil {{
        writeError(out, http.StatusBadRequest, err.Error())
        return
    }}
    headers := request.Headers()
    for _, header := range headers.Entries() {{
        incoming.Header.Add(header.F0, string(header.F1))
    }}
    headers.Drop()
    recorder := httptest.NewRecorder()
    pitfastHandler.ServeHTTP(recorder, incoming)

    response := MakeOutgoingResponse(MakeFields())
    response.SetStatusCode(uint16(recorder.Code))
    responseHeaders := response.Headers()
    for name, values := range recorder.Header() {{
        for _, value := range values {{
            responseHeaders.Append(name, []byte(value))
        }}
    }}
    responseHeaders.Drop()
    bodyResult := response.Body()
    ResponseOutparamSet(out, wit_types.Ok[*OutgoingResponse, ErrorCode](response))
    if bodyResult.IsErr() {{
        return
    }}
    outputResult := bodyResult.Ok().Write()
    if outputResult.IsErr() {{
        OutgoingBodyFinish(bodyResult.Ok(), wit_types.None[*Fields]())
        return
    }}
    output := outputResult.Ok()
    output.BlockingWriteAndFlush(recorder.Body.Bytes())
    output.Drop()
    OutgoingBodyFinish(bodyResult.Ok(), wit_types.None[*Fields]())
}}

func methodName(method Method) string {{
    switch method.Tag() {{
    case MethodGet: return "GET"
    case MethodHead: return "HEAD"
    case MethodPost: return "POST"
    case MethodPut: return "PUT"
    case MethodDelete: return "DELETE"
    case MethodConnect: return "CONNECT"
    case MethodOptions: return "OPTIONS"
    case MethodTrace: return "TRACE"
    case MethodPatch: return "PATCH"
    case MethodOther: return method.Other()
    default: return "GET"
    }}
}}

func readBody(request *IncomingRequest) []byte {{
    body := request.Consume()
    if body.IsErr() {{ return nil }}
    stream := body.Ok().Stream()
    if stream.IsErr() {{ return nil }}
    var result []byte
    for {{
        chunk := stream.Ok().BlockingRead(65536)
        if chunk.IsErr() || len(chunk.Ok()) == 0 {{ break }}
        result = append(result, chunk.Ok()...)
    }}
    stream.Ok().Drop()
    trailers := IncomingBodyFinish(body.Ok())
    pollable := trailers.Subscribe()
    pollable.Block()
    pollable.Drop()
    trailers.Get()
    return result
}}

func writeError(out *ResponseOutparam, status int, message string) {{
    response := MakeOutgoingResponse(MakeFields())
    response.SetStatusCode(uint16(status))
    body := response.Body()
    ResponseOutparamSet(out, wit_types.Ok[*OutgoingResponse, ErrorCode](response))
    if body.IsErr() {{ return }}
    stream := body.Ok().Write()
    if stream.IsOk() {{
        stream.Ok().BlockingWriteAndFlush([]byte(strings.TrimSpace(message)))
        stream.Ok().Drop()
    }}
    OutgoingBodyFinish(body.Ok(), wit_types.None[*Fields]())
}}

"#
    )
}

#[derive(Debug, Clone)]
struct PythonDistribution {
    name: String,
    root: PathBuf,
    metadata_dir: PathBuf,
    requires: Vec<String>,
    wheel_tags: Vec<String>,
}

/// Inspect the package metadata that componentize-py can actually see. This
/// is intentionally a conservative metadata reader, not a second pip: it
/// reports confirmed native files and unknown resolution separately instead of
/// pretending that a source-level heuristic proves compatibility.
fn inspect_python_dependencies(project: &ProjectInspection) -> CompatibilityReport {
    let direct = python_direct_dependencies(project);
    let mut report = CompatibilityReport {
        status: CompatibilityStatus::Supported,
        messages: Vec::new(),
        findings: Vec::new(),
    };
    if direct.is_empty() {
        report
            .messages
            .push("no declared Python dependencies to inspect".into());
    }

    let distributions = python_distributions(project);
    let mut by_name = std::collections::BTreeMap::new();
    for distribution in distributions {
        by_name.insert(
            normalize_distribution_name(&distribution.name),
            distribution,
        );
    }

    let mut queue = direct
        .into_iter()
        .map(|name| (normalize_distribution_name(&name), vec![name]))
        .collect::<std::collections::VecDeque<_>>();
    let mut visited = std::collections::BTreeSet::new();
    while let Some((name, path)) = queue.pop_front() {
        if !visited.insert(name.clone()) {
            continue;
        }
        let Some(distribution) = by_name.get(&name) else {
            report.findings.push(CompatibilityFinding {
                category: "dependency-resolution".into(),
                severity: CompatibilitySeverity::Warning,
                certainty: CompatibilityCertainty::Unknown,
                package: Some(name.clone()),
                dependency_path: path.clone(),
                evidence: vec![DetectionEvidence {
                    source: "Python project metadata".into(),
                    detail: "no installed dist-info metadata was visible to PitCrew".into(),
                }],
                reason: format!("dependency '{name}' could not be inspected locally"),
                recommendation:
                    "run the build in the same environment used by componentize-py or provide a lock/materialization".into(),
                blocks_build: false,
            });
            continue;
        };

        if let Some(native) = native_extension_evidence(distribution) {
            let mut evidence = vec![DetectionEvidence {
                source: native,
                detail: format!(
                    "{} contains a native extension; wheel tags: {}",
                    distribution.name,
                    if distribution.wheel_tags.is_empty() {
                        "unknown".into()
                    } else {
                        distribution.wheel_tags.join(", ")
                    }
                ),
            }];
            if distribution.name.eq_ignore_ascii_case("pydantic_core") {
                evidence.push(DetectionEvidence {
                    source: "pydantic_core package metadata".into(),
                    detail: "maturin wheel targets CPython/Linux rather than the embedded WASI Python runtime".into(),
                });
            }
            report.findings.push(CompatibilityFinding {
                category: "native-extension".into(),
                severity: CompatibilitySeverity::Error,
                certainty: CompatibilityCertainty::Confirmed,
                package: Some(distribution.name.clone()),
                dependency_path: path.clone(),
                evidence,
                reason: format!(
                    "{} loads a CPython/native shared object that the current componentize-py WASI runtime cannot load",
                    distribution.name
                ),
                recommendation: "use a pure-Python dependency or a wheel/source build explicitly targeting the current WASI Python toolchain".into(),
                blocks_build: true,
            });
        }

        for dependency in &distribution.requires {
            let mut dependency_path = path.clone();
            dependency_path.push(dependency.clone());
            queue.push_back((normalize_distribution_name(dependency), dependency_path));
        }
    }

    for (category, token, recommendation) in [
        (
            "subprocess",
            "subprocess",
            "replace host process execution with a guest-native API; host exec is unavailable",
        ),
        (
            "raw-socket",
            "socket",
            "use WASI HTTP or an explicitly configured resource capability",
        ),
        (
            "dynamic-loading",
            "ctypes",
            "verify that the dependency does not require host shared libraries",
        ),
        (
            "dynamic-loading",
            "cffi",
            "verify that the dependency does not require host shared libraries",
        ),
    ] {
        if project_python_source_contains(project, token) {
            report.findings.push(CompatibilityFinding {
                category: category.into(),
                severity: CompatibilitySeverity::Warning,
                certainty: CompatibilityCertainty::Potential,
                package: None,
                dependency_path: Vec::new(),
                evidence: vec![DetectionEvidence {
                    source: "Python source inspection".into(),
                    detail: format!("found '{token}' in project source"),
                }],
                reason: format!(
                    "project may rely on {token} capabilities outside the default guest contract"
                ),
                recommendation: recommendation.into(),
                blocks_build: false,
            });
        }
    }

    if report.findings.iter().any(|finding| finding.blocks_build) {
        report.status = CompatibilityStatus::Unsupported;
        report
            .messages
            .push("one or more dependencies are confirmed incompatible with the current WASI Python target".into());
    } else if report
        .findings
        .iter()
        .any(|finding| finding.certainty != CompatibilityCertainty::Confirmed)
    {
        report.status = CompatibilityStatus::PotentiallyUnsupported;
        report
            .messages
            .push("dependency compatibility has potential or unresolved findings".into());
    } else {
        report
            .messages
            .push("declared Python dependencies have no confirmed native incompatibility".into());
    }
    report
}

fn python_direct_dependencies(project: &ProjectInspection) -> Vec<String> {
    let mut dependencies = Vec::new();
    if let Ok(contents) = std::fs::read_to_string(project.root.join("requirements.txt")) {
        for line in contents.lines() {
            if let Some(name) = parse_dependency_token(line) {
                dependencies.push(name);
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string(project.root.join("pyproject.toml")) {
        let mut in_dependencies = false;
        for line in contents.lines() {
            let trimmed = line.trim();
            if trimmed.contains("dependencies") && trimmed.contains('[') {
                in_dependencies = true;
            }
            if in_dependencies {
                let content = trimmed.strip_prefix("dependencies =").unwrap_or(trimmed);
                for item in content.split(',') {
                    if let Some(name) = parse_dependency_token(item) {
                        dependencies.push(name);
                    }
                }
                if trimmed.contains(']') {
                    in_dependencies = false;
                }
            }
        }
    }
    if let Ok(contents) = std::fs::read_to_string(project.root.join("setup.py")) {
        for line in contents
            .lines()
            .filter(|line| line.contains("install_requires"))
        {
            for item in line.split([',', '[', ']']) {
                if let Some(name) = parse_dependency_token(item) {
                    dependencies.push(name);
                }
            }
        }
    }
    dependencies.sort_by_key(|value| normalize_distribution_name(value));
    dependencies.dedup_by_key(|value| normalize_distribution_name(value));
    dependencies
}

fn python_distributions(project: &ProjectInspection) -> Vec<PythonDistribution> {
    let mut roots = Vec::new();
    // Prefer project-local environments so `pit doctor` works from a cloned
    // repository without requiring the developer to activate its venv.
    roots.push(project.root.join(".venv").join("lib"));
    roots.push(project.root.join("venv").join("lib"));
    if let Ok(virtual_env) = std::env::var("VIRTUAL_ENV") {
        roots.push(PathBuf::from(virtual_env).join("lib"));
    }
    if let Ok(path) = std::env::var("PITFAST_PYTHON_SITE_PACKAGES") {
        roots.push(PathBuf::from(path));
    }
    let mut site_packages = Vec::new();
    for root in roots {
        collect_named_dirs(&root, "site-packages", &mut site_packages, 4);
    }
    let mut result = Vec::new();
    for site in site_packages {
        let Ok(entries) = std::fs::read_dir(site) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let is_dist_info = path.is_dir()
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| {
                        name.ends_with(".dist-info") || name.ends_with(".egg-info")
                    });
            if !is_dist_info {
                continue;
            }
            let metadata = path.join("METADATA");
            let Ok(contents) = std::fs::read_to_string(&metadata) else {
                continue;
            };
            let name = metadata_field(&contents, "Name").unwrap_or_else(|| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("unknown")
                    .trim_end_matches(".dist-info")
                    .to_owned()
            });
            let requires = contents
                .lines()
                .filter_map(|line| line.strip_prefix("Requires-Dist:"))
                .filter_map(|line| {
                    if line.contains("extra ==") {
                        return None;
                    }
                    let value = line.trim().split(';').next().unwrap_or("");
                    let value = value.split(['<', '>', '=', '!', '~', '[']).next()?.trim();
                    (!value.is_empty()).then_some(value.to_owned())
                })
                .collect();
            let wheel_tags = std::fs::read_to_string(path.join("WHEEL"))
                .ok()
                .into_iter()
                .flat_map(|contents| {
                    contents
                        .lines()
                        .filter_map(|line| {
                            line.strip_prefix("Tag:").map(|tag| tag.trim().to_owned())
                        })
                        .collect::<Vec<_>>()
                })
                .collect();
            result.push(PythonDistribution {
                name,
                root: path.parent().unwrap_or(&path).to_path_buf(),
                metadata_dir: path.clone(),
                requires,
                wheel_tags,
            });
        }
    }
    result
}

fn collect_named_dirs(root: &Path, name: &str, result: &mut Vec<PathBuf>, depth: usize) {
    if depth == 0 || !root.is_dir() {
        return;
    }
    if root.file_name().and_then(|value| value.to_str()) == Some(name) {
        result.push(root.to_path_buf());
        return;
    }
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.path().is_dir() {
            collect_named_dirs(&entry.path(), name, result, depth - 1);
        }
    }
}

fn metadata_field(contents: &str, field: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        line.strip_prefix(field)
            .and_then(|value| value.strip_prefix(':'))
            .map(|value| value.trim().to_owned())
    })
}

fn parse_dependency_token(value: &str) -> Option<String> {
    let value = value
        .split('#')
        .next()
        .unwrap_or(value)
        .trim()
        .trim_matches(['[', ']', ',', ' ', '"', '\'']);
    let value = value.split(';').next().unwrap_or(value);
    let value = value
        .split(['<', '>', '=', '!', '~', '['])
        .next()
        .unwrap_or(value)
        .trim();
    (!value.is_empty()
        && value.bytes().any(|byte| byte.is_ascii_alphabetic())
        && value
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_alphabetic()))
    .then(|| value.to_owned())
}

fn normalize_distribution_name(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['_', '.'], "-")
}

fn native_extension_evidence(distribution: &PythonDistribution) -> Option<String> {
    let mut package_roots = Vec::new();
    if let Ok(contents) = std::fs::read_to_string(distribution.metadata_dir.join("top_level.txt")) {
        package_roots.extend(
            contents
                .lines()
                .filter(|line| !line.trim().is_empty())
                .map(|line| distribution.root.join(line.trim())),
        );
    }
    if package_roots.is_empty() {
        package_roots.push(
            distribution
                .root
                .join(normalize_distribution_name(&distribution.name).replace('-', "_")),
        );
    }
    let mut stack = package_roots;
    while let Some(path) = stack.pop() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                stack.extend(entries.flatten().map(|entry| entry.path()));
            }
        } else if path.extension().is_some_and(|extension| {
            matches!(extension.to_str(), Some("so" | "pyd" | "dll" | "dylib"))
        }) {
            return Some(path.display().to_string());
        }
    }
    None
}

fn project_python_source_contains(project: &ProjectInspection, token: &str) -> bool {
    project
        .files
        .iter()
        .filter(|path| path.extension().is_some_and(|extension| extension == "py"))
        .any(|path| {
            std::fs::read_to_string(project.root.join(path))
                .map(|contents| {
                    contents.lines().any(|line| {
                        line.trim_start().starts_with("import ") && line.contains(token)
                            || line.trim_start().starts_with("from ") && line.contains(token)
                    })
                })
                .unwrap_or(false)
        })
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
