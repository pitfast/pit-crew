//! Rust Cargo builder adapter for PitCrew.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use pit_artifact::{
    ArtifactFormat, ArtifactManifest, ArtifactSpec, BuildProfile, BuildSpec, Capability,
    ComponentWorld, Entrypoint, RuntimeAbi, RuntimeSpec, SCHEMA_VERSION,
};
use pit_crew::{BuildArtifact, BuildRequest, BuilderAdapter};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;

const BUILDER_CONTRACT: &str = "pit-builder-rust-v1";

fn effective_world(request: &BuildRequest) -> Option<ComponentWorld> {
    match request.abi.as_str() {
        "wasi-preview2" => Some(request.world.unwrap_or(ComponentWorld::WasiCliCommand)),
        _ => None,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct RustBuilder;

impl RustBuilder {
    pub fn new() -> Self {
        Self
    }

    pub async fn target_available_for(abi: &RuntimeAbi) -> Result<bool> {
        let target = abi
            .target()
            .ok_or_else(|| anyhow::anyhow!("unsupported Rust WASI ABI '{}'", abi.as_str()))?;
        let output = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .await
            .context("failed to run rustup; install the Rust WASI target manually")?;
        Ok(output.status.success()
            && String::from_utf8_lossy(&output.stdout)
                .lines()
                .any(|line| line.trim() == target))
    }

    async fn metadata(project_dir: &Path) -> Result<CargoMetadata> {
        let manifest = project_dir.join("Cargo.toml");
        let output = Command::new("cargo")
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--manifest-path",
            ])
            .arg(&manifest)
            .current_dir(project_dir)
            .output()
            .await
            .context("failed to run cargo metadata")?;
        if !output.status.success() {
            bail!(
                "cargo metadata failed:\n{}",
                compiler_output(&output.stdout, &output.stderr)
            );
        }
        serde_json::from_slice(&output.stdout).context("cargo metadata returned invalid JSON")
    }

    fn select_binary(metadata: &CargoMetadata, requested: Option<&str>) -> Result<String> {
        let binaries = metadata
            .packages
            .iter()
            .flat_map(|package| {
                package
                    .targets
                    .iter()
                    .filter(|target| {
                        target
                            .kind
                            .iter()
                            .any(|kind| kind == "bin" || kind == "cdylib")
                    })
                    .map(|target| target.name.as_str())
            })
            .collect::<Vec<_>>();
        match requested {
            Some(name) if binaries.contains(&name) => Ok(name.to_owned()),
            Some(name) => bail!(
                "binary target '{name}' was not found; available binaries: {}",
                binaries.join(", ")
            ),
            None => match binaries.as_slice() {
                [name] => Ok((*name).to_owned()),
                [] => bail!("Rust project has no runnable binary target"),
                _ => bail!(
                    "Rust project has multiple binary targets ({}); use pit build --bin <name>",
                    binaries.join(", ")
                ),
            },
        }
    }

    fn fingerprint_files(root: &Path) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();
        collect_fingerprint_files(root, root, &mut files)?;
        files.sort();
        Ok(files)
    }

    async fn build_project(&self, request: &BuildRequest, name: &str) -> Result<PathBuf> {
        let target = request.abi.target().ok_or_else(|| {
            anyhow::anyhow!("unsupported Rust WASI ABI '{}'", request.abi.as_str())
        })?;
        if !Self::target_available_for(&request.abi).await? {
            bail!(
                "Rust WASI target {target} is not installed.\n\nInstall it with:\n\nrustup target add {target}"
            );
        }
        let mut command = Command::new("cargo");
        command
            .arg("build")
            .arg("--target")
            .arg(target)
            .current_dir(&request.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if request.profile == BuildProfile::Release {
            command.arg("--release");
        }
        if let Some(bin) = &request.bin {
            command.arg("--bin").arg(bin);
        }
        let output = command
            .output()
            .await
            .context("failed to run cargo build")?;
        if !output.status.success() {
            bail!(
                "cargo build failed with {}:\n{}",
                output.status,
                compiler_output(&output.stdout, &output.stderr)
            );
        }

        let metadata = Self::metadata(&request.project_dir).await?;
        let base = metadata
            .target_directory
            .join(target)
            .join(request.profile.cargo_directory())
            .join(format!("{name}.wasm"));
        let path = if base.is_file() {
            base
        } else {
            metadata
                .target_directory
                .join(target)
                .join(request.profile.cargo_directory())
                .join(format!("{}.wasm", name.replace('-', "_")))
        };
        if !path.is_file() {
            bail!(
                "cargo build succeeded but expected WASM artifact was not found: {}",
                path.display()
            );
        }
        Ok(path)
    }
}

