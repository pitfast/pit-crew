//! Public, CLI-independent build orchestration for PitFast.

pub mod adapters;

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pit_artifact::{ArtifactManifest, detect_artifact_format};
use pit_builder_core::{ApplicationAdapter, BuildOutput, Detection};

pub use pit_artifact::{
    ArtifactFormat, ArtifactSpec, BuildProfile, BuildSpec, Capability, ComponentWorld, Entrypoint,
    RuntimeAbi, RuntimeSpec, SCHEMA_VERSION, WASI_PREVIEW1_ENTRYPOINT, WASI_PREVIEW2_ENTRYPOINT,
    manifest_path, sha256_file,
};

pub use pit_builder_core::{
    AdapterId, AdapterOutput, AdapterWorkspace, ApplicationInterface, BuildRequest,
    CompatibilityCertainty, CompatibilityFinding, CompatibilityReport, CompatibilitySeverity,
    CompatibilityStatus, DetectionCandidate, DetectionEvidence, Language, LanguageBuilder,
    ProjectInspection, ProjectModel, ToolchainInfo,
};
pub type BuildArtifact = BuildOutput;

/// Compatibility name retained for existing integrations. New builders should
/// implement [`LanguageBuilder`] directly.
pub trait BuilderAdapter: LanguageBuilder {}
impl<T: LanguageBuilder + ?Sized> BuilderAdapter for T {}

pub struct BuildOutcome {
    pub artifact: BuildArtifact,
    pub reused: bool,
}

pub struct PitCrew {
    adapters: Vec<Arc<dyn LanguageBuilder>>,
    application_adapters: Vec<Arc<dyn ApplicationAdapter>>,
}

impl PitCrew {
    pub fn new(adapters: Vec<Arc<dyn LanguageBuilder>>) -> Self {
        Self {
            adapters,
            application_adapters: Vec::new(),
        }
    }

    pub fn with_adapter(adapter: impl LanguageBuilder + 'static) -> Self {
        Self::new(vec![Arc::new(adapter)])
    }

    pub fn with_default_adapters(adapters: Vec<Arc<dyn LanguageBuilder>>) -> Self {
        let mut crew = Self::new(adapters);
        crew.application_adapters
            .push(Arc::new(adapters::PythonAsgiAdapter::new()));
        crew.application_adapters
            .push(Arc::new(adapters::JavaScriptFetchAdapter::new(
                Language::JavaScript,
            )));
        crew.application_adapters
            .push(Arc::new(adapters::JavaScriptFetchAdapter::new(
                Language::TypeScript,
            )));
        crew.application_adapters
            .push(Arc::new(adapters::StaticWebAdapter::new(
                Language::JavaScript,
            )));
        crew.application_adapters
            .push(Arc::new(adapters::StaticWebAdapter::new(
                Language::TypeScript,
            )));
        crew.application_adapters
            .push(Arc::new(adapters::GoNetHttpAdapter::new()));
        crew
    }

    pub fn with_application_adapter(mut self, adapter: impl ApplicationAdapter + 'static) -> Self {
        self.application_adapters.push(Arc::new(adapter));
        self
    }

    pub fn application_adapters(&self) -> &[Arc<dyn ApplicationAdapter>] {
        &self.application_adapters
    }

    pub fn inspect(&self, project_dir: &std::path::Path) -> Result<Vec<DetectionCandidate>> {
        let project = ProjectInspection::discover(project_dir)?;
        Ok(self
            .application_adapters
            .iter()
            .filter_map(|adapter| adapter.detect(&project))
            .collect())
    }

    pub fn detect_languages(&self, project_dir: &std::path::Path) -> Result<Vec<Language>> {
        let project = ProjectInspection::discover(project_dir)?;
        let mut languages = self
            .adapters
            .iter()
            .filter(|adapter| adapter.detect(project_dir) == Detection::Yes)
            .map(|adapter| adapter.language())
            .collect::<Vec<_>>();
        languages.extend(
            self.application_adapters
                .iter()
                .filter_map(|adapter| adapter.detect(&project).map(|candidate| candidate.language)),
        );
        languages.sort_by_key(|language| language.as_str());
        languages.dedup();
        Ok(languages)
    }

