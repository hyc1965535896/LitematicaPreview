use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::EXTENSIONS;

#[derive(Clone)]
pub struct Resources {
    root: PathBuf,
}

#[derive(Serialize)]
pub struct Demo {
    name: String,
    path: String,
    extension: &'static str,
}

impl Resources {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn resolve(&self, bundled: &str, source: &str) -> Result<PathBuf, String> {
        let path = self.root.join(bundled);
        if path.exists() {
            return Ok(path);
        }
        // Development builds fall back to the crate-relative source path. Release
        // builds use only installed resources.
        #[cfg(dev)]
        {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../..")
                .join(source);
            if path.exists() {
                return Ok(path);
            }
        }
        #[cfg(not(dev))]
        let _ = source;
        Err(format!(
            "随附的 {bundled} 资源缺失，请重新安装完整的应用。"
        ))
    }

    pub fn pack(&self) -> Result<PathBuf, String> {
        self.resolve("Assets/pack.zip", "Assets/pack.zip")
    }

    pub fn demos(&self) -> Result<Vec<Demo>, String> {
        let directory = self.resolve("Demos", "Fixtures/Demos")?;
        let entries = std::fs::read_dir(&directory)
            .map_err(|e| format!("无法读取内置示例：{e}"))?;
        let mut demos = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| format!("无法读取某个内置示例：{e}"))?;
            let path = entry.path();
            if !entry
                .file_type()
                .map_err(|e| format!("无法检查某个内置示例：{e}"))?
                .is_file()
            {
                continue;
            }
            let Some(extension) = path.extension().and_then(|value| value.to_str()) else {
                continue;
            };
            let Some(&extension) = EXTENSIONS
                .iter()
                .find(|candidate| candidate[1..].eq_ignore_ascii_case(extension))
            else {
                continue;
            };
            let name = path
                .file_stem()
                .and_then(|value| value.to_str())
                .ok_or("某个内置示例的文件名无效。")?
                .to_string();
            demos.push(Demo {
                name,
                path: path_string(&path)?,
                extension,
            });
        }
        demos.sort_by(|a, b| {
            let index = |extension| EXTENSIONS.iter().position(|value| *value == extension);
            index(a.extension)
                .cmp(&index(b.extension))
                .then(a.name.cmp(&b.name))
        });
        Ok(demos)
    }

    pub fn licenses(&self) -> Result<PathBuf, String> {
        self.resolve("Licenses", ".")
    }
}

pub fn path_string(path: &Path) -> Result<String, String> {
    path.to_str()
        .map(str::to_owned)
        .ok_or_else(|| "此文件路径无法用 Unicode 表示。".into())
}
