//! Public, CLI-independent build orchestration for PitFast.
//!
//! PitCrew coordinates builder adapters. It does not compile source code
//! itself; a language-specific adapter owns toolchain and compiler details.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub project_dir: PathBuf,
    pub bin: Option<String>,
    pub profile: BuildProfile,
}

impl BuildRequest {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
            bin: None,
            profile: BuildProfile::Release,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BuildProfile {
    Debug,
    Release,
}

impl BuildProfile {
    pub fn cargo_directory(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Debug => "debug",
            Self::Release => "release",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildArtifact {
    pub name: String,
    pub language: Language,
    pub target: String,
    pub profile: BuildProfile,
    pub artifact_path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub name: String,
    pub language: Language,
    pub target: String,
    pub profile: BuildProfile,
    /// Path relative to the .pit directory.
    pub artifact: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

impl ArtifactManifest {
    pub const SCHEMA_VERSION: u32 = 1;

    pub fn from_artifact(project_dir: &Path, artifact: &BuildArtifact) -> Result<Self> {
        let pit_dir = project_dir.join(".pit");
        let relative_artifact =
            artifact
                .artifact_path
                .strip_prefix(&pit_dir)
                .with_context(|| {
                    format!(
                        "artifact path '{}' is not inside '{}'",
                        artifact.artifact_path.display(),
                        pit_dir.display()
                    )
                })?;
        Ok(Self {
            schema_version: Self::SCHEMA_VERSION,
            name: artifact.name.clone(),
            language: artifact.language,
            target: artifact.target.clone(),
            profile: artifact.profile,
            artifact: relative_artifact.to_path_buf(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        })
    }

    pub async fn read(project_dir: impl AsRef<Path>) -> Result<Self> {
        let path = manifest_path(project_dir.as_ref());
        let bytes = tokio::fs::read(&path)
            .await
            .with_context(|| format!("failed to read {}", path.display()))?;
        let manifest = serde_json::from_slice(&bytes)
            .with_context(|| format!("malformed PitFast artifact manifest {}", path.display()))?;
        Ok(manifest)
    }

    pub fn resolve_path(&self, project_dir: &Path) -> Result<PathBuf> {
        if self.schema_version != Self::SCHEMA_VERSION {
            bail!(
                "unsupported PitFast artifact manifest schema version {}",
                self.schema_version
            );
        }
        if self.artifact.is_absolute()
            || self
                .artifact
                .components()
                .any(|component| matches!(component, std::path::Component::ParentDir))
        {
            bail!("artifact path in manifest must be relative to .pit and cannot escape it");
        }
        let path = project_dir.join(".pit").join(&self.artifact);
        if !path.is_file() {
            bail!(
                "artifact referenced by manifest does not exist: {}",
                path.display()
            );
        }
        Ok(path)
    }
}

pub fn manifest_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".pit").join("artifact.json")
}

#[async_trait]
pub trait BuilderAdapter: Send + Sync {
    fn language(&self) -> Language;
    fn detect(&self, project_dir: &Path) -> bool;
    async fn build(&self, request: &BuildRequest) -> Result<BuildArtifact>;
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
        let artifact = adapter
            .build(&request)
            .await
            .with_context(|| format!("{} build failed", format_language(adapter.language())))?;
        let manifest = ArtifactManifest::from_artifact(&request.project_dir, &artifact)?;
        let manifest_path = manifest_path(&request.project_dir);
        let manifest_json = serde_json::to_string_pretty(&manifest)? + "\n";
        tokio::fs::create_dir_all(manifest_path.parent().unwrap_or(Path::new("."))).await?;
        tokio::fs::write(&manifest_path, manifest_json)
            .await
            .with_context(|| format!("failed to write {}", manifest_path.display()))?;
        Ok(artifact)
    }
}

fn format_language(language: Language) -> &'static str {
    match language {
        Language::Rust => "Rust",
    }
}

#[cfg(test)]
mod tests {
    use super::{ArtifactManifest, BuildArtifact, BuildProfile, Language, manifest_path};
    use std::path::{Path, PathBuf};

    #[test]
    fn manifest_serialization_is_stable_and_relative() {
        let project = PathBuf::from("/tmp/project");
        let artifact = BuildArtifact {
            name: "hello".to_owned(),
            language: Language::Rust,
            target: "wasm32-wasip1".to_owned(),
            profile: BuildProfile::Release,
            artifact_path: project.join(".pit/build/hello.wasm"),
            sha256: "abc".to_owned(),
            size_bytes: 3,
        };
        let manifest = ArtifactManifest::from_artifact(&project, &artifact).unwrap();
        assert_eq!(manifest_path(&project), project.join(".pit/artifact.json"));
        assert_eq!(manifest.artifact, PathBuf::from("build/hello.wasm"));
        let json = serde_json::to_string(&manifest).unwrap();
        assert!(json.contains("\"schema_version\":1"));
        assert!(json.contains("\"language\":\"rust\""));
    }

    #[test]
    fn rejects_manifest_escape_paths() {
        let manifest = ArtifactManifest {
            schema_version: 1,
            name: "x".to_owned(),
            language: Language::Rust,
            target: "wasm32-wasip1".to_owned(),
            profile: BuildProfile::Release,
            artifact: PathBuf::from("../x.wasm"),
            sha256: String::new(),
            size_bytes: 0,
        };
        assert!(manifest.resolve_path(Path::new("/tmp/project")).is_err());
    }
}