    /// Run the selected adapter's project/dependency compatibility inspection
    /// without compiling. This is the shared implementation used by `pit
    /// doctor` and by the build path for confirmed blockers.
    pub fn compatibility(
        &self,
        project_dir: &std::path::Path,
        request: &BuildRequest,
    ) -> Result<Option<CompatibilityReport>> {
        let project = ProjectInspection::discover(project_dir)?;
        let candidates = self
            .application_adapters
            .iter()
            .filter_map(|adapter| adapter.detect(&project))
            .filter(|candidate| {
                request
                    .language
                    .is_none_or(|language| candidate.language == language)
            })
            .collect::<Vec<_>>();
        let candidate = choose_candidate(request, &candidates)?;
        let adapter = self.select_application_adapter(request, candidate.as_ref(), &project)?;
        let Some(adapter) = adapter else {
            return Ok(None);
        };
        let model = candidate
            .map(|candidate| ProjectModel {
                language: candidate.language,
                application_interface: request
                    .application_interface
                    .clone()
                    .or(candidate.application_interface),
                entrypoint: request.entrypoint.clone().or(candidate.entrypoint),
                framework_hint: candidate.framework_hint,
                confidence: candidate.confidence,
                evidence: candidate.evidence,
            })
            .unwrap_or_else(|| ProjectModel {
                language: request.language.unwrap_or_else(|| adapter.language()),
                application_interface: request
                    .application_interface
                    .clone()
                    .or_else(|| Some(adapter.interface().clone())),
                entrypoint: request.entrypoint.clone(),
                framework_hint: None,
                confidence: 100,
                evidence: vec![pit_builder_core::DetectionEvidence {
                    source: "explicit configuration".into(),
                    detail: format!("adapter {}", adapter.id()),
                }],
            });
        Ok(Some(adapter.inspect_compatibility(&project, &model)?))
    }

    pub async fn build(&self, request: BuildRequest) -> Result<BuildArtifact> {
        Ok(self.build_with_status(request).await?.artifact)
    }

