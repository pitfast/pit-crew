//! The canonical, versioned PitFast project artifact contract.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, bail};
use serde::de::{self, Visitor};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use sha2::{Digest, Sha256};
use wasmparser::{Encoding, Parser, Payload};

pub const SCHEMA_VERSION: u32 = 1;
pub const WASI_PREVIEW1_ENTRYPOINT: &str = "_start";
pub const WASI_PREVIEW2_ENTRYPOINT: &str = "wasi:cli/command";
pub const WASI_HTTP_PROXY_WORLD: &str = "wasi:http/proxy";

pub fn manifest_path(project_dir: &Path) -> PathBuf {
    project_dir.join(".pit/artifact.json")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeAbi(String);

impl RuntimeAbi {
    pub fn wasi_preview1() -> Self {
        Self("wasi-preview1".to_owned())
    }

    pub fn wasi_preview2() -> Self {
        Self("wasi-preview2".to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn target(&self) -> Option<&'static str> {
        match self.0.as_str() {
            "wasi-preview1" => Some("wasm32-wasip1"),
            "wasi-preview2" => Some("wasm32-wasip2"),
            _ => None,
        }
    }

    pub fn is_supported(&self) -> bool {
        matches!(self.0.as_str(), "wasi-preview1" | "wasi-preview2")
    }
}

impl FromStr for RuntimeAbi {
    type Err = anyhow::Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if matches!(value, "wasi-preview1" | "wasi-preview2") {
            Ok(Self(value.to_owned()))
        } else {
            bail!("unsupported runtime ABI '{value}'; expected wasi-preview1 or wasi-preview2")
        }
    }
}

impl std::fmt::Display for RuntimeAbi {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
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
#[serde(rename_all = "kebab-case")]
pub enum ArtifactFormat {
    CoreModule,
    Component,
}

impl std::fmt::Display for ArtifactFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::CoreModule => "core-module",
            Self::Component => "component",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entrypoint {
    WasiPreview1Start,
    WasiPreview2Command,
    WasiHttpProxy,
    Custom(String),
}

impl Entrypoint {
    pub fn wasi_preview1() -> Self {
        Self::WasiPreview1Start
    }

    pub fn wasi_preview2() -> Self {
        Self::WasiPreview2Command
    }

    pub fn as_str(&self) -> &str {
        match self {
            Self::WasiPreview1Start => WASI_PREVIEW1_ENTRYPOINT,
            Self::WasiPreview2Command => WASI_PREVIEW2_ENTRYPOINT,
            Self::WasiHttpProxy => WASI_HTTP_PROXY_WORLD,
            Self::Custom(value) => value,
        }
    }

    fn from_string(value: String) -> Self {
        match value.as_str() {
            WASI_PREVIEW1_ENTRYPOINT => Self::wasi_preview1(),
            WASI_PREVIEW2_ENTRYPOINT => Self::wasi_preview2(),
            WASI_HTTP_PROXY_WORLD => Self::WasiHttpProxy,
            _ => Self::Custom(value),
        }
    }
}

/// Standard Component Model world implemented by a component artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ComponentWorld {
    #[serde(rename = "wasi:cli/command")]
    WasiCliCommand,
    #[serde(rename = "wasi:http/proxy")]
    WasiHttpProxy,
}

impl ComponentWorld {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WasiCliCommand => WASI_PREVIEW2_ENTRYPOINT,
            Self::WasiHttpProxy => WASI_HTTP_PROXY_WORLD,
        }
    }
}

impl FromStr for ComponentWorld {
    type Err = anyhow::Error;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            WASI_PREVIEW2_ENTRYPOINT => Ok(Self::WasiCliCommand),
            WASI_HTTP_PROXY_WORLD => Ok(Self::WasiHttpProxy),
            _ => bail!("unsupported Component Model world '{value}'"),
        }
    }
}

impl Serialize for Entrypoint {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for Entrypoint {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Ok(Self::from_string(String::deserialize(deserializer)?))
    }
}

impl std::fmt::Display for Entrypoint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toolchain_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_interface: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuntimeSpec {
    pub abi: RuntimeAbi,
    pub entrypoint: Entrypoint,
    #[serde(default = "default_artifact_format")]
    pub format: ArtifactFormat,
    /// The Component Model world. Omitted by legacy v0.4 command manifests.
    #[serde(default)]
    pub world: Option<ComponentWorld>,
}

