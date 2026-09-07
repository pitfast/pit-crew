//! JavaScript and TypeScript builder backed by ComponentizeJS.

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

const BUILDER_CONTRACT: &str = "pit-builder-js-v1";

#[derive(Debug, Clone, Copy)]
pub struct JsBuilder {
    language: Language,
}

impl JsBuilder {
    pub fn javascript() -> Self {
        Self {
            language: Language::JavaScript,
        }
    }
    pub fn typescript() -> Self {
        Self {
            language: Language::TypeScript,
        }
    }

    fn node() -> String {
        std::env::var("PITFAST_NODE").unwrap_or_else(|_| "node".into())
    }
    fn tsc() -> String {
        std::env::var("PITFAST_TSC").unwrap_or_else(|_| "tsc".into())
    }
    fn script() -> Option<String> {
        std::env::var("PITFAST_COMPONENTIZE_JS_SCRIPT").ok()
    }
    fn wit(request: &BuildRequest) -> Result<PathBuf> {
        let path = request.project_dir.join("wit");
        if path.is_dir() {
            Ok(path)
        } else {
            std::env::var_os("PITFAST_HTTP_WIT")
                .map(PathBuf::from)
                .filter(|p| p.exists())
                .ok_or_else(|| {
                    anyhow::anyhow!("JavaScript builder needs project/wit or PITFAST_HTTP_WIT")
                })
        }
    }
    fn source(&self, root: &Path) -> Result<PathBuf> {
        let path = match self.language {
            Language::TypeScript => root.join("main.ts"),
            _ => root.join("main.js"),
        };
        if path.is_file() {
            Ok(path)
        } else {
            bail!("{} project must contain {}", self.language, path.display())
        }
    }
    async fn output(command: &str, args: &[&str]) -> Result<String> {
        let out = Command::new(command)
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to run {command}"))?;
        if !out.status.success() {
            bail!("{command} failed: {}", text(&out.stdout, &out.stderr));
        }
        Ok(text(&out.stdout, &out.stderr))
    }
    async fn toolchain_for(&self, request: &BuildRequest) -> Result<ToolchainInfo> {
        let node = Self::output(&Self::node(), &["--version"]).await?;
        let componentizer = if let Some(script) = Self::script() {
            Self::output(&Self::node(), &[&script, "--version"])
                .await
                .unwrap_or_else(|_| "ComponentizeJS 0.22.0".into())
        } else {
            Self::output("componentize-js", &["--version"]).await?
        };
        Ok(ToolchainInfo {
            name: "componentize-js".into(),
            version: componentizer
                .lines()
                .next()
                .unwrap_or("componentize-js")
                .trim()
                .into(),
            compiler: Some(node.lines().next().unwrap_or("node").trim().into()),
            componentizer: Some(
                componentizer
                    .lines()
                    .next()
                    .unwrap_or("componentize-js")
                    .trim()
                    .into(),
            ),
            target: request.abi.target().unwrap_or("wasm32-wasip2").into(),
        })
    }
    fn files(root: &Path, result: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(root)? {
            let path = entry?.path();
            if path.is_dir() {
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default();
                if !matches!(name, ".git" | ".pit" | "node_modules" | "target") {
                    Self::files(&path, result)?;
                }
            } else if path.is_file() {
                let name = path
                    .file_name()
                    .and_then(|v| v.to_str())
                    .unwrap_or_default();
                if matches!(
                    name,
                    "package.json" | "package-lock.json" | "pnpm-lock.yaml" | "main.js" | "main.ts"
                ) || path.extension().is_some_and(|e| e == "wit")
                {
                    result.push(path);
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl LanguageBuilder for JsBuilder {
    fn language(&self) -> Language {
        self.language
    }
    fn detect(&self, project_dir: &Path) -> Detection {
        let marker = project_dir.join("package.json").is_file()
            && match self.language {
                Language::TypeScript => project_dir.join("main.ts").is_file(),
                _ => {
                    project_dir.join("main.js").is_file() && !project_dir.join("main.ts").is_file()
                }
            };
        if marker {
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
        let mut hasher = Sha256::new();
        for value in [
            BUILDER_CONTRACT,
            self.language.as_str(),
            request.abi.as_str(),
            request.profile.as_str(),
            toolchain.version.as_str(),
            ComponentWorld::WasiHttpProxy.as_str(),
        ] {
            hasher.update(value.as_bytes());
            hasher.update([0]);
        }
        let mut files = Vec::new();
        Self::files(&request.project_dir, &mut files)?;
        files.sort();
        for path in files {
            hasher.update(
                path.strip_prefix(&request.project_dir)?
                    .to_string_lossy()
                    .as_bytes(),
            );
            hasher.update([0]);
            hasher.update(std::fs::read(path)?);
            hasher.update([0]);
        }
        Ok(format!("{:x}", hasher.finalize()))
    }
    async fn build(&self, request: &BuildRequest, fingerprint: &str) -> Result<BuildOutput> {
        if request.abi.as_str() != "wasi-preview2" {
            bail!(
                "{} builder targets wasi-preview2 Components only",
                self.language
            );
        }
        let world = request.world.unwrap_or(ComponentWorld::WasiHttpProxy);
        if world != ComponentWorld::WasiHttpProxy {
            bail!("{} builder supports wasi:http/proxy only", self.language);
        }
        let wit = Self::wit(request)?;
        let source = self.source(&request.project_dir)?;
        let toolchain = self.toolchain_for(request).await?;
        let pit_dir = request.project_dir.join(".pit");
        let generated = pit_dir.join("generated");
        let build_dir = pit_dir.join("build");
        tokio::fs::create_dir_all(&generated).await?;
        tokio::fs::create_dir_all(&build_dir).await?;
        let js_source = if self.language == Language::TypeScript {
            let mut tsc = Command::new(Self::tsc());
            tsc.args([
                source.to_str().unwrap_or("main.ts"),
                "--target",
                "ES2022",
                "--module",
                "ES2022",
                "--moduleResolution",
                "node",
                "--lib",
                "ESNext,DOM",
                "--skipLibCheck",
            ]);
            let declarations = request.project_dir.join("wasi-http.d.ts");
            if declarations.is_file() {
                tsc.arg(&declarations);
            }
            let output = tsc
                .args(["--outDir"])
                .arg(&generated)
                .output()
                .await
                .context("failed to run TypeScript compiler")?;
            if !output.status.success() {
                bail!(
                    "TypeScript compilation failed: {}",
                    text(&output.stdout, &output.stderr)
                );
            }
            generated.join("main.js")
        } else {
            source.clone()
        };
        let name = request
            .project_dir
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("javascript-component")
            .replace('-', "_");
        let artifact_path = build_dir.join(format!("{name}.wasm"));
        let mut command = if let Some(script) = Self::script() {
            let mut c = Command::new(Self::node());
            c.arg(script);
            c
        } else {
            Command::new("componentize-js")
        };
        command
            .arg(&js_source)
            .args(["--wit"])
            .arg(&wit)
            .args(["--world-name", "proxy", "--out"])
            .arg(&artifact_path)
            .current_dir(&request.project_dir)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let output = command
            .output()
            .await
            .context("failed to run ComponentizeJS")?;
        if !output.status.success() {
            bail!(
                "ComponentizeJS failed: {}",
                text(&output.stdout, &output.stderr)
            );
        }
        if detect_artifact_format(&artifact_path)? != ArtifactFormat::Component {
            bail!("ComponentizeJS produced a core module, expected a Component");
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
                language: self.language.to_string(),
                target: "wasm32-wasip2".into(),
                profile: request.profile,
                fingerprint: fingerprint.into(),
                toolchain: Some("componentize-js".into()),
                toolchain_version: Some(toolchain.version.clone()),
                application_interface: None,
                adapter: None,
                adapter_digest: None,
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
    use super::JsBuilder;
    use pit_builder_core::{Detection, Language, LanguageBuilder};
    use std::path::Path;
    #[test]
    fn detects_js_and_ts() {
        assert_eq!(JsBuilder::javascript().language(), Language::JavaScript);
        assert_eq!(JsBuilder::typescript().language(), Language::TypeScript);
        assert_eq!(
            JsBuilder::javascript().detect(Path::new("/missing")),
            Detection::No
        );
    }
}