    pub async fn build_with_status(&self, request: BuildRequest) -> Result<BuildOutcome> {
        let project_dir = request.project_dir.canonicalize().with_context(|| {
            format!(
                "failed to access project directory {}",
                request.project_dir.display()
            )
        })?;
        if !project_dir.is_dir() {
            bail!(
                "project directory is not a directory: {}",
                project_dir.display()
            );
        }
        let mut request = BuildRequest {
            project_dir,
            ..request
        };
        if let Some(raw_artifact) = request.raw_artifact.as_mut() {
            *raw_artifact = raw_artifact.canonicalize().with_context(|| {
                format!(
                    "failed to access raw WASM artifact {}",
                    raw_artifact.display()
                )
            })?;
        }
        let raw_format = if let Some(raw_artifact) = &request.raw_artifact {
            Some(detect_artifact_format(raw_artifact)?)
        } else {
            None
        };
        if let Some(format) = raw_format {
            if request.application_interface.is_none() {
                request.application_interface = Some(match format {
                    ArtifactFormat::Component => ApplicationInterface::new("wasi-http")?,
                    ArtifactFormat::CoreModule => ApplicationInterface::new("wasi-cli")?,
                });
            }
            let fingerprint = raw_artifact_fingerprint(&request, format)?;
            if !request.force
                && let Some(manifest) = load_valid_cached_manifest(&request, &fingerprint)
            {
                let mut manifest = manifest;
                manifest.execution = request.execution_defaults.clone();
                manifest.write_atomic(&manifest_path(&request.project_dir))?;
                return Ok(BuildOutcome {
                    artifact: BuildArtifact::from_manifest(&request.project_dir, manifest)?,
                    reused: true,
                });
            }
            return Ok(BuildOutcome {
                artifact: build_raw_artifact(&request, format, &fingerprint)?,
                reused: false,
            });
        }
        let project_inspection = ProjectInspection::discover(&request.project_dir)?;
        let matches = self
            .adapters
            .iter()
            .filter(|adapter| {
                request
                    .language
                    .is_none_or(|language| adapter.language() == language)
                    && adapter.detect(&request.project_dir) == Detection::Yes
            })
            .collect::<Vec<_>>();
        let language_builder = match matches.as_slice() {
            [adapter] => *adapter,
            [] => bail!(
                "no PitCrew builder detected for {}",
                request.project_dir.display()
            ),
            _ => bail!(
                "multiple PitCrew languages detected for {}; select --language",
                request.project_dir.display()
            ),
        };
        let detected = self
            .application_adapters
            .iter()
            .filter_map(|adapter| adapter.detect(&project_inspection))
            .filter(|candidate| {
                request
                    .language
                    .is_none_or(|language| candidate.language == language)
            })
            .collect::<Vec<_>>();
        let candidate = choose_candidate(&request, &detected)?;
        let selected_adapter =
            self.select_application_adapter(&request, candidate.as_ref(), &project_inspection)?;
        if let Some(adapter) = &selected_adapter {
            let candidate =
                candidate
                    .clone()
                    .unwrap_or_else(|| pit_builder_core::DetectionCandidate {
                        language: language_builder.language(),
                        application_interface: request
                            .application_interface
                            .clone()
                            .or_else(|| Some(adapter.interface().clone())),
                        entrypoint: request.entrypoint.clone(),
                        framework_hint: None,
                        confidence: 100,
                        evidence: vec![pit_builder_core::DetectionEvidence {
                            source: "explicit configuration".into(),
                            detail: format!("adapter {}", adapter.id()),
                        }],
                    });
            request.application_interface = candidate.application_interface.clone();
            request.entrypoint = candidate.entrypoint.clone();
            request.adapter = Some(adapter.id().to_string());
            let model = ProjectModel {
                language: candidate.language,
                application_interface: request.application_interface.clone(),
                entrypoint: request.entrypoint.clone(),
                framework_hint: candidate.framework_hint.clone(),
                confidence: candidate.confidence,
                evidence: candidate.evidence.clone(),
            };
            let compatibility = adapter.inspect_compatibility(&project_inspection, &model)?;
            if compatibility.blocks_build() {
                bail!(format_compatibility_error(&compatibility));
            }
            let output = adapter.prepare(&project_inspection, &model)?;
            request.adapter_workspace = Some(output.workspace);
        } else if request.application_interface.is_none() {
            request.application_interface = Some(default_interface(&request));
        }
        let builder_fingerprint = language_builder
            .fingerprint(&request)
            .await
            .with_context(|| format!("{} build fingerprint failed", language_builder.language()))?;
        let fingerprint = adaptation_fingerprint(&builder_fingerprint, &request);

        if !request.force
            && let Some(manifest) = load_valid_cached_manifest(&request, &fingerprint)
        {
            let mut manifest = manifest;
            manifest.execution = request.execution_defaults.clone();
            manifest.write_atomic(&manifest_path(&request.project_dir))?;
            return Ok(BuildOutcome {
                artifact: BuildArtifact::from_manifest(&request.project_dir, manifest)?,
                reused: true,
            });
        }

        let mut artifact = language_builder
            .build(&request, &fingerprint)
            .await
            .with_context(|| format!("{} build failed", language_builder.language()))?;
        artifact.manifest.build.application_interface = request
            .application_interface
            .as_ref()
            .map(ToString::to_string);
        artifact.manifest.build.adapter = request.adapter.clone();
        artifact.manifest.build.adapter_digest = request
            .adapter_workspace
            .as_ref()
            .map(|workspace| workspace.digest.clone());
        artifact.manifest.validate()?;
        if artifact.manifest.build.fingerprint != fingerprint {
            bail!("builder returned an artifact with a mismatched build fingerprint");
        }
        artifact.manifest.verify_artifact(&request.project_dir)?;
        artifact
            .manifest
            .write_atomic(&manifest_path(&request.project_dir))?;
        Ok(BuildOutcome {
            artifact,
            reused: false,
        })
    }

