//! The canonical, versioned PitFast project artifact contract.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};

pub const SCHEMA_VERSION: u32 = 1;
pub const WASI_PREVIEW1_ENTRYPOINT: &str = "_start";

pub fn manifest_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".pit/artifact.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAbi(String);

impl RuntimeAbi {
    pub fn wasi_preview1() -> Self {
        Self("wasi-preview1".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_supported(&self) -> bool {
        self.0 == "wasi-preview1"
    }
}

// Keep RuntimeAbi forward-compatible: an unknown ABI remains readable and can
// be rejected by the runtime compatibility check instead of looking malformed.
impl Serialize for RuntimeAbi {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for RuntimeAbi {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AbiVisitor;
        impl<'de> Visitor<'de> for AbiVisitor {
            type Value = RuntimeAbi;
            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a runtime ABI string")
            }
            fn visit_str<E>(self, value: &str) -> std::result::Result<Self::Value, E>
            where
                E: de::Error,
            {
                Ok(RuntimeAbi(value.to_owned()))
            }
        }
        deserializer.deserialize_str(AbiVisitor)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability(String);

impl Capability {
    pub fn stdio() -> Self {
        Self("stdio".to_owned())
    }
    pub fn args() -> Self {
        Self("args".to_owned())
    }
    pub fn env() -> Self {
        Self("env".to_owned())
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
    pub fn is_supported(&self) -> bool {
        matches!(self.0.as_str(), "stdio" | "args" | "env")
    }
}

impl Serialize for Capability {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for Capability {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Ok(Self(value))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactSpec {
    pub name: String,
    pub path: PathBuf,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildSpec {
    pub language: String,
    pub target: String,
    pub profile: BuildProfile,
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub abi: RuntimeAbi,
    pub entrypoint: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionDefaults {
    pub timeout_ms: Option<u64>,
    pub memory_bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    pub schema_version: u32,
    pub artifact: ArtifactSpec,
    pub build: BuildSpec,
    pub runtime: RuntimeSpec,
    pub execution: ExecutionDefaults,
    pub capabilities: Vec<Capability>,
}

impl ArtifactManifest {
    pub fn validate(&self) -> Result<()> {
        if self.schema_version != SCHEMA_VERSION {
            bail!(
                "unsupported PitFast artifact schema version {}; supported version is {}",
                self.schema_version,
                SCHEMA_VERSION
            );
        }
        if self.artifact.name.trim().is_empty() {
            bail!("artifact name must not be empty");
        }
        validate_relative_path(&self.artifact.path)?;
        if self.artifact.sha256.len() != 64
            || !self
                .artifact
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("artifact sha256 must be a 64-character hexadecimal digest");
        }
        if self.artifact.size_bytes == 0 {
            bail!("artifact size_bytes must be greater than zero");
        }
        if self.build.language.trim().is_empty()
            || self.build.target.trim().is_empty()
            || self.build.fingerprint.len() != 64
            || !self
                .build
                .fingerprint
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        {
            bail!("build metadata is incomplete or has an invalid fingerprint");
        }
        if self.runtime.entrypoint.trim().is_empty() {
            bail!("runtime entrypoint must not be empty");
        }
        if self
            .capabilities
            .iter()
            .any(|capability| !capability.is_supported())
        {
            bail!("manifest declares an unsupported capability");
        }
        Ok(())
    }

    pub fn validate_runtime_compatibility(&self) -> Result<()> {
        if !self.runtime.abi.is_supported() {
            bail!(
                "unsupported runtime ABI '{}'; PitBox currently supports wasi-preview1",
                self.runtime.abi.as_str()
            );
        }
        if self.runtime.entrypoint != WASI_PREVIEW1_ENTRYPOINT {
            bail!(
                "unsupported WASI Preview 1 entrypoint '{}'; expected {}",
                self.runtime.entrypoint,
                WASI_PREVIEW1_ENTRYPOINT
            );
        }
        Ok(())
    }

    pub fn resolve_artifact_path(&self, project_dir: &Path) -> Result<PathBuf> {
        self.validate()?;
        let pit_dir = project_dir.join(".pit");
        let pit_root = pit_dir
            .canonicalize()
            .with_context(|| format!("failed to access {}", pit_dir.display()))?;
        let path = pit_dir.join(&self.artifact.path);
        let canonical = path
            .canonicalize()
            .with_context(|| format!("artifact does not exist: {}", path.display()))?;
        if !canonical.starts_with(&pit_root) {
            bail!("artifact path escapes the .pit directory");
        }
        Ok(canonical)
    }

    pub fn verify_artifact(&self, project_dir: &Path) -> Result<PathBuf> {
        let path = self.resolve_artifact_path(project_dir)?;
        let metadata = fs::metadata(&path)?;
        if metadata.len() != self.artifact.size_bytes {
            bail!(
                "artifact size mismatch: manifest {}, actual {}",
                self.artifact.size_bytes,
                metadata.len()
            );
        }
        let digest = sha256_file(&path)?;
        if digest != self.artifact.sha256 {
            bail!(
                "artifact SHA-256 mismatch: manifest {}, actual {}",
                self.artifact.sha256,
                digest
            );
        }
        Ok(path)
    }

    pub fn to_json(&self) -> Result<String> {
        self.validate()?;
        Ok(serde_json::to_string_pretty(self)? + "\n")
    }

    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let bytes = fs::read(path)
            .with_context(|| format!("failed to read artifact manifest {}", path.display()))?;
        let manifest: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("malformed artifact manifest {}", path.display()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn write_atomic(&self, path: &Path) -> Result<()> {
        let contents = self.to_json()?;
        let parent = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(parent)?;
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let temp = parent.join(format!(
            ".{}.{}.{}.tmp",
            path.file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("manifest"),
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            file.write_all(contents.as_bytes())?;
            file.sync_all()?;
            fs::rename(&temp, path)?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

pub fn validate_relative_path(path: &Path) -> Result<()> {
    if path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
        || path.as_os_str().is_empty()
    {
        bail!("artifact path must be a non-empty relative path inside .pit");
    }
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest() -> ArtifactManifest {
        ArtifactManifest {
            schema_version: SCHEMA_VERSION,
            artifact: ArtifactSpec {
                name: "hello".into(),
                path: "build/hello.wasm".into(),
                sha256: "a".repeat(64),
                size_bytes: 1,
            },
            build: BuildSpec {
                language: "rust".into(),
                target: "wasm32-wasip1".into(),
                profile: BuildProfile::Release,
                fingerprint: "b".repeat(64),
            },
            runtime: RuntimeSpec {
                abi: RuntimeAbi::wasi_preview1(),
                entrypoint: WASI_PREVIEW1_ENTRYPOINT.into(),
            },
            execution: ExecutionDefaults::default(),
            capabilities: vec![Capability::stdio(), Capability::args(), Capability::env()],
        }
    }

    #[test]
    fn roundtrip_is_deterministic() {
        let original = manifest();
        let json = original.to_json().unwrap();
        let decoded: ArtifactManifest = serde_json::from_str(&json).unwrap();
        assert_eq!(original, decoded);
        assert_eq!(json, decoded.to_json().unwrap());
    }

    #[test]
    fn rejects_unsupported_schema_and_traversal() {
        let mut value = manifest();
        value.schema_version = 99;
        assert!(value.validate().is_err());
        let mut value = manifest();
        value.artifact.path = "../outside.wasm".into();
        assert!(value.validate().is_err());
    }

    #[test]
    fn rejects_bad_integrity_metadata() {
        let mut value = manifest();
        value.artifact.sha256 = "bad".into();
        assert!(value.validate().is_err());
        let mut value = manifest();
        value.artifact.size_bytes = 0;
        assert!(value.validate().is_err());
    }

    #[test]
    fn unknown_abi_is_readable_but_not_compatible() {
        let mut value = manifest();
        value.runtime.abi = RuntimeAbi("wasi-preview2".into());
        assert!(value.validate().is_ok());
        assert!(value.validate_runtime_compatibility().is_err());
    }

    #[test]
    fn atomic_manifest_roundtrip_verifies_sha256_and_size() {
        let root = std::env::temp_dir().join(format!("pit-artifact-{}", std::process::id()));
        let artifact_path = root.join(".pit/build/hello.wasm");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        std::fs::write(&artifact_path, b"wasm").unwrap();
        let mut value = manifest();
        value.artifact.path = "build/hello.wasm".into();
        value.artifact.sha256 = sha256_file(&artifact_path).unwrap();
        value.artifact.size_bytes = 4;
        let manifest_path = root.join(".pit/artifact.json");
        value.write_atomic(&manifest_path).unwrap();
        let loaded = ArtifactManifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.verify_artifact(&root).unwrap(), artifact_path);
        std::fs::write(&artifact_path, b"changed").unwrap();
        assert!(loaded.verify_artifact(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
