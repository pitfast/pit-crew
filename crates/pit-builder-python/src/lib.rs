//! Python Component builder.  The Python interpreter is embedded by
//! componentize-py; PitBox never launches a host Python process.

use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use pit_artifact::{
    ArtifactFormat, ArtifactManifest, ArtifactSpec, BuildSpec, Capability, ComponentWorld,
    Entrypoint, RuntimeAbi, RuntimeSpec, SCHEMA_VERSION, detect_artifact_format,
};
use pit_builder_core::{
    BuildOutput, BuildRequest, Detection, Language, LanguageBuilder, ToolchainInfo,
};
use sha2::{Digest, Sha256};
use tokio::process::Command;

const BUILDER_CONTRACT: &str = "pit-builder-python-v1";

#[derive(Debug, Default, Clone, Copy)]
pub struct PythonBuilder;

impl PythonBuilder {
    pub fn new() -> Self {
        Self
    }
    fn tool() -> String {
        std::env::var("PITFAST_COMPONENTIZE_PY").unwrap_or_else(|_| "componentize-py".into())
    }
    fn python() -> String {
        std::env::var("PITFAST_PYTHON").unwrap_or_else(|_| "python3".into())
    }
    fn wit(request: &BuildRequest) -> Result<PathBuf> {
        let path = request.project_dir.join("wit");
        if path.is_dir() {
            Ok(path)
        } else {
            bail!("Python Component projects need a wit/ directory")
        }
    }
    async fn output(command: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(command)
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to run {command}"))?;
        if !output.status.success() {
            bail!("{command} failed: {}", text(&output.stdout, &output.stderr));
        }
        Ok(text(&output.stdout, &output.stderr))
    }
    async fn toolchain_for(&self, request: &BuildRequest) -> Result<ToolchainInfo> {
        let version = Self::output(&Self::tool(), &["--version"]).await?;
        let python = Self::output(&Self::python(), &["--version"]).await?;
        Ok(ToolchainInfo {
            name: "componentize-py".into(),
            version: version
                .lines()
                .next()
                .unwrap_or("componentize-py")
                .trim()
                .into(),
            compiler: Some(python.lines().next().unwrap_or("python").trim().into()),
            componentizer: Some(
                version
                    .lines()
                    .next()
                    .unwrap_or("componentize-py")
                    .trim()
                    .into(),
            ),
            target: request.abi.target().unwrap_or("wasm32-wasip2").into(),
        })
    }
    fn files(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default();
                if !matches!(name, ".git" | ".pit" | "__pycache__" | "venv") {
                    Self::files(&path, out)?;
                }
            } else if path.is_file()
                && path
                    .extension()
                    .is_some_and(|e| e == "py" || e == "wit" || e == "toml")
            {
                out.push(path);
            }
        }
        Ok(())
    }
}

#[async_trait]
impl LanguageBuilder for PythonBuilder {
    fn language(&self) -> Language {
        Language::Python
    }
    fn detect(&self, project_dir: &Path) -> Detection {
        if project_dir.join("pyproject.toml").is_file() || project_dir.join("app.py").is_file() {
            Detection::Yes
        } else {
            Detection::No
        }
    }
    async fn probe_toolchain(&self) -> Result<ToolchainInfo> {
        self.toolchain_for(&BuildRequest::new(".")).await
    }
    async fn fingerprint(&self, request: &BuildRequest) -> Result<String> {
        let toolchain = self.toolchain_for(request).await?;
        let mut h = Sha256::new();
        for value in [
            BUILDER_CONTRACT,
            "python",
            request.abi.as_str(),
            request.profile.as_str(),
            toolchain.version.as_str(),
            ComponentWorld::WasiHttpProxy.as_str(),
        ] {
            h.update(value.as_bytes());
            h.update([0]);
        }
        let mut files = Vec::new();
        Self::files(&request.project_dir, &mut files)?;
        files.sort();
        for p in files {
            h.update(
                p.strip_prefix(&request.project_dir)?
                    .to_string_lossy()
                    .as_bytes(),
            );
            h.update([0]);
            h.update(std::fs::read(p)?);
            h.update([0]);
        }
        Ok(format!("{:x}", h.finalize()))
    }
    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildOutput> {
        if request.abi.as_str() != "wasi-preview2" {
            bail!("Python builder targets wasi-preview2 Components only");
        }
        let world = request.world.unwrap_or(ComponentWorld::WasiHttpProxy);
        if world != ComponentWorld::WasiHttpProxy {
            bail!("Python builder supports wasi:http/proxy only");
        }
        let wit = Self::wit(request)?;
        let toolchain = self.toolchain_for(request).await?;
        let pit_dir = request.project_dir.join(".pit");
        let build = pit_dir.join("build");
        tokio::fs::create_dir_all(&build).await?;
        let name = request
            .project_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("python-component")
            .replace('-', "_");
        let artifact_path = build.join(format!("{name}.wasm"));
        let output = Command::new(Self::tool())
            .args(["-d"])
            .arg(&wit)
            .args(["-w", "wasi:http/proxy@0.2.0", "componentize", "app", "-p"])
            .arg(&request.project_dir)
            .args(["-o"])
            .arg(&artifact_path)
            .current_dir(&request.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await
            .context("failed to run componentize-py")?;
        if !output.status.success() {
            bail!(
                "componentize-py failed: {}",
                text(&output.stdout, &output.stderr)
            );
        }
        if detect_artifact_format(&artifact_path)? != ArtifactFormat::Component {
            bail!("componentize-py produced a core module, expected a Component");
        }
        let bytes = tokio::fs::read(&artifact_path).await?;
        let manifest = ArtifactManifest {
            schema_version: SCHEMA_VERSION,
            artifact: ArtifactSpec {
                name: name.clone(),
                path: PathBuf::from("build").join(format!("{name}.wasm")),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size_bytes: bytes.len() as u64,
            },
            build: BuildSpec {
                language: "python".into(),
                target: "wasm32-wasip2".into(),
                profile: request.profile,
                fingerprint: fingerprint.into(),
                toolchain: Some("componentize-py".into()),
                toolchain_version: Some(toolchain.version.clone()),
            },
            runtime: RuntimeSpec {
                abi: RuntimeAbi::wasi_preview2(),
                entrypoint: Entrypoint::WasiHttpProxy,
                format: ArtifactFormat::Component,
                world: Some(world),
            },
            execution: request.execution_defaults.clone(),
            capabilities: vec![Capability::stdio(), Capability::args(), Capability::env()],
        };
        manifest.validate()?;
        Ok(BuildOutput {
            manifest,
            artifact_path,
            toolchain,
        })
    }
}

fn text(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout).trim().to_owned();
    let err = String::from_utf8_lossy(stderr).trim().to_owned();
    if out.is_empty() {
        err
    } else if err.is_empty() {
        out
    } else {
        format!("{out}\n{err}")
    }
}

#[cfg(test)]
mod tests {
    use super::PythonBuilder;
    use pit_builder_core::{Detection, Language, LanguageBuilder};
    use std::path::Path;
    #[test]
    fn detects_python() {
        assert_eq!(PythonBuilder::new().language(), Language::Python);
        assert_eq!(
            PythonBuilder::new().detect(Path::new("/missing")),
            Detection::No
        );
    }
}