    fn select_application_adapter(
        &self,
        request: &BuildRequest,
        candidate: Option<&pit_builder_core::DetectionCandidate>,
        _project: &ProjectInspection,
    ) -> Result<Option<Arc<dyn ApplicationAdapter>>> {
        if let Some(value) = &request.adapter {
            let adapter_path = std::path::Path::new(value);
            let adapter_path = if adapter_path.is_absolute() {
                adapter_path.to_path_buf()
            } else {
                request.project_dir.join(adapter_path)
            };
            if adapter_path.is_dir() {
                return Ok(Some(Arc::new(adapters::ExternalAdapter::load(
                    adapter_path,
                )?)));
            }
            let found = self
                .application_adapters
                .iter()
                .find(|adapter| adapter.id().as_str() == value)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("unknown application adapter '{value}'"))?;
            return Ok(Some(found));
        }
        let interface = candidate
            .and_then(|candidate| candidate.application_interface.as_ref())
            .or(request.application_interface.as_ref());
        let Some(interface) = interface else {
            return Ok(None);
        };
        let language = candidate
            .map(|candidate| candidate.language)
            .or(request.language)
            .ok_or_else(|| {
                anyhow::anyhow!("select --language when selecting an application adapter")
            })?;
        let found = self
            .application_adapters
            .iter()
            .filter(|adapter| adapter.language() == language && adapter.interface() == interface)
            .cloned()
            .collect::<Vec<_>>();
        match found.as_slice() {
            [] => {
                if let Some(interface) = &request.application_interface {
                    bail!("no adapter is registered for interface '{}'", interface);
                }
                Ok(None)
            }
            [adapter] => Ok(Some(adapter.clone())),
            _ => bail!("multiple adapters match the detected application interface"),
        }
    }
}

fn format_compatibility_error(report: &CompatibilityReport) -> String {
    let mut message = report
        .messages
        .first()
        .cloned()
        .unwrap_or_else(|| "application compatibility check failed".into());
    for finding in &report.findings {
        if !finding.blocks_build {
            continue;
        }
        message.push_str(&format!(
            "\n\n[{}] {}{}\nReason: {}\nRecommendation: {}",
            finding.category,
            finding.package.as_deref().unwrap_or("project"),
            if finding.dependency_path.is_empty() {
                String::new()
            } else {
                format!(
                    "\nDependency path: {}",
                    finding.dependency_path.join(" -> ")
                )
            },
            finding.reason,
            finding.recommendation
        ));
    }
    message
}

fn choose_candidate(
    request: &BuildRequest,
    candidates: &[pit_builder_core::DetectionCandidate],
) -> Result<Option<pit_builder_core::DetectionCandidate>> {
    if let Some(interface) = &request.application_interface {
        let mut matching = candidates
            .iter()
            .filter(|candidate| candidate.application_interface.as_ref() == Some(interface))
            .cloned()
            .collect::<Vec<_>>();
        if matching.len() > 1 {
            bail!(
                "multiple projects/interfaces match '{}'; specify --adapter and --entry",
                interface
            );
        }
        return Ok(matching.pop());
    }
    match candidates {
        [] => Ok(None),
        [candidate] => Ok(Some(candidate.clone())),
        _ => bail!("multiple application interfaces detected; specify --interface and --entry"),
    }
}

fn default_interface(request: &BuildRequest) -> ApplicationInterface {
    let value = match request.world {
        Some(pit_artifact::ComponentWorld::WasiHttpProxy) => "wasi-http",
        _ => "wasi-cli",
    };
    ApplicationInterface::new(value).expect("built-in interface id is valid")
}

fn adaptation_fingerprint(builder: &str, request: &BuildRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(builder.as_bytes());
    hash.update([0]);
    hash.update(request.adaptation_fingerprint_material().as_bytes());
    format!("{:x}", hash.finalize())
}

