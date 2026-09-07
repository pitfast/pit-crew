//! The language-neutral contract between PitCrew orchestration and builders.

use std::path::{Path, PathBuf};
use std::str::FromStr;

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

/// A stable, extensible application-facing interface identifier.
///
/// This is deliberately not an enum: adding a new interface or a local
/// adapter must not require a PitFast runtime release.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ApplicationInterface(String);

impl ApplicationInterface {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_identifier(&value, "application interface")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ApplicationInterface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for ApplicationInterface {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

/// A stable adapter identity. Local adapters may use names such as
/// `local/banana-http`; built-in adapters use names such as `python/asgi`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AdapterId(String);

impl AdapterId {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        validate_identifier(&value, "adapter id")?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for AdapterId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for AdapterId {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::new(value)
    }
}

fn validate_identifier(value: &str, kind: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_lowercase()
                || byte.is_ascii_digit()
                || matches!(byte, b'-' | b'_' | b'/' | b'.') && index > 0 && index + 1 < value.len()
        })
        || !value.as_bytes()[0].is_ascii_lowercase() && !value.as_bytes()[0].is_ascii_digit()
    {
        anyhow::bail!(
            "invalid {kind} '{value}'; use lowercase ASCII segments separated by '-', '_', '/', or '.'"
        );
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionEvidence {
    pub source: String,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct ProjectInspection {
    pub root: PathBuf,
    pub files: Vec<PathBuf>,
}

impl ProjectInspection {
    pub fn discover(root: &Path) -> Result<Self> {
        let mut files = Vec::new();
        collect_project_files(root, root, &mut files)?;
        files.sort();
        Ok(Self {
            root: root.to_path_buf(),
            files,
        })
    }

    pub fn has_file(&self, relative: &str) -> bool {
        self.files.iter().any(|path| path == Path::new(relative))
    }
}

fn collect_project_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if matches!(
                name.as_ref(),
                ".git" | ".pit" | "target" | "node_modules" | "venv"
            ) {
                continue;
            }
            collect_project_files(root, &path, files)?;
        } else if path.is_file() {
            files.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionCandidate {
    pub language: Language,
    pub application_interface: Option<ApplicationInterface>,
    pub entrypoint: Option<String>,
    pub framework_hint: Option<String>,
    pub confidence: u8,
    pub evidence: Vec<DetectionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectModel {
    pub language: Language,
    pub application_interface: Option<ApplicationInterface>,
    pub entrypoint: Option<String>,
    pub framework_hint: Option<String>,
    pub confidence: u8,
    pub evidence: Vec<DetectionEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterWorkspace {
    pub root: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub wit_path: Option<PathBuf>,
    pub entrypoint: String,
    pub adapter: AdapterId,
    pub digest: String,
    pub generated_files: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterOutput {
    pub workspace: AdapterWorkspace,
    pub runtime_world: ComponentWorld,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompatibilityReport {
    pub status: CompatibilityStatus,
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompatibilityStatus {
    Supported,
    PotentialIssue,
    Unsupported,
    Unknown,
}

/// Build-time adapter contract. Implementations may generate files under
/// `.pit/generated`; they never execute guest code and never affect PitBox.
pub trait ApplicationAdapter: Send + Sync {
    fn id(&self) -> AdapterId;
    fn language(&self) -> Language;
    fn interface(&self) -> &ApplicationInterface;
    fn runtime_world(&self) -> ComponentWorld;
    fn detect(&self, project: &ProjectInspection) -> Option<DetectionCandidate>;
    fn validate(&self, model: &ProjectModel) -> Result<CompatibilityReport>;
    fn prepare(&self, project: &ProjectInspection, model: &ProjectModel) -> Result<AdapterOutput>;
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
    pub application_interface: Option<ApplicationInterface>,
    pub entrypoint: Option<String>,
    pub adapter: Option<String>,
    pub adapter_workspace: Option<AdapterWorkspace>,
    pub raw_artifact: Option<PathBuf>,
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
            application_interface: None,
            entrypoint: None,
            adapter: None,
            adapter_workspace: None,
            raw_artifact: None,
        }
    }
}

impl BuildRequest {
    /// Stable adaptation inputs appended by orchestration to each builder's
    /// language/toolchain fingerprint.
    pub fn adaptation_fingerprint_material(&self) -> String {
        format!(
            "interface={};entry={};adapter={};adapter_digest={}",
            self.application_interface
                .as_ref()
                .map_or("", ApplicationInterface::as_str),
            self.entrypoint.as_deref().unwrap_or(""),
            self.adapter.as_deref().unwrap_or(""),
            self.adapter_workspace
                .as_ref()
                .map(|workspace| workspace.digest.as_str())
                .unwrap_or("")
        )
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
    use super::{AdapterId, ApplicationInterface, Language};
    use std::str::FromStr;

    #[test]
    fn language_names_are_stable() {
        for language in Language::ALL {
            assert_eq!(Language::from_str(language.as_str()).unwrap(), language);
        }
        assert_eq!(Language::Cpp.to_string(), "cpp");
        assert_eq!(Language::CSharp.to_string(), "csharp");
    }

    #[test]
    fn extensible_identifiers_are_stable_and_validated() {
        assert_eq!(
            ApplicationInterface::new("python/asgi").unwrap().as_str(),
            "python/asgi"
        );
        assert_eq!(
            AdapterId::new("local/banana-http").unwrap().to_string(),
            "local/banana-http"
        );
        assert!(ApplicationInterface::new("ASGI").is_err());
        assert!(ApplicationInterface::new("../asgi").is_err());
        assert!(AdapterId::new("local/").is_err());
    }
}
