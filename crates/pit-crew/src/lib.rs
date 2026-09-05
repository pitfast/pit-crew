//! Public, CLI-independent build orchestration for PitFast.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use pit_artifact::{ArtifactManifest, ExecutionDefaults};

pub use pit_artifact::{
    ArtifactSpec, BuildProfile, BuildSpec, Capability, RuntimeAbi, RuntimeSpec, SCHEMA_VERSION,
    WASI_PREVIEW1_ENTRYPOINT, manifest_path, sha256_file,
};

#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub project_dir: PathBuf,
    pub bin: Option<String>,
    pub profile: BuildProfile,
    pub execution_defaults: ExecutionDefaults,
    pub force: bool,
}

impl BuildRequest {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
            bin: None,
            profile: BuildProfile::Release,
            execution_defaults: ExecutionDefaults::default(),
            force: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildArtifact {
    pub manifest: ArtifactManifest,
    pub artifact_path: PathBuf,
}

impl BuildArtifact {
    pub fn from_manifest(project_dir: &Path, manifest: ArtifactManifest) -> Result<Self> {
        let artifact_path = manifest.resolve_artifact_path(project_dir)?;
        Ok(Self {
            manifest,
            artifact_path,
        })
    }
}

#[async_trait]
pub trait BuilderAdapter: Send + Sync {
    fn language(&self) -> &str;
    fn detect(&self, project_dir: &Path) -> bool;
    async fn fingerprint(&self, request: &BuildRequest) -> Result<String>;
    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildArtifact>;
}

pub struct BuildOutcome {
    pub artifact: BuildArtifact,
    pub reused: bool,
}

pub struct PitCrew {
    adapters: Vec<Arc<dyn BuilderAdapter>>,
}

impl PitCrew {
    pub fn new(adapters: Vec<Arc<dyn BuilderAdapter>>) -> Self {
        Self { adapters }
    }

    pub fn with_adapter(adapter: impl BuilderAdapter + 'static) -> Self {
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
        let adapter = self
            .adapters
            .iter()
            .find(|adapter| adapter.detect(&request.project_dir))
            .ok_or_else(|| {
                anyhow!(
                    "no PitCrew builder detected for {}",
                    request.project_dir.display()
                )
            })?;
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
    if manifest.build.fingerprint != fingerprint || manifest.build.profile != request.profile {
        return None;
    }
    if manifest.verify_artifact(&request.project_dir).is_err() {
        return None;
    }
    Some(manifest)
}

#[cfg(test)]
mod tests {
    use super::{BuildArtifact, BuildProfile, BuildRequest, PitCrew};
    use async_trait::async_trait;
    use pit_artifact::{
        ArtifactManifest, ArtifactSpec, BuildSpec, Capability, ExecutionDefaults, RuntimeAbi,
        RuntimeSpec,
    };
    use std::path::Path;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct FakeBuilder {
        builds: Arc<AtomicUsize>,
    }

    #[async_trait]
    impl super::BuilderAdapter for FakeBuilder {
        fn language(&self) -> &str {
            "test"
        }
        fn detect(&self, project_dir: &Path) -> bool {
            project_dir.is_dir()
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
            std::fs::write(&path, b"wasm")?;
            let manifest = ArtifactManifest {
                schema_version: 1,
                artifact: ArtifactSpec {
                    name: "test".into(),
                    path: "build/test.wasm".into(),
                    sha256: super::sha256_file(&path)?,
                    size_bytes: 4,
                },
                build: BuildSpec {
                    language: "test".into(),
                    target: "test".into(),
                    profile: BuildProfile::Release,
                    fingerprint: fingerprint.into(),
                },
                runtime: RuntimeSpec {
                    abi: RuntimeAbi::wasi_preview1(),
                    entrypoint: "_start".into(),
                },
                execution: ExecutionDefaults::default(),
                capabilities: vec![Capability::stdio()],
            };
            Ok(BuildArtifact {
                manifest,
                artifact_path: path,
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
        let request = BuildRequest::new(&root);
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
}