fn raw_artifact_fingerprint(request: &BuildRequest, format: ArtifactFormat) -> Result<String> {
    use sha2::{Digest, Sha256};
    let path = request
        .raw_artifact
        .as_ref()
        .context("raw artifact path is missing")?;
    let mut hash = Sha256::new();
    hash.update(b"pit-raw-component-v1");
    hash.update([0]);
    hash.update(std::fs::read(path)?);
    hash.update([0]);
    hash.update(format.to_string().as_bytes());
    hash.update([0]);
    hash.update(request.adaptation_fingerprint_material().as_bytes());
    hash.update([0]);
    hash.update(
        request
            .execution_defaults
            .timeout_ms
            .unwrap_or_default()
            .to_le_bytes(),
    );
    hash.update(
        request
            .execution_defaults
            .memory_bytes
            .unwrap_or_default()
            .to_le_bytes(),
    );
    Ok(format!("{:x}", hash.finalize()))
}

fn build_raw_artifact(
    request: &BuildRequest,
    format: ArtifactFormat,
    fingerprint: &str,
) -> Result<BuildArtifact> {
    let source = request
        .raw_artifact
        .as_ref()
        .context("raw artifact path is missing")?;
    let build_dir = request.project_dir.join(".pit/build");
    fs::create_dir_all(&build_dir)?;
    let name = request
        .project_dir
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("raw-component")
        .replace('-', "_");
    let artifact_path = build_dir.join(format!("{name}.wasm"));
    if source != &artifact_path {
        let temporary = build_dir.join(format!(".{name}.{}.tmp", std::process::id()));
        fs::copy(source, &temporary)?;
        fs::rename(&temporary, &artifact_path)?;
    }
    let (abi, entrypoint, world, target) = match format {
        ArtifactFormat::Component => (
            RuntimeAbi::wasi_preview2(),
            Entrypoint::WasiHttpProxy,
            Some(ComponentWorld::WasiHttpProxy),
            "wasm32-wasip2",
        ),
        ArtifactFormat::CoreModule => (
            RuntimeAbi::wasi_preview1(),
            Entrypoint::wasi_preview1(),
            None,
            "wasm32-wasip1",
        ),
    };
    if request.abi != abi {
        bail!("raw {} requires {}; select the matching --abi", format, abi);
    }
    if let Some(requested_world) = request.world
        && Some(requested_world) != world
    {
        bail!("raw artifact format does not implement requested world {requested_world:?}");
    }
    let toolchain = ToolchainInfo {
        name: "provided".into(),
        version: "raw-artifact".into(),
        compiler: None,
        componentizer: None,
        target: target.into(),
    };
    let manifest = ArtifactManifest {
        schema_version: SCHEMA_VERSION,
        artifact: ArtifactSpec {
            name,
            path: PathBuf::from("build").join(artifact_path.file_name().unwrap()),
            sha256: sha256_file(&artifact_path)?,
            size_bytes: fs::metadata(&artifact_path)?.len(),
        },
        build: BuildSpec {
            language: request
                .language
                .map(|language| language.to_string())
                .unwrap_or_else(|| "raw".into()),
            target: target.into(),
            profile: request.profile,
            fingerprint: fingerprint.into(),
            toolchain: Some(toolchain.name.clone()),
            toolchain_version: Some(toolchain.version.clone()),
            application_interface: request
                .application_interface
                .as_ref()
                .map(ToString::to_string),
            adapter: None,
            adapter_digest: None,
        },
        runtime: RuntimeSpec {
            abi,
            entrypoint,
            format,
            world,
        },
        execution: request.execution_defaults.clone(),
        capabilities: Vec::new(),
    };
    manifest.validate()?;
    manifest.verify_artifact(&request.project_dir)?;
    manifest.write_atomic(&manifest_path(&request.project_dir))?;
    Ok(BuildArtifact {
        manifest,
        artifact_path,
        toolchain,
    })
}

