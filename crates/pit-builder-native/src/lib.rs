//! WASI Component builders for C and C++.
//!
//! Both builders use the stable C ABI emitted by `wit-bindgen` for the
//! `wasi:http/proxy` world.  A C++ guest can include that ABI without using
//! the still-evolving C++ convenience bindings, while the generated artifact
//! remains an ordinary standards-compliant Component.

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
use std::path::{Path, PathBuf};
use tokio::process::Command;

const BUILDER_CONTRACT: &str = "pit-builder-native-v1";

#[derive(Debug, Clone, Copy)]
pub struct NativeBuilder {
    language: Language,
}

impl NativeBuilder {
    pub fn c() -> Self {
        Self {
            language: Language::C,
        }
    }

    pub fn cpp() -> Self {
        Self {
            language: Language::Cpp,
        }
    }

    fn sdk() -> Result<PathBuf> {
        std::env::var_os("PITFAST_WASI_SDK")
            .map(PathBuf::from)
            .filter(|path| path.is_dir())
            .ok_or_else(|| {
                anyhow::anyhow!("PITFAST_WASI_SDK must point to a WASI SDK installation")
            })
    }

    fn wit(request: &BuildRequest) -> Result<PathBuf> {
        let path = request
            .wit_path
            .clone()
            .or_else(|| {
                let path = request.project_dir.join("wit");
                path.is_dir().then_some(path)
            })
            .or_else(|| std::env::var_os("PITFAST_HTTP_WIT").map(PathBuf::from))
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "HTTP builder needs a WIT directory; set [build].wit or PITFAST_HTTP_WIT"
                )
            })?;
        if !path.exists() {
            bail!("WIT path does not exist: {}", path.display());
        }
        Ok(path)
    }

    fn compiler(sdk: &Path, language: Language) -> PathBuf {
        let name = match language {
            Language::C => "wasm32-wasip2-clang",
            Language::Cpp => "wasm32-wasip2-clang++",
            _ => unreachable!(),
        };
        sdk.join("bin").join(name)
    }

    fn bindgen() -> String {
        std::env::var("PITFAST_WIT_BINDGEN").unwrap_or_else(|_| "wit-bindgen".into())
    }

    fn wasm_tools() -> String {
        std::env::var("PITFAST_WASM_TOOLS").unwrap_or_else(|_| "wasm-tools".into())
    }

    async fn text(command: &str, args: &[&str]) -> Result<String> {
        let output = Command::new(command)
            .args(args)
            .output()
            .await
            .with_context(|| format!("failed to run {command}"))?;
        if !output.status.success() {
            bail!(
                "{command} failed: {}",
                output_text(&output.stdout, &output.stderr)
            );
        }
        Ok(output_text(&output.stdout, &output.stderr))
    }

    async fn toolchain_for(&self, request: &BuildRequest) -> Result<ToolchainInfo> {
        let sdk = Self::sdk()?;
        let compiler = Self::compiler(&sdk, self.language);
        if !compiler.is_file() {
            bail!("WASI SDK compiler is missing: {}", compiler.display());
        }
        let compiler_version =
            Self::text(compiler.to_str().unwrap_or_default(), &["--version"]).await?;
        let bindgen = Self::text(&Self::bindgen(), &["--version"]).await?;
        let wasm_tools = Self::text(&Self::wasm_tools(), &["--version"]).await?;
        Ok(ToolchainInfo {
            name: "wasi-sdk".into(),
            version: compiler_version
                .lines()
                .next()
                .unwrap_or("wasi-sdk")
                .trim()
                .into(),
            compiler: Some(
                compiler_version
                    .lines()
                    .next()
                    .unwrap_or("clang")
                    .trim()
                    .into(),
            ),
            componentizer: Some(format!(
                "{}; {}",
                bindgen.lines().next().unwrap_or("wit-bindgen").trim(),
                wasm_tools.lines().next().unwrap_or("wasm-tools").trim()
            )),
            target: request.abi.target().unwrap_or("wasm32-wasip2").into(),
        })
    }

    fn source(root: &Path, language: Language) -> Result<PathBuf> {
        let extensions: &[&str] = match language {
            Language::C => &["c"],
            Language::Cpp => &["cpp", "cc", "cxx"],
            _ => unreachable!(),
        };
        let mut sources = std::fs::read_dir(root)?
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.is_file()
                    && path
                        .extension()
                        .and_then(|extension| extension.to_str())
                        .is_some_and(|extension| extensions.contains(&extension))
            })
            .collect::<Vec<_>>();
        sources.sort();
        sources.into_iter().next().ok_or_else(|| {
            anyhow::anyhow!(
                "{} project must contain a {} source file",
                language,
                extensions.join(", ")
            )
        })
    }

    fn fingerprint_files(root: &Path, wit: &Path, source: &Path) -> Result<Vec<PathBuf>> {
        let mut files = vec![source.to_path_buf()];
        collect_files(wit, &mut files)?;
        files.sort();
        files.dedup();
        let _ = root;
        Ok(files)
    }
}

#[async_trait]
impl LanguageBuilder for NativeBuilder {
    fn language(&self) -> Language {
        self.language
    }

