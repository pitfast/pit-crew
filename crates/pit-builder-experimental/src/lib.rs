//! Capability probes and honest adapters for languages without a supported
//! standalone WASI Component toolchain yet.
//!
//! These adapters deliberately fail at build planning rather than emitting a
//! browser WASM module or requiring a host language runtime in PitBox.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};
use async_trait::async_trait;
use pit_builder_core::{
    BuildOutput, BuildRequest, Detection, Language, LanguageBuilder, ToolchainInfo,
};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy)]
pub struct ExperimentalBuilder {
    language: Language,
}

impl ExperimentalBuilder {
    pub const fn csharp() -> Self {
        Self {
            language: Language::CSharp,
        }
    }

    pub const fn java() -> Self {
        Self {
            language: Language::Java,
        }
    }

    fn command(language: Language) -> &'static str {
        match language {
            Language::CSharp => "dotnet",
            Language::Java => "javac",
            _ => unreachable!(),
        }
    }

    fn configured_command(language: Language) -> String {
        let variable = match language {
            Language::CSharp => "PITFAST_DOTNET",
            Language::Java => "PITFAST_JAVAC",
            _ => unreachable!(),
        };
        std::env::var(variable).unwrap_or_else(|_| Self::command(language).into())
    }

    fn has_marker(project_dir: &Path, language: Language) -> bool {
        match language {
            Language::CSharp => ["*.csproj", "*.sln", "*.slnx"]
                .iter()
                .any(|pattern| glob_marker(project_dir, pattern)),
            Language::Java => {
                ["pom.xml", "build.gradle", "build.gradle.kts"]
                    .iter()
                    .any(|name| project_dir.join(name).is_file())
                    || glob_marker(project_dir, "*.java")
            }
            _ => false,
        }
    }

    fn probe_compiler(&self) -> Result<String> {
        let command = Self::configured_command(self.language);
        let args: &[&str] = match self.language {
            Language::CSharp => &["--version"],
            Language::Java => &["-version"],
            _ => &[],
        };
        let output = Command::new(&command)
            .args(args)
            .output()
            .with_context(|| format!("{} compiler is not installed", self.language))?;
        if !output.status.success() {
            bail!(
                "{} compiler probe failed: {}",
                self.language,
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        let text = format!(
            "{}",
            String::from_utf8_lossy(if output.stdout.is_empty() {
                &output.stderr
            } else {
                &output.stdout
            })
        );
        Ok(text.lines().next().unwrap_or("unknown").trim().into())
    }

    fn blocker(&self) -> &'static str {
        match self.language {
            Language::CSharp => {
                "the installed .NET workload exposes browser-wasm only; no maintained .NET wasm32-wasi/component backend is available to produce a standalone wasi:http/proxy Component"
            }
            Language::Java => {
                "no installed or validated Java-to-wasm32-wasi Component toolchain was found; a JVM/browser WASM output would not satisfy the standalone PitFast Component contract"
            }
            _ => unreachable!(),
        }
    }
}

fn glob_marker(root: &Path, pattern: &str) -> bool {
    let suffix = pattern.trim_start_matches('*');
    std::fs::read_dir(root)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .any(|entry| {
            entry.path().is_file() && entry.file_name().to_string_lossy().ends_with(suffix)
        })
}

#[async_trait]
impl LanguageBuilder for ExperimentalBuilder {
    fn language(&self) -> Language {
        self.language
    }

    fn detect(&self, project_dir: &Path) -> Detection {
        if Self::has_marker(project_dir, self.language) {
            Detection::Yes
        } else {
            Detection::No
        }
    }

    async fn probe_toolchain(&self) -> Result<ToolchainInfo> {
        let compiler = self.probe_compiler()?;
        bail!(
            "{} compiler detected ({compiler}), but capability is blocked: {}",
            self.language,
            self.blocker()
        )
    }

    async fn fingerprint(&self, _request: &BuildRequest) -> Result<String> {
        let compiler = self.probe_compiler()?;
        let mut hash = Sha256::new();
        hash.update(self.language.as_str().as_bytes());
        hash.update([0]);
        hash.update(compiler.as_bytes());
        bail!(
            "{} builder is experimental and cannot emit a PitFast Component: {} (probe fingerprint {:x})",
            self.language,
            self.blocker(),
            hash.finalize()
        )
    }

    async fn build(&self, _request: &BuildRequest, _fingerprint: &str) -> Result<BuildOutput> {
        bail!("{} builder is blocked: {}", self.language, self.blocker())
    }
}

#[cfg(test)]
mod tests {
    use super::ExperimentalBuilder;
    use pit_builder_core::{Detection, Language, LanguageBuilder};
    use std::path::Path;

    #[test]
    fn language_markers_are_conservative() {
        assert_eq!(ExperimentalBuilder::csharp().language(), Language::CSharp);
        assert_eq!(ExperimentalBuilder::java().language(), Language::Java);
        assert_eq!(
            ExperimentalBuilder::csharp().detect(Path::new(".")),
            Detection::No
        );
    }
}
