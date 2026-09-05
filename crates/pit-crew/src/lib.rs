//! Public, CLI-independent build orchestration for PitFast.

use std::sync::Arc;

use anyhow::{Context, Result, bail};
use pit_artifact::ArtifactManifest;
use pit_builder_core::{BuildOutput, Detection};

pub use pit_artifact::{
    ArtifactFormat, ArtifactSpec, BuildProfile, BuildSpec, Capability, ComponentWorld, Entrypoint,
    RuntimeAbi, RuntimeSpec, SCHEMA_VERSION, WASI_PREVIEW1_ENTRYPOINT, WASI_PREVIEW2_ENTRYPOINT,
    manifest_path, sha256_file,
};

pub use pit_builder_core::{BuildRequest, Language, LanguageBuilder, ToolchainInfo};
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
}

impl PitCrew {
    pub fn new(adapters: Vec<Arc<dyn LanguageBuilder>>) -> Self {
        Self { adapters }
    }

    pub fn with_adapter(adapter: impl LanguageBuilder + 'static) -> Self {
        Self::new(vec![Arc::new(adapter)])
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
        let request = BuildRequest {
            project_dir,
            ..request
        };
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
        let adapter = match matches.as_slice() {
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
        let fingerprint = adapter
            .fingerprint(&request)
            .await
            .with_context(|| format!("{} build fingerprint failed", adapter.language()))?;

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

        let artifact = adapter
            .build(&request, &fingerprint)
            .await
            .with_context(|| format!("{} build failed", adapter.language()))?;
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
}