    fn detect(&self, project_dir: &Path) -> Detection {
        let marker = match self.language {
            Language::C => project_dir.join("main.c").is_file() || has_extension(project_dir, "c"),
            Language::Cpp => {
                project_dir.join("main.cpp").is_file()
                    || has_extension(project_dir, "cpp")
                    || has_extension(project_dir, "cc")
                    || has_extension(project_dir, "cxx")
            }
            _ => false,
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
        let wit = Self::wit(request)?;
        let source = Self::source(&request.project_dir, self.language)?;
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
        for path in Self::fingerprint_files(&request.project_dir, &wit, &source)? {
            hasher.update(path.to_string_lossy().replace('\\', "/").as_bytes());
            hasher.update([0]);
            hasher.update(std::fs::read(&path)?);
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
            bail!(
                "{} builder currently supports wasi:http/proxy only",
                self.language
            );
        }
        let sdk = Self::sdk()?;
        let wit = Self::wit(request)?;
        let source = Self::source(&request.project_dir, self.language)?;
        let toolchain = self.toolchain_for(request).await?;
        let pit_dir = request.project_dir.join(".pit");
        let generated = pit_dir.join("generated").join(self.language.as_str());
        let build_dir = pit_dir.join("build");
        tokio::fs::create_dir_all(&generated).await?;
        tokio::fs::create_dir_all(&build_dir).await?;
        let binding_language = "c";
        let bindings = Command::new(Self::bindgen())
            .args([binding_language])
            .args(["-w", world.as_str(), "--out-dir"])
            .arg(&generated)
            .arg(&wit)
            .output()
            .await
            .context("failed to run wit-bindgen")?;
        if !bindings.status.success() {
            bail!(
                "wit-bindgen failed: {}",
                output_text(&bindings.stdout, &bindings.stderr)
            );
        }
        let generated_source = generated.join("proxy.c");
        let generated_object = generated.join("bindings.o");
        let compiler_c = Self::compiler(&sdk, Language::C);
        let mut compile_bindings = Command::new(&compiler_c);
        compile_bindings
            .arg("-I")
            .arg(&generated)
            .arg("-c")
            .arg(&generated_source)
            .arg("-o")
            .arg(&generated_object);
        let output = compile_bindings
            .output()
            .await
            .context("failed to compile generated C bindings")?;
        if !output.status.success() {
            bail!(
                "generated C bindings failed: {}",
                output_text(&output.stdout, &output.stderr)
            );
        }
        let name = request
            .project_dir
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("native-component")
            .replace('-', "_");
        let core = build_dir.join(format!("{name}.core.wasm"));
        let artifact_path = build_dir.join(format!("{name}.wasm"));
        let compiler = Self::compiler(&sdk, self.language);
        let output = Command::new(&compiler)
            .arg("-I")
            .arg(&generated)
            .arg(&source)
            .arg(&generated_object)
            .arg(generated.join("proxy_component_type.o"))
            .arg("-o")
            .arg(&core)
            .output()
            .await
            .context("failed to compile native WASI source")?;
        if !output.status.success() {
            bail!(
                "native compiler failed: {}",
                output_text(&output.stdout, &output.stderr)
            );
        }
        if detect_artifact_format(&core)? == ArtifactFormat::Component {
            tokio::fs::rename(&core, &artifact_path).await?;
        } else {
            let output = Command::new(Self::wasm_tools())
                .args(["component", "new"])
                .arg(&core)
                .args(["-o"])
                .arg(&artifact_path)
                .output()
                .await
                .context("failed to componentize native WASI module")?;
            if !output.status.success() {
                bail!(
                    "wasm-tools component new failed: {}",
                    output_text(&output.stdout, &output.stderr)
                );
            }
        }
        if detect_artifact_format(&artifact_path)? != ArtifactFormat::Component {
            bail!("native builder produced a core module, expected a Component");
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
                toolchain: Some("wasi-sdk".into()),
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

fn has_extension(root: &Path, extension: &str) -> bool {
    std::fs::read_dir(root)
        .map(|entries| {
            entries.flatten().any(|entry| {
                entry
                    .path()
                    .extension()
                    .is_some_and(|value| value == extension)
            })
        })
        .unwrap_or(false)
}

fn collect_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if root.is_file() {
        files.push(root.to_path_buf());
        return Ok(());
    }
    for entry in std::fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_files(&path, files)?;
        } else if path.is_file() {
            files.push(path);
        }
    }
    Ok(())
}

fn output_text(stdout: &[u8], stderr: &[u8]) -> String {
    let out = String::from_utf8_lossy(stdout).trim().to_owned();
    let err = String::from_utf8_lossy(stderr).trim().to_owned();
    match (out.is_empty(), err.is_empty()) {
        (true, true) => String::new(),
        (false, true) => out,
        (true, false) => err,
        (false, false) => format!("{out}\n{err}"),
    }
}

#[cfg(test)]
mod tests {
    use super::NativeBuilder;
    use pit_builder_core::{Detection, Language, LanguageBuilder};
    use std::path::Path;

    #[test]
    fn detects_c_and_cpp_sources() {
        assert_eq!(NativeBuilder::c().language(), Language::C);
        assert_eq!(NativeBuilder::cpp().language(), Language::Cpp);
        assert_eq!(
            NativeBuilder::c().detect(Path::new("/definitely/not/a/project")),
            Detection::No
        );
    }
}