#[async_trait]
impl BuilderAdapter for RustBuilder {
    fn language(&self) -> &str {
        "rust"
    }

    fn detect(&self, project_dir: &Path) -> bool {
        project_dir.join("Cargo.toml").is_file()
    }

    async fn fingerprint(&self, request: &BuildRequest) -> Result<String> {
        let metadata = Self::metadata(&request.project_dir).await?;
        let name = Self::select_binary(&metadata, request.bin.as_deref())?;
        let mut hasher = Sha256::new();
        let target = request.abi.target().ok_or_else(|| {
            anyhow::anyhow!("unsupported Rust WASI ABI '{}'", request.abi.as_str())
        })?;
        for value in [
            BUILDER_CONTRACT,
            self.language(),
            target,
            request.abi.as_str(),
            request.profile.as_str(),
            name.as_str(),
        ] {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        if let Some(world) = effective_world(request) {
            hasher.update(world.as_str().as_bytes());
            hasher.update([0]);
        }
        for path in Self::fingerprint_files(&metadata.workspace_root)? {
            hasher.update(path.to_string_lossy().replace('\\', "/").as_bytes());
            hasher.update([0]);
            hasher.update(std::fs::read(metadata.workspace_root.join(&path))?);
            hasher.update([0]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildArtifact> {
        let metadata = Self::metadata(&request.project_dir).await?;
        let name = Self::select_binary(&metadata, request.bin.as_deref())?;
        let cargo_artifact = self.build_project(request, &name).await?;
        let pit_dir = request.project_dir.join(".pit");
        let staging_dir = pit_dir.join("tmp");
        let build_dir = pit_dir.join("build");
        tokio::fs::create_dir_all(&staging_dir).await?;
        tokio::fs::create_dir_all(&build_dir).await?;

        let staging_path = staging_dir.join(format!("{name}.{}.wasm", std::process::id()));
        let artifact_path = build_dir.join(format!("{name}.wasm"));
        tokio::fs::copy(&cargo_artifact, &staging_path)
            .await
            .with_context(|| format!("failed to stage {}", cargo_artifact.display()))?;
        validate_wasm(&staging_path, &request.abi).await?;
        let bytes = tokio::fs::read(&staging_path).await?;
        let manifest = ArtifactManifest {
            schema_version: SCHEMA_VERSION,
            artifact: ArtifactSpec {
                name: name.clone(),
                path: PathBuf::from("build").join(format!("{name}.wasm")),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size_bytes: bytes.len() as u64,
            },
            build: BuildSpec {
                language: self.language().to_owned(),
                target: request.abi.target().unwrap_or_default().to_owned(),
                profile: request.profile,
                fingerprint: fingerprint.to_owned(),
            },
            runtime: runtime_spec(&request.abi, effective_world(request)),
            execution: request.execution_defaults.clone(),
            capabilities: vec![Capability::stdio(), Capability::args(), Capability::env()],
        };
        manifest.validate()?;
        tokio::fs::rename(&staging_path, &artifact_path)
            .await
            .with_context(|| format!("failed to promote {}", artifact_path.display()))?;
        Ok(BuildArtifact {
            manifest,
            artifact_path,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    target_directory: PathBuf,
    workspace_root: PathBuf,
}

#[derive(Debug, Deserialize)]
struct CargoPackage {
    targets: Vec<CargoTarget>,
}

#[derive(Debug, Deserialize)]
struct CargoTarget {
    name: String,
    kind: Vec<String>,
}

fn collect_fingerprint_files(root: &Path, current: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in std::fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if path.is_dir() {
            if matches!(name.as_ref(), "target" | ".pit" | ".git") {
                continue;
            }
            collect_fingerprint_files(root, &path, files)?;
        } else if path.is_file()
            && (name == "Cargo.toml"
                || name == "Cargo.lock"
                || name == "rust-toolchain"
                || name == "rust-toolchain.toml"
                || path.extension().is_some_and(|extension| extension == "rs"))
        {
            files.push(path.strip_prefix(root)?.to_path_buf());
        }
    }
    Ok(())
}

fn compiler_output(stdout: &[u8], stderr: &[u8]) -> String {
    let stdout = String::from_utf8_lossy(stdout).trim().to_owned();
    let stderr = String::from_utf8_lossy(stderr).trim().to_owned();
    match (stdout.is_empty(), stderr.is_empty()) {
        (true, true) => "(compiler produced no output)".to_owned(),
        (true, false) => stderr,
        (false, true) => stdout,
        (false, false) => format!("{stdout}\n{stderr}"),
    }
}

pub async fn validate_wasm(path: impl AsRef<Path>, abi: &RuntimeAbi) -> Result<()> {
    let path = path.as_ref();
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read WASM artifact {}", path.display()))?;
    if bytes.is_empty() {
        bail!("WASM artifact is empty: {}", path.display());
    }
    let actual = pit_artifact::detect_artifact_format(path)?;
    let expected = match abi.as_str() {
        "wasi-preview1" => ArtifactFormat::CoreModule,
        "wasi-preview2" => ArtifactFormat::Component,
        _ => bail!("unsupported Rust WASI ABI '{}'", abi.as_str()),
    };
    if actual != expected {
        bail!("WASM format mismatch: expected {expected:?}, found {actual:?}");
    }
    Ok(())
}

fn runtime_spec(abi: &RuntimeAbi, world: Option<ComponentWorld>) -> RuntimeSpec {
    match abi.as_str() {
        "wasi-preview1" => RuntimeSpec {
            abi: abi.clone(),
            entrypoint: Entrypoint::wasi_preview1(),
            format: ArtifactFormat::CoreModule,
            world: None,
        },
        "wasi-preview2" => RuntimeSpec {
            abi: abi.clone(),
            entrypoint: match world {
                Some(ComponentWorld::WasiHttpProxy) => Entrypoint::WasiHttpProxy,
                _ => Entrypoint::wasi_preview2(),
            },
            format: ArtifactFormat::Component,
            world,
        },
        _ => RuntimeSpec {
            abi: abi.clone(),
            entrypoint: Entrypoint::Custom(String::new()),
            format: ArtifactFormat::CoreModule,
            world: None,
        },
    }
}

pub async fn fingerprint_project(request: &BuildRequest) -> Result<String> {
    RustBuilder::new().fingerprint(request).await
}

#[cfg(test)]
mod tests {
    use super::{RustBuilder, fingerprint_project, validate_wasm};
    use pit_crew::{BuildRequest, BuilderAdapter};
    use std::path::Path;

    #[test]
    fn detects_only_cargo_projects() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        assert!(RustBuilder::new().detect(&fixture));
        assert!(!RustBuilder::new().detect(Path::new("/definitely/not/a/project")));
        assert!(pit_artifact::RuntimeAbi::wasi_preview1().target().is_some());
    }

    #[tokio::test]
    async fn rejects_invalid_wasm() {
        let path = std::env::temp_dir().join(format!("pit-invalid-{}.wasm", std::process::id()));
        tokio::fs::write(&path, b"not wasm").await.unwrap();
        assert!(
            validate_wasm(&path, &pit_artifact::RuntimeAbi::wasi_preview1())
                .await
                .is_err()
        );
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn fingerprint_ignores_build_directories_and_changes_with_source() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        let request = BuildRequest::new(&project);
        let first = fingerprint_project(&request).await.unwrap();
        tokio::fs::write(project.join("ignored.txt"), "ignored")
            .await
            .unwrap();
        let second = fingerprint_project(&request).await.unwrap();
        assert_eq!(first, second);
        let source = project.join("src/main.rs");
        let original = tokio::fs::read_to_string(&source).await.unwrap();
        tokio::fs::write(&source, format!("{original}\n"))
            .await
            .unwrap();
        let third = fingerprint_project(&request).await.unwrap();
        assert_ne!(second, third);
        tokio::fs::write(source, original).await.unwrap();
        let _ = tokio::fs::remove_file(project.join("ignored.txt")).await;
    }

    #[tokio::test]
    async fn fingerprint_separates_preview1_and_preview2() {
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        let p2 = BuildRequest::new(&project);
        let mut p1 = p2.clone();
        p1.abi = pit_artifact::RuntimeAbi::wasi_preview1();
        assert_ne!(
            fingerprint_project(&p1).await.unwrap(),
            fingerprint_project(&p2).await.unwrap()
        );
    }

    #[tokio::test]
    async fn builds_fixture_when_target_is_installed() {
        if !RustBuilder::target_available_for(&pit_artifact::RuntimeAbi::wasi_preview1())
            .await
            .unwrap_or(false)
        {
            return;
        }
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        let request = BuildRequest::new(project);
        let builder = RustBuilder::new();
        let fingerprint = builder.fingerprint(&request).await.unwrap();
        let artifact = builder.build(&request, &fingerprint).await.unwrap();
        assert_eq!(artifact.manifest.artifact.name, "rust-hello");
        assert!(artifact.artifact_path.is_file());
    }
}