fn default_artifact_format() -> ArtifactFormat {
    ArtifactFormat::CoreModule
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
        if self.runtime.entrypoint.as_str().trim().is_empty() {
            bail!("runtime entrypoint must not be empty");
        }
        let expected_format = match self.runtime.abi.as_str() {
            "wasi-preview1" => ArtifactFormat::CoreModule,
            "wasi-preview2" => ArtifactFormat::Component,
            _ => self.runtime.format,
        };
        if self.runtime.format != expected_format {
            bail!(
                "runtime ABI '{}' requires {} format",
                self.runtime.abi.as_str(),
                expected_format
            );
        }
        match self.runtime.world {
            Some(_) if self.runtime.abi.as_str() != "wasi-preview2" => {
                bail!("component world is only valid for wasi-preview2 artifacts")
            }
            Some(ComponentWorld::WasiCliCommand)
                if self.runtime.entrypoint.as_str() != WASI_PREVIEW2_ENTRYPOINT =>
            {
                bail!("wasi:cli/command world requires entrypoint {WASI_PREVIEW2_ENTRYPOINT}")
            }
            Some(ComponentWorld::WasiHttpProxy)
                if self.runtime.entrypoint.as_str() != WASI_HTTP_PROXY_WORLD =>
            {
                bail!("wasi:http/proxy world requires entrypoint {WASI_HTTP_PROXY_WORLD}")
            }
            None if self.runtime.abi.as_str() == "wasi-preview2"
                && self.runtime.entrypoint.as_str() != WASI_PREVIEW2_ENTRYPOINT =>
            {
                bail!("legacy wasi-preview2 manifests must use {WASI_PREVIEW2_ENTRYPOINT}")
            }
            None if self.runtime.abi.as_str() == "wasi-preview1"
                && self.runtime.entrypoint.as_str() != WASI_PREVIEW1_ENTRYPOINT =>
            {
                bail!("wasi-preview1 artifacts require entrypoint {WASI_PREVIEW1_ENTRYPOINT}")
            }
            _ => {}
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
                "unsupported runtime ABI '{}'; PitBox currently supports wasi-preview1 and wasi-preview2",
                self.runtime.abi.as_str()
            );
        }
        let expected = match self.runtime.abi.as_str() {
            "wasi-preview1" => WASI_PREVIEW1_ENTRYPOINT,
            "wasi-preview2" => match self.runtime.world {
                Some(ComponentWorld::WasiHttpProxy) => WASI_HTTP_PROXY_WORLD,
                _ => WASI_PREVIEW2_ENTRYPOINT,
            },
            _ => unreachable!(),
        };
        if self.runtime.entrypoint.as_str() != expected {
            bail!(
                "unsupported entrypoint '{}'; expected {expected}",
                self.runtime.entrypoint.as_str()
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
        let actual_format = detect_artifact_format(&path)?;
        if actual_format != self.runtime.format {
            bail!(
                "artifact format mismatch: manifest {}, actual {}",
                format_name(self.runtime.format),
                format_name(actual_format)
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

pub fn detect_artifact_format(path: &Path) -> Result<ArtifactFormat> {
    let bytes = fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.is_empty() {
        bail!("WASM artifact is empty: {}", path.display());
    }
    for payload in Parser::new(0).parse_all(&bytes) {
        if let Payload::Version { encoding, .. } =
            payload.with_context(|| format!("invalid WebAssembly artifact {}", path.display()))?
        {
            return Ok(match encoding {
                Encoding::Module => ArtifactFormat::CoreModule,
                Encoding::Component => ArtifactFormat::Component,
            });
        }
    }
    bail!(
        "WASM artifact has no WebAssembly version header: {}",
        path.display()
    )
}

fn format_name(format: ArtifactFormat) -> &'static str {
    match format {
        ArtifactFormat::CoreModule => "core-module",
        ArtifactFormat::Component => "component",
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
        value.runtime.abi = RuntimeAbi("wasi-future".into());
        assert!(value.validate().is_ok());
        assert!(value.validate_runtime_compatibility().is_err());
    }

    #[test]
    fn preview2_manifest_roundtrips_with_component_contract() {
        let mut value = manifest();
        value.runtime = RuntimeSpec {
            abi: RuntimeAbi::wasi_preview2(),
            entrypoint: Entrypoint::wasi_preview2(),
            format: ArtifactFormat::Component,
            world: Some(ComponentWorld::WasiCliCommand),
        };
        let decoded: ArtifactManifest = serde_json::from_str(&value.to_json().unwrap()).unwrap();
        assert_eq!(decoded, value);
        assert!(decoded.validate_runtime_compatibility().is_ok());
    }

    #[test]
    fn legacy_preview1_manifest_defaults_to_core_module_format() {
        let value = serde_json::json!({
            "schema_version": 1,
            "artifact": { "name": "hello", "path": "build/hello.wasm", "sha256": "a".repeat(64), "size_bytes": 1 },
            "build": { "language": "rust", "target": "wasm32-wasip1", "profile": "release", "fingerprint": "b".repeat(64) },
            "runtime": { "abi": "wasi-preview1", "entrypoint": "_start" },
            "execution": {},
            "capabilities": ["stdio"]
        });
        let decoded: ArtifactManifest = serde_json::from_value(value).unwrap();
        assert_eq!(decoded.runtime.format, ArtifactFormat::CoreModule);
        assert!(decoded.validate_runtime_compatibility().is_ok());
    }

    #[test]
    fn runtime_contract_rejects_abi_format_mismatch() {
        let mut value = manifest();
        value.runtime.abi = RuntimeAbi::wasi_preview2();
        assert!(value.validate().is_err());
    }

    #[test]
    fn atomic_manifest_roundtrip_verifies_sha256_and_size() {
        let root = std::env::temp_dir().join(format!("pit-artifact-{}", std::process::id()));
        let artifact_path = root.join(".pit/build/hello.wasm");
        std::fs::create_dir_all(artifact_path.parent().unwrap()).unwrap();
        std::fs::write(&artifact_path, [0, 97, 115, 109, 1, 0, 0, 0]).unwrap();
        let mut value = manifest();
        value.artifact.path = "build/hello.wasm".into();
        value.artifact.sha256 = sha256_file(&artifact_path).unwrap();
        value.artifact.size_bytes = 8;
        let manifest_path = root.join(".pit/artifact.json");
        value.write_atomic(&manifest_path).unwrap();
        let loaded = ArtifactManifest::load(&manifest_path).unwrap();
        assert_eq!(loaded.verify_artifact(&root).unwrap(), artifact_path);
        let mut p2_claim = loaded.clone();
        p2_claim.runtime = RuntimeSpec {
            abi: RuntimeAbi::wasi_preview2(),
            entrypoint: Entrypoint::wasi_preview2(),
            format: ArtifactFormat::Component,
            world: Some(ComponentWorld::WasiCliCommand),
        };
        assert!(p2_claim.verify_artifact(&root).is_err());
        std::fs::write(&artifact_path, b"changed").unwrap();
        assert!(loaded.verify_artifact(&root).is_err());
        let _ = std::fs::remove_dir_all(root);
    }
}