fn load_valid_cached_manifest(
    request: &BuildRequest,
    fingerprint: &str,
) -> Option<ArtifactManifest> {
    let manifest = ArtifactManifest::load(manifest_path(&request.project_dir)).ok()?;
    if manifest.build.fingerprint != fingerprint
        || manifest.build.profile != request.profile
        || manifest.build.target != request.abi.target().unwrap_or_default()
        || manifest.runtime.abi != request.abi
        || (request.abi.as_str() == "wasi-preview2"
            && request.world.is_some()
            && manifest.runtime.world != request.world)
        || manifest.build.application_interface
            != request
                .application_interface
                .as_ref()
                .map(ToString::to_string)
        || manifest.build.adapter != request.adapter
        || manifest.build.adapter_digest
            != request
                .adapter_workspace
                .as_ref()
                .map(|workspace| workspace.digest.clone())
    {
        return None;
    }
    if manifest.verify_artifact(&request.project_dir).is_err() {
        return None;
    }
    Some(manifest)
}

#[cfg(test)]
mod tests {
    use super::{BuildArtifact, BuildProfile, BuildRequest, Language, LanguageBuilder, PitCrew};
    use async_trait::async_trait;
    use pit_artifact::{
        ArtifactFormat, ArtifactManifest, ArtifactSpec, BuildSpec, Capability, Entrypoint,
        ExecutionDefaults, RuntimeAbi, RuntimeSpec,
    };
    use pit_builder_core::{Detection, ToolchainInfo};
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBuilder {
        builds: Arc<AtomicUsize>,
    }

    struct DetectOnlyBuilder {
        language: Language,
    }

    #[async_trait]
    impl LanguageBuilder for DetectOnlyBuilder {
        fn language(&self) -> Language {
            self.language
        }

        fn detect(&self, _project_dir: &Path) -> Detection {
            Detection::Yes
        }

        async fn probe_toolchain(&self) -> anyhow::Result<ToolchainInfo> {
            anyhow::bail!("probe not needed for detection test")
        }

        async fn fingerprint(&self, _request: &BuildRequest) -> anyhow::Result<String> {
            anyhow::bail!("fingerprint not needed for detection test")
        }

        async fn build(
            &self,
            _request: &BuildRequest,
            _fingerprint: &str,
        ) -> anyhow::Result<BuildArtifact> {
            anyhow::bail!("build not needed for detection test")
        }
    }

    #[async_trait]
    impl LanguageBuilder for FakeBuilder {
        fn language(&self) -> Language {
            Language::Rust
        }
        fn detect(&self, project_dir: &Path) -> Detection {
            if project_dir.is_dir() {
                Detection::Yes
            } else {
                Detection::No
            }
        }
        async fn probe_toolchain(&self) -> anyhow::Result<ToolchainInfo> {
            Ok(ToolchainInfo {
                name: "test".into(),
                version: "1".into(),
                compiler: None,
                componentizer: None,
                target: "wasm32-wasip1".into(),
            })
        }
        async fn fingerprint(&self, _request: &BuildRequest) -> anyhow::Result<String> {
            Ok("a".repeat(64))
        }
        async fn build(
            &self,
            request: &BuildRequest,
            fingerprint: &str,
        ) -> anyhow::Result<BuildArtifact> {
            self.builds.fetch_add(1, Ordering::SeqCst);
            let project = &request.project_dir;
            let dir = project.join(".pit/build");
            std::fs::create_dir_all(&dir)?;
            let path = dir.join("test.wasm");
            std::fs::write(&path, [0, 97, 115, 109, 1, 0, 0, 0])?;
            let manifest = ArtifactManifest {
                schema_version: 1,
                artifact: ArtifactSpec {
                    name: "test".into(),
                    path: "build/test.wasm".into(),
                    sha256: super::sha256_file(&path)?,
                    size_bytes: 8,
                },
                build: BuildSpec {
                    language: "test".into(),
                    target: "wasm32-wasip1".into(),
                    profile: BuildProfile::Release,
                    fingerprint: fingerprint.into(),
                    toolchain: None,
                    toolchain_version: None,
                    application_interface: None,
                    adapter: None,
                    adapter_digest: None,
                },
                runtime: RuntimeSpec {
                    abi: RuntimeAbi::wasi_preview1(),
                    entrypoint: Entrypoint::wasi_preview1(),
                    format: ArtifactFormat::CoreModule,
                    world: None,
                },
                execution: ExecutionDefaults::default(),
                capabilities: vec![Capability::stdio()],
            };
            Ok(BuildArtifact {
                manifest,
                artifact_path: path,
                toolchain: ToolchainInfo {
                    name: "test".into(),
                    version: "1".into(),
                    compiler: None,
                    componentizer: None,
                    target: "wasm32-wasip1".into(),
                },
            })
        }
    }

