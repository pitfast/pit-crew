//! Go Component builder using the official Bytecode Alliance componentize-go tool.

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

const BUILDER_CONTRACT: &str = "pit-builder-go-v1";

#[derive(Debug, Default, Clone, Copy)]
pub struct GoBuilder;

impl GoBuilder {
    pub fn new() -> Self {
        Self
    }

    fn tool() -> String {
        std::env::var("PITFAST_COMPONENTIZE_GO").unwrap_or_else(|_| "componentize-go".into())
    }

    fn go_tool() -> Option<String> {
        std::env::var("PITFAST_GO").ok()
    }

    async fn output(command: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(command)
            .args(args)
            .output()
            .await
            .with_context(|| {
                format!("failed to run {command}; install the Go Component toolchain")
            })?;
        if !output.status.success() {
            bail!(
                "{command} probe failed: {}",
                text(&output.stdout, &output.stderr)
            );
        }
        Ok(text(&output.stdout, &output.stderr))
    }

    async fn toolchain_for(&self, request: &BuildRequest) -> Result<ToolchainInfo> {
        let componentizer = Self::output(&Self::tool(), &["--version"]).await?;
        let go = Self::go_tool().unwrap_or_else(|| "go".into());
        let compiler = Self::output(&go, &["version"]).await?;
        Ok(ToolchainInfo {
            name: "componentize-go".into(),
            version: componentizer
                .lines()
                .next()
                .unwrap_or("componentize-go")
                .trim()
                .into(),
            compiler: Some(compiler.lines().next().unwrap_or("go").trim().into()),
            componentizer: Some(
                componentizer
                    .lines()
                    .next()
                    .unwrap_or("componentize-go")
                    .trim()
                    .into(),
            ),
            target: request.abi.target().unwrap_or("wasm32-wasip2").into(),
        })
    }

    fn files(root: &Path, current: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(current)? {
            let entry = entry?;
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if path.is_dir() {
                if matches!(name.as_ref(), ".git" | ".pit" | "target" | "vendor")
                    || name.starts_with("wasi_")
                {
                    continue;
                }
                Self::files(root, &path, result)?;
            } else if path.is_file()
                && name != "wit_exports.go"
                && (name == "go.mod"
                    || name == "go.sum"
                    || name == "componentize-go.toml"
                    || path.extension().is_some_and(|e| e == "go" || e == "wit"))
            {
                result.push(path.strip_prefix(root)?.to_path_buf());
            }
        }
        Ok(())
    }
}

#[async_trait]
impl LanguageBuilder for GoBuilder {
    fn language(&self) -> Language {
        Language::Go
    }

    fn detect(&self, project_dir: &Path) -> Detection {
        if project_dir.join("go.mod").is_file() {
            Detection::Yes
        } else {
            Detection::No
        }
    }

    async fn probe_toolchain(&self) -> Result<ToolchainInfo> {
        let request = BuildRequest::new(".");
        self.toolchain_for(&request).await
    }

    async fn fingerprint(&self, request: &BuildRequest) -> Result<String> {
        let toolchain = self.toolchain_for(request).await?;
        let mut hasher = Sha256::new();
        for value in [
            BUILDER_CONTRACT,
            self.language().as_str(),
            request.abi.as_str(),
            request.profile.as_str(),
            toolchain.version.as_str(),
            request
                .world
                .map(ComponentWorld::as_str)
                .unwrap_or(ComponentWorld::WasiHttpProxy.as_str()),
        ] {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        let mut files = Vec::new();
        Self::files(&request.project_dir, &request.project_dir, &mut files)?;
        files.sort();
        for path in files {
            hasher.update(path.to_string_lossy().replace('\\', "/").as_bytes());
            hasher.update([0]);
            hasher.update(std::fs::read(request.project_dir.join(path))?);
            hasher.update([0]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }

    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildOutput> {
        if request.abi.as_str() != "wasi-preview2" {
            bail!(
                "Go Component builder currently targets wasi-preview2; use the Rust builder for P1"
            );
        }
        let world = request.world.unwrap_or(ComponentWorld::WasiHttpProxy);
        let toolchain = self.toolchain_for(request).await?;
        let pit_dir = request.project_dir.join(".pit");
        let staging_dir = pit_dir.join("tmp");
        let build_dir = pit_dir.join("build");
        tokio::fs::create_dir_all(&staging_dir).await?;
        tokio::fs::create_dir_all(&build_dir).await?;
        let name = request
            .project_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("go-component")
            .replace('-', "_");
        let staging = staging_dir.join(format!("{name}.{}.wasm", std::process::id()));
        let artifact_path = build_dir.join(format!("{name}.wasm"));
        let mut bindings = Command::new(Self::tool());
        bindings
            .args(["--world", world.as_str(), "bindings", "--format"])
            .current_dir(&request.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let bindings_output = bindings
            .output()
            .await
            .context("failed to generate Go Component bindings")?;
        if !bindings_output.status.success() {
            bail!(
                "componentize-go bindings failed: {}",
                text(&bindings_output.stdout, &bindings_output.stderr)
            );
        }
        let mut command = Command::new(Self::tool());
        command
            .args(["--world", world.as_str(), "build", "--output"])
            .arg(&staging)
            .current_dir(&request.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(go) = Self::go_tool() {
            command.args(["--go", &go]);
        }
        let output = command
            .output()
            .await
            .context("failed to run componentize-go")?;
        if !output.status.success() {
            bail!(
                "componentize-go failed: {}",
                text(&output.stdout, &output.stderr)
            );
        }
        if detect_artifact_format(&staging)? != ArtifactFormat::Component {
            bail!("componentize-go produced a core module, expected a Component");
        }
        let bytes = tokio::fs::read(&staging).await?;
        let manifest = ArtifactManifest {
            schema_version: SCHEMA_VERSION,
            artifact: ArtifactSpec {
                name: name.clone(),
                path: PathBuf::from("build").join(format!("{name}.wasm")),
                sha256: format!("{:x}", Sha256::digest(&bytes)),
                size_bytes: bytes.len() as u64,
            },
            build: BuildSpec {
                language: self.language().to_string(),
                target: request.abi.target().unwrap_or("wasm32-wasip2").into(),
                profile: request.profile,
                fingerprint: fingerprint.into(),
                toolchain: Some("componentize-go".into()),
                toolchain_version: Some(toolchain.version.clone()),
            },
            runtime: RuntimeSpec {
                abi: RuntimeAbi::wasi_preview2(),
                entrypoint: if world == ComponentWorld::WasiHttpProxy {
                    Entrypoint::WasiHttpProxy
                } else {
                    Entrypoint::wasi_preview2()
                },
                format: ArtifactFormat::Component,
                world: Some(world),
            },
            execution: request.execution_defaults.clone(),
            capabilities: vec![Capability::stdio(), Capability::args(), Capability::env()],
        };
        manifest.validate()?;
        tokio::fs::rename(&staging, &artifact_path).await?;
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
    use super::GoBuilder;
    use pit_builder_core::{Detection, Language, LanguageBuilder};
    use std::path::Path;

    #[test]
    fn detects_go_modules() {
        assert_eq!(
            GoBuilder::new().detect(Path::new("/definitely/not/go")),
            Detection::No
        );
        assert_eq!(GoBuilder::new().language(), Language::Go);
    }
}
