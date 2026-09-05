//! Rust Cargo builder adapter for PitCrew.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use pit_crew::{BuildArtifact, BuildProfile, BuildRequest, BuilderAdapter, Language};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::process::Command;

const TARGET: &str = "wasm32-wasip1";

#[derive(Debug, Default, Clone, Copy)]
pub struct RustBuilder;

impl RustBuilder {
    pub fn new() -> Self {
        Self
    }

    pub async fn target_available() -> Result<bool> {
        let output = Command::new("rustup")
            .args(["target", "list", "--installed"])
            .output()
            .await
            .context("failed to run rustup; install rustup and the Rust WASI target manually")?;
        if !output.status.success() {
            return Ok(false);
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .any(|line| line.trim() == TARGET))
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

    async fn build_project(&self, request: &BuildRequest, name: &str) -> Result<PathBuf> {
        if !Self::target_available().await? {
            bail!(
                "Rust WASI target wasm32-wasip1 is not installed.\n\nInstall it with:\n\nrustup target add wasm32-wasip1"
            );
        }

        let mut command = Command::new("cargo");
        command
            .arg("build")
            .arg("--target")
            .arg(TARGET)
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
        let cargo_artifact = metadata
            .target_directory
            .join(TARGET)
            .join(request.profile.cargo_directory())
            .join(format!("{name}.wasm"));
        if !cargo_artifact.is_file() {
            bail!(
                "cargo build succeeded but expected WASM artifact was not found: {}",
                cargo_artifact.display()
            );
        }
        Ok(cargo_artifact)
    }
}

#[async_trait]
impl BuilderAdapter for RustBuilder {
    fn language(&self) -> Language {
        Language::Rust
    }

    fn detect(&self, project_dir: &Path) -> bool {
        project_dir.join("Cargo.toml").is_file()
    }

    async fn build(&self, request: &BuildRequest) -> Result<BuildArtifact> {
        let metadata = Self::metadata(&request.project_dir).await?;
        let binaries = metadata
            .packages
            .iter()
            .flat_map(|package| {
                package
                    .targets
                    .iter()
                    .filter(|target| target.kind.iter().any(|kind| kind == "bin"))
                    .map(|target| BinaryTarget {
                        name: target.name.clone(),
                    })
            })
            .collect::<Vec<_>>();

        let name = match request.bin.as_deref() {
            Some(name) => {
                if !binaries.iter().any(|binary| binary.name == name) {
                    bail!(
                        "binary target '{name}' was not found; available binaries: {}",
                        available_binaries(&binaries)
                    );
                }
                name.to_owned()
            }
            None => match binaries.as_slice() {
                [binary] => binary.name.clone(),
                [] => bail!("Rust project has no runnable binary target"),
                _ => bail!(
                    "Rust project has multiple binary targets ({}); use pit build --bin <name>",
                    available_binaries(&binaries)
                ),
            },
        };

        let cargo_artifact = self.build_project(request, &name).await?;
        let output_dir = request.project_dir.join(".pit/build");
        tokio::fs::create_dir_all(&output_dir)
            .await
            .with_context(|| format!("failed to create {}", output_dir.display()))?;
        let artifact_path = output_dir.join(format!("{name}.wasm"));
        tokio::fs::copy(&cargo_artifact, &artifact_path)
            .await
            .with_context(|| {
                format!(
                    "failed to copy {} to {}",
                    cargo_artifact.display(),
                    artifact_path.display()
                )
            })?;

        validate_wasm(&artifact_path).await?;
        let bytes = tokio::fs::read(&artifact_path).await?;
        let size_bytes = bytes.len() as u64;
        let sha256 = format!("{:x}", Sha256::digest(&bytes));
        Ok(BuildArtifact {
            name,
            language: Language::Rust,
            target: TARGET.to_owned(),
            profile: request.profile,
            artifact_path,
            sha256,
            size_bytes,
        })
    }
}

#[derive(Debug, Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    target_directory: PathBuf,
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

#[derive(Debug, Clone)]
struct BinaryTarget {
    name: String,
}

fn available_binaries(binaries: &[BinaryTarget]) -> String {
    binaries
        .iter()
        .map(|binary| binary.name.as_str())
        .collect::<Vec<_>>()
        .join(", ")
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

pub async fn validate_wasm(path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    let bytes = tokio::fs::read(path)
        .await
        .with_context(|| format!("failed to read WASM artifact {}", path.display()))?;
    if bytes.is_empty() {
        bail!("WASM artifact is empty: {}", path.display());
    }
    for payload in wasmparser::Parser::new(0).parse_all(&bytes) {
        payload.with_context(|| format!("invalid WebAssembly artifact {}", path.display()))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{RustBuilder, TARGET, validate_wasm};
    use pit_crew::{BuildRequest, BuilderAdapter};
    use std::path::Path;

    #[test]
    fn detects_only_cargo_projects() {
        let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        assert!(RustBuilder::new().detect(&fixture));
        assert!(!RustBuilder::new().detect(Path::new("/definitely/not/a/project")));
        assert_eq!(TARGET, "wasm32-wasip1");
    }

    #[tokio::test]
    async fn rejects_invalid_wasm() {
        let path = std::env::temp_dir().join(format!("pit-invalid-{}.wasm", std::process::id()));
        tokio::fs::write(&path, b"not wasm").await.unwrap();
        assert!(validate_wasm(&path).await.is_err());
        let _ = tokio::fs::remove_file(path).await;
    }

    #[tokio::test]
    async fn builds_fixture_when_target_is_installed() {
        if !RustBuilder::target_available().await.unwrap_or(false) {
            return;
        }
        let project = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/rust-hello");
        let artifact = RustBuilder::new()
            .build(&BuildRequest::new(project))
            .await
            .unwrap();
        assert_eq!(artifact.name, "rust-hello");
        assert!(artifact.artifact_path.is_file());
        assert!(artifact.size_bytes > 8);
    }
}
