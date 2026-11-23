//! 排除规则：`.dkignore`（唯一排除声明）+ 内置无争议排除（SPEC §2.2）。
//!
//! `.dkignore` 语法同 `.gitignore`，直接复用 `ignore` crate 的匹配器，
//! 不自己实现通配——自己实现的通配一定会在某个 `**` 上错一次。

use anyhow::{Context, Result};
use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::path::Path;

/// 无需声明即排除的目录段。只放「放进去必然错」的：
/// 版本库元数据、dk 自己的状态、依赖安装目录、Python/IDE 缓存。
/// `dist` / `build` / `vendor` 不内置——不同生态含义不同，交给 `.dkignore` 或 `dk_sweep` 的 vendor 标记。
pub const BUILTIN_DIRS: [&str; 7] = [
    ".git",
    ".dk",
    "node_modules",
    "target",
    ".venv",
    "__pycache__",
    ".idea",
];

/// 已知二进制扩展名：连打开都不必。svg 是文本，故意不在此列。
pub const BINARY_EXT: [&str; 34] = [
    "png", "jpg", "jpeg", "gif", "bmp", "ico", "webp", "pdf", "zip", "gz", "tgz", "tar", "xz",
    "bz2", "7z", "rar", "exe", "dll", "so", "dylib", "a", "o", "obj", "lib", "class", "jar", "war",
    "wasm", "woff", "woff2", "ttf", "otf", "mp3", "mp4",
];

pub fn is_builtin_dir(name: &str) -> bool {
    BUILTIN_DIRS.contains(&name)
}

pub fn is_binary_ext(rel: &str) -> bool {
    match rel.rsplit_once('.') {
        Some((_, ext)) => {
            let e = ext.to_ascii_lowercase();
            BINARY_EXT.contains(&e.as_str())
        }
        None => false,
    }
}

pub struct Rules {
    gi: Gitignore,
    /// 解析不了的模式行。坏一行不该让整份索引不可用，但也不能一声不吭。
    bad: Vec<String>,
}

impl Rules {
    pub fn load(root: &Path) -> Result<Self> {
        let mut b = GitignoreBuilder::new(root);
        let mut bad = Vec::new();
        let f = root.join(".dkignore");
        if f.exists() {
            if let Some(e) = b.add(&f) {
                bad.push(e.to_string());
            }
        }
        // 解析失败不致命：宁可整份 .dkignore 当场失效（并告警），也不能让索引与 check 一起罢工。
        let gi = match b.build() {
            Ok(g) => g,
            Err(e) => {
                bad.push(format!("整份 .dkignore 未生效：{e}"));
                GitignoreBuilder::new(root)
                    .build()
                    .context("空规则构建失败")?
            }
        };
        Ok(Self { gi, bad })
    }

    /// 解析失败的模式行，交给 dk_check 报出来。
    pub fn bad_patterns(&self) -> &[String] {
        &self.bad
    }

    /// `rel` 是仓库相对路径（正斜杠）。父目录被排除即本文件被排除。
    pub fn is_ignored(&self, rel: &str, is_dir: bool) -> bool {
        self.gi
            .matched_path_or_any_parents(Path::new(rel), is_dir)
            .is_ignore()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tmp(tag: &str) -> std::path::PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn glob_and_dir_rules() {
        let root = tmp("ignore_rules");
        fs::write(
            root.join(".dkignore"),
            "docs/theory/**\n*.min.js\n/scratch.txt\n",
        )
        .unwrap();
        let r = Rules::load(&root).unwrap();
        assert!(r.is_ignored("docs/theory/a/b.md", false));
        assert!(r.is_ignored("web/app.min.js", false));
        assert!(r.is_ignored("scratch.txt", false));
        assert!(!r.is_ignored("src/main.rs", false));
    }

    #[test]
    fn builtin_and_binary() {
        assert!(is_builtin_dir("node_modules"));
        assert!(!is_builtin_dir("vendor"));
        assert!(is_binary_ext("a/b/logo.PNG"));
        assert!(!is_binary_ext("a/b/logo.svg"));
    }
}