    #[tokio::test]
    async fn cached_build_reuses_verified_artifact() {
        let root = std::env::temp_dir().join(format!("pit-crew-cache-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let builds = Arc::new(AtomicUsize::new(0));
        let crew = PitCrew::with_adapter(FakeBuilder {
            builds: Arc::clone(&builds),
        });
        let mut request = BuildRequest::new(&root);
        request.abi = RuntimeAbi::wasi_preview1();
        assert!(
            !crew
                .build_with_status(request.clone())
                .await
                .unwrap()
                .reused
        );
        assert!(crew.build_with_status(request).await.unwrap().reused);
        assert_eq!(builds.load(Ordering::SeqCst), 1);
        std::fs::write(root.join(".pit/build/test.wasm"), b"corrupt").unwrap();
        assert!(
            !crew
                .build_with_status(BuildRequest::new(&root))
                .await
                .unwrap()
                .reused
        );
        assert_eq!(builds.load(Ordering::SeqCst), 2);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn ambiguous_detection_requires_explicit_language() {
        let root = std::env::temp_dir().join(format!("pit-crew-detect-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let crew = PitCrew::new(vec![
            Arc::new(DetectOnlyBuilder {
                language: Language::Rust,
            }),
            Arc::new(DetectOnlyBuilder {
                language: Language::Go,
            }),
        ]);
        let error = match crew.build_with_status(BuildRequest::new(&root)).await {
            Ok(_) => panic!("ambiguous detection unexpectedly built"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("multiple PitCrew languages detected"));

        let mut request = BuildRequest::new(&root);
        request.language = Some(Language::Go);
        let error = match crew.build_with_status(request).await {
            Ok(_) => panic!("detection test unexpectedly built"),
            Err(error) => error.to_string(),
        };
        assert!(error.contains("build fingerprint failed"));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn unknown_framework_is_detected_by_interface_shape_only() {
        let root = std::env::temp_dir().join(format!("pit-crew-asgi-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("pyproject.toml"), "[project]\nname='mystery'\n").unwrap();
        std::fs::write(
            root.join("main.py"),
            "async def app(scope, receive, send):\n    pass\n",
        )
        .unwrap();
        let crew = PitCrew::new(Vec::new())
            .with_application_adapter(crate::adapters::PythonAsgiAdapter::new());
        let candidates = crew.inspect(&root).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(
            candidates[0]
                .application_interface
                .as_ref()
                .unwrap()
                .as_str(),
            "asgi"
        );
        assert!(candidates[0].framework_hint.is_none());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn ambiguous_interfaces_require_an_explicit_selection() {
        let root = std::env::temp_dir().join(format!("pit-crew-interfaces-{}", std::process::id()));
        let candidates = vec![
            pit_builder_core::DetectionCandidate {
                language: Language::Python,
                application_interface: Some("asgi".parse().unwrap()),
                entrypoint: Some("main:app".into()),
                framework_hint: None,
                confidence: 90,
                evidence: Vec::new(),
            },
            pit_builder_core::DetectionCandidate {
                language: Language::Python,
                application_interface: Some("wsgi".parse().unwrap()),
                entrypoint: Some("main:application".into()),
                framework_hint: None,
                confidence: 90,
                evidence: Vec::new(),
            },
        ];
        let error = super::choose_candidate(&BuildRequest::new(&root), &candidates)
            .unwrap_err()
            .to_string();
        assert!(error.contains("multiple application interfaces"));
        let mut request = BuildRequest::new(&root);
        request.application_interface = Some("wsgi".parse().unwrap());
        assert_eq!(
            super::choose_candidate(&request, &candidates)
                .unwrap()
                .unwrap()
                .entrypoint
                .as_deref(),
            Some("main:application")
        );
    }
}
