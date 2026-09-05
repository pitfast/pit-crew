//! The language-neutral contract between PitCrew orchestration and builders.

use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use pit_artifact::{ArtifactManifest, BuildProfile, ComponentWorld, ExecutionDefaults, RuntimeAbi};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Rust,
    Go,
    C,
    #[serde(rename = "cpp")]
    Cpp,
    #[serde(rename = "javascript")]
    JavaScript,
    TypeScript,
    Python,
    #[serde(rename = "csharp")]
    CSharp,
    Java,
}

impl Language {
    pub const ALL: [Self; 9] = [
        Self::Rust,
        Self::Go,
        Self::C,
        Self::Cpp,
        Self::JavaScript,
        Self::TypeScript,
        Self::Python,
        Self::CSharp,
        Self::Java,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::C => "c",
            Self::Cpp => "cpp",
            Self::JavaScript => "javascript",
            Self::TypeScript => "typescript",
            Self::Python => "python",
            Self::CSharp => "csharp",
            Self::Java => "java",
        }
    }
}

impl std::fmt::Display for Language {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl std::str::FromStr for Language {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value.to_ascii_lowercase().as_str() {
            "rust" => Ok(Self::Rust),
            "go" => Ok(Self::Go),
            "c" => Ok(Self::C),
            "cpp" | "c++" => Ok(Self::Cpp),
            "javascript" | "js" => Ok(Self::JavaScript),
            "typescript" | "ts" => Ok(Self::TypeScript),
            "python" | "py" => Ok(Self::Python),
            "csharp" | "c#" => Ok(Self::CSharp),
            "java" => Ok(Self::Java),
            _ => Err(anyhow::anyhow!("unsupported language '{value}'")),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolchainInfo {
    pub name: String,
    pub version: String,
    pub compiler: Option<String>,
    pub componentizer: Option<String>,
    pub target: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Detection {
    No,
    Yes,
}

#[derive(Debug, Clone)]
pub struct BuildRequest {
    pub project_dir: PathBuf,
    pub bin: Option<String>,
    /// Optional WIT package or directory used by source-level componentizers.
    pub wit_path: Option<PathBuf>,
    pub profile: BuildProfile,
    pub abi: RuntimeAbi,
    pub world: Option<ComponentWorld>,
    pub execution_defaults: ExecutionDefaults,
    pub force: bool,
    pub language: Option<Language>,
}

impl BuildRequest {
    pub fn new(project_dir: impl Into<PathBuf>) -> Self {
        Self {
            project_dir: project_dir.into(),
            bin: None,
            wit_path: None,
            profile: BuildProfile::Release,
            abi: RuntimeAbi::wasi_preview2(),
            world: None,
            execution_defaults: ExecutionDefaults::default(),
            force: false,
            language: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOutput {
    pub manifest: ArtifactManifest,
    pub artifact_path: PathBuf,
    pub toolchain: ToolchainInfo,
}

impl BuildOutput {
    pub fn from_manifest(project_dir: &Path, manifest: ArtifactManifest) -> Result<Self> {
        let artifact_path = manifest.resolve_artifact_path(project_dir)?;
        Ok(Self {
            toolchain: ToolchainInfo {
                name: manifest
                    .build
                    .toolchain
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                version: manifest
                    .build
                    .toolchain_version
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
                compiler: None,
                componentizer: None,
                target: manifest.build.target.clone(),
            },
            manifest,
            artifact_path,
        })
    }
}

#[async_trait]
pub trait LanguageBuilder: Send + Sync {
    fn language(&self) -> Language;
    fn detect(&self, project_dir: &Path) -> Detection;
    async fn probe_toolchain(&self) -> Result<ToolchainInfo>;
    async fn fingerprint(&self, request: &BuildRequest) -> Result<String>;
    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildOutput>;
}

#[cfg(test)]
mod tests {
    use super::Language;
    use std::str::FromStr;

    #[test]
    fn language_names_are_stable() {
        for language in Language::ALL {
            assert_eq!(Language::from_str(language.as_str()).unwrap(), language);
        }
        assert_eq!(Language::Cpp.to_string(), "cpp");
        assert_eq!(Language::CSharp.to_string(), "csharp");
    }
}
