//! 存储：`.dk/` 布局、O_EXCL 锁、CAS 写入、`.dkignore` 追加（SPEC §2.2）。
//!
//! 只有一种状态是合法的：受管范围内每个文件都有卡。所以这里没有「待办队列」，
//! 过期由源码哈希现算（`dk_get` / `dk_check`），不需要另立一份账本。

use crate::card::{short_hash, Card, ParseError};
use crate::ignore_rules::Rules;
use crate::managed::{self, Verdict};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

pub const HEADER: &str = "#dk-tok:v1";
/// `.dkignore` 里「模型提议的排除」分节标记。人可整段删除。
pub const PROPOSED_MARK: &str = "# dk:proposed";
const LOCK_STALE: Duration = Duration::from_secs(60);

pub const DEFAULT_IGNORE: &str = "\
# dk 排除声明（.gitignore 语法）。
# 人在此写目录级排除，一次写清，例如：docs/theory/**
# 工具会把「判定不值得建卡」的精确路径追加到下方的 dk:proposed 分节。
";

/// 坏行：行号 + 原因。
pub type ParseFailure = (usize, ParseError);

/// 一次写入请求中的一条。
/// 拒未知字段：schema 里写了 additionalProperties:false，实现不真拒就只是句场面话——
/// 拼错 Fn/F 会被 serde 静默吞掉，模型以为写进去了（审查 F6）。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PutEntry {
    pub path: String,
    // 卡片字段是大写的（F/R/A/S）。参数名必须与卡片长得一样，
    // 否则模型照着 SPEC 传 F，只会得到一句「新卡必须有 F」——自相矛盾的错误最难查。
    #[serde(default, rename = "F", alias = "f")]
    pub f: Option<String>,
    #[serde(default, rename = "R", alias = "r")]
    pub r: Option<String>,
    #[serde(default, rename = "A", alias = "a")]
    pub a: Option<String>,
    #[serde(default, rename = "S", alias = "s")]
    pub s: Option<String>,
    #[serde(default)]
    pub tags: Option<String>,
    /// 该文件不需要卡：删掉已有卡，并把路径追加进 `.dkignore`。
    #[serde(default)]
    pub skip: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct PutReport {
    pub added: Vec<String>,
    pub updated: Vec<String>,
    pub removed: Vec<String>,
    /// 本次新写入 `.dkignore` 的路径（人不认可就删掉那几行）。
    pub proposed_ignore: Vec<String>,
    /// 拿不到源码或新建卡缺 F 的条目。
    pub rejected: Vec<String>,
    pub bad_lines_kept: usize,
    /// 本次按 `drop_bad` 清掉的坏行数。
    pub bad_lines_dropped: usize,
    /// 软门告警（SPEC §8），不阻断。
    pub warnings: Vec<String>,
}

/// 写入开关。默认保留坏行——宁可留在文件里让人看见，也不静默删掉别人的东西。
#[derive(Debug, Clone, Copy, Default)]
pub struct PutOpts {
    pub drop_bad: bool,
}

/// 软门默认值（SPEC §8）。
pub struct Limits {
    pub f: usize,
    pub a: usize,
    pub s: usize,
    pub r_items: usize,
    pub total: usize,
    pub sweep_batch: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            f: 200,
            a: 150,
            s: 400,
            r_items: 8,
            total: 800,
            sweep_batch: 10,
        }
    }
}

pub struct Store {
    root: PathBuf,
    dir: PathBuf,
}

impl Store {
    pub fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        let dir = root.join(".dk");
        Ok(Self { root, dir })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn ignore_path(&self) -> PathBuf {
        self.root.join(".dkignore")
    }

    /// 主索引路径。分片模式（M4）会在这里按 `path` 顶层目录路由到
    /// `.dk/map/<top>.txt`；调用方拿到的仍是一个 `PathBuf`，接口不变。
    pub fn index_file(&self) -> PathBuf {
        self.dir.join("map.txt")
    }

    pub fn is_initialized(&self) -> bool {
        self.dir.is_dir()
    }

    pub fn ensure_layout(&self) -> Result<()> {
        fs::create_dir_all(&self.dir).with_context(|| format!("mkdir {}", self.dir.display()))?;
        let ignore = self.ignore_path();
        if !ignore.exists() {
            fs::write(&ignore, DEFAULT_IGNORE)?;
        }
        Ok(())
    }

    fn read_map_raw(&self) -> Result<String> {
        let f = self.index_file();
        if !f.exists() {
            return Ok(String::new());
        }
        // 索引里混进一个坏字节，不该让 check / map / sweep 全部瘫痪：
        // 降级成 U+FFFD 读进来，该行自然变成坏行进坏行容器，check 仍是救命的工具。
        let bytes = fs::read(&f).with_context(|| format!("read {}", f.display()))?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    }

    /// 读全部卡片。坏行不中断整体读取，行号收在 `errors` 里。
    pub fn load(&self) -> Result<(Vec<Card>, Vec<ParseFailure>)> {
        let raw = self.read_map_raw()?;
        let mut cards = Vec::new();
        let mut errors = Vec::new();
        for (i, line) in raw.lines().enumerate() {
            let t = line.trim_end();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            match Card::parse(t) {
                Ok(c) => cards.push(c),
                Err(e) => errors.push((i + 1, e)),
            }
        }
        Ok((cards, errors))
    }

    /// `.dkignore` 的 `# dk:proposed` 分节里列出的路径。
    pub fn proposed_ignore(&self) -> Result<Vec<String>> {
        let Ok(text) = fs::read_to_string(self.ignore_path()) else {
            return Ok(Vec::new());
        };
        let mut in_section = false;
        let mut out = Vec::new();
        for line in text.lines() {
            let t = line.trim();
            if t == PROPOSED_MARK {
                in_section = true;
                continue;
            }
            if !in_section || t.is_empty() || t.starts_with('#') {
                continue;
            }
            out.push(t.to_string());
        }
        Ok(out)
    }

    /// 把路径追加进 `.dkignore` 的 `# dk:proposed` 分节（已存在则跳过）。
    pub fn append_proposed(&self, paths: &[String]) -> Result<()> {
        let mut text =
            fs::read_to_string(self.ignore_path()).unwrap_or_else(|_| DEFAULT_IGNORE.into());
        let existing = self.proposed_ignore()?;
        let fresh: Vec<&String> = paths.iter().filter(|p| !existing.contains(p)).collect();
        if fresh.is_empty() {
            return Ok(());
        }
        if !text.contains(PROPOSED_MARK) {
            if !text.ends_with('\n') {
                text.push('\n');
            }
            text.push('\n');
            text.push_str(PROPOSED_MARK);
        }
        if !text.ends_with('\n') {
            text.push('\n');
        }
        for p in fresh {
            text.push_str(p);
            text.push('\n');
        }
        fs::write(self.ignore_path(), text)?;
        Ok(())
    }

    /// 写卡。语义：传了的字段覆盖，没传的保留；新路径补全创建。
    pub fn put(&self, entries: &[PutEntry], limits: &Limits, opts: PutOpts) -> Result<PutReport> {
        if entries.is_empty() {
            bail!("entries 不能为空");
        }
        self.ensure_layout()?;
        let rules = Rules::load(self.root())?;
        let before_raw = self.read_map_raw()?;

        let mut map: BTreeMap<String, Card> = BTreeMap::new();
        let mut bad: Vec<String> = Vec::new();
        for line in before_raw.lines() {
            let t = line.trim_end();
            if t.is_empty() || t.starts_with('#') {
                continue;
            }
            match Card::parse(t) {
                Ok(c) => {
                    map.insert(c.path.clone(), c);
                }
                Err(_) => bad.push(t.to_string()),
            }
        }

        let mut report = PutReport::default();
        let mut to_ignore: Vec<String> = Vec::new();
        let mut seen: BTreeMap<String, usize> = BTreeMap::new();

        for (idx, e) in entries.iter().enumerate() {
            let raw = managed::normalize(&e.path);
            // skip 只做形态校验：它最常见的用途恰恰是「这个文件不受管，别再来问我」，
            // 因此不能要求它先通过受管判定。
            if raw.is_empty()
                || raw.starts_with('/')
                || raw.contains(':')
                || raw.contains('@')
                || raw.split('/').any(|s| s == "..")
            {
                report.rejected.push(format!("{}（路径不合法）", e.path));
                continue;
            }
            if e.skip {
                if map.remove(&raw).is_some() {
                    report.removed.push(raw.clone());
                }
                to_ignore.push(raw.clone());
                report.proposed_ignore.push(raw.clone());
                continue;
            }
            // 一次收口：形态 / 是否在库内 / walk 会不会收它 / 正名，全在 managed 里判。
            // 少了这一步就会造出「写得进索引、检索永远看不到」的失联卡，并让 check 永久报错。
            let path = match managed::verify(self.root(), &rules, &e.path) {
                Verdict::Managed { rel } => rel,
                Verdict::Rejected(why) => {
                    report.rejected.push(format!("{}（{why}）", e.path));
                    continue;
                }
            };
            if path != raw {
                report
                    .warnings
                    .push(format!("{} → {}（按磁盘正名）", e.path, path));
            }
            if seen.insert(path.clone(), idx).is_some() {
                report
                    .warnings
                    .push(format!("{path}（同批出现多次，后一条生效）"));
            }
            // SPEC §8 硬门：R 指向必须存在。写进去再让 check 报错，等于把错误推给下一个受害者。
            if let Some(r) = &e.r {
                let bad: Vec<String> = r
                    .split(',')
                    .map(|s| s.trim())
                    .filter_map(|s| s.strip_prefix("code:"))
                    .map(|s| s.trim().to_string())
                    .filter(|p| !managed::exists_in_repo(self.root(), p))
                    .collect();
                if !bad.is_empty() {
                    report
                        .rejected
                        .push(format!("{path}（R 指向不存在：{}）", bad.join(", ")));
                    continue;
                }
                // code: 有硬门，其余前缀与裸文本只当自由文本——于是「跨库文件」写成裸路径也照样
                // 写进去：不校验、连不上邻居、永远不新鲜，看起来记下来了其实什么都没记。
                for item in r.split(',').map(str::trim) {
                    // 「不带命名空间」不能拿「含 :」判：Windows 盘符（D:/other/x.rs）与 URL 都带 :，
                    // 而前者恰恰是最常见的跨库写法，被这条判成有命名空间就静默写进卡（审查 P2-1）。
                    let namespaced = item.starts_with("code:") || item.starts_with("database://");
                    let bare = !item.is_empty() && !namespaced && item.contains('/');
                    if bare && !managed::exists_in_repo(self.root(), item) {
                        report.warnings.push(format!(
                            "{path}（R 里的 {item} 不是库内文件：不带 code: / database:// 前缀的项只当自由文本，不连邻居也不校验；跨库或远程的东西写进卡片正文才算记下来）"
                        ));
                    }
                }
            }
            let Ok(bytes) = fs::read(self.root.join(&path)) else {
                report.rejected.push(format!("{path}（读不到源码）"));
                continue;
            };
            let h = short_hash(&bytes);
            let orig = map.get(&path).cloned();
            match map.get_mut(&path) {
                Some(c) => {
                    if let Some(v) = &e.f {
                        c.f = v.clone();
                    }
                    if let Some(v) = &e.r {
                        c.r = v.clone();
                    }
                    if let Some(v) = &e.a {
                        c.a = v.clone();
                    }
                    if let Some(v) = &e.s {
                        c.s = v.clone();
                    }
                    if e.tags.is_some() {
                        c.tags = e.tags.clone().filter(|t| !t.is_empty());
                    }
                    c.src_hash = h;
                    report.updated.push(path.clone());
                }
                None => {
                    let Some(f) = e.f.clone().filter(|s| !s.trim().is_empty()) else {
                        report.rejected.push(format!("{path}（新卡必须有 F）"));
                        continue;
                    };
                    map.insert(
                        path.clone(),
                        Card {
                            path: path.clone(),
                            src_hash: h,
                            tags: e.tags.clone().filter(|t| !t.is_empty()),
                            f,
                            r: e.r.clone().unwrap_or_else(|| "-".into()),
                            a: e.a.clone().unwrap_or_else(|| "-".into()),
                            s: e.s.clone().unwrap_or_else(|| "-".into()),
                        },
                    );
                    report.added.push(path.clone());
                }
            }
            // 回环校验：渲染出的行必须能原样解析回同一张卡。
            // 字段里有换行、或出现「 | R:」这类分段标记，都会把这行切成两条：
            // 写进去就是一条永久坏行，而原卡也随之消失。这与「文件名带 @」是同类入口，
            // 所以它必须和别的硬门一样在写入前判，不能留给 check 事后报。
            if let Some(c) = map.get(&path) {
                let line = c.render();
                // 换行不会让 parse 失败（校验和覆盖的是含换行的整串），但写进 map.txt 后
                // 一张卡就变成两条物理行，第二条必然成坏行。所以单行性必须单独判。
                let round_trip = !line.contains(['\n', '\r'])
                    && matches!(Card::parse(&line), Ok(back) if back.body() == c.body());
                if !round_trip {
                    report.added.retain(|p| p != &path);
                    report.updated.retain(|p| p != &path);
                    report.rejected.push(format!(
                        "{path}（字段里有换行或「 | F:」这类分段标记，会把卡片行切坏）"
                    ));
                    match orig {
                        Some(prev) => {
                            map.insert(path.clone(), prev);
                        }
                        None => {
                            map.remove(&path);
                        }
                    }
                }
            }
        }

        let mut out = String::new();
        out.push_str(HEADER);
        out.push('\n');
        for c in map.values() {
            report.warnings.extend(soft_gate(c, limits));
            out.push_str(c.render().trim_end());
            out.push('\n');
        }
        if !bad.is_empty() {
            if opts.drop_bad {
                report.bad_lines_dropped = bad.len();
            } else {
                out.push_str("# 以下为无法解析的历史行，未纳入索引，请人工处理\n");
                for b in &bad {
                    out.push_str(b);
                    out.push('\n');
                }
                report.bad_lines_kept = bad.len();
            }
        }

        // CAS：从读到写之间若被别的进程改过，直接失败，不做合并猜测。
        if self.read_map_raw()? != before_raw {
            bail!("索引在写入期间被其他进程修改，请重试");
        }
        let idx = self.index_file();
        let tmp = self.dir.join("map.txt.tmp");
        {
            let mut f = OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&tmp)
                .with_context(|| format!("open {}", tmp.display()))?;
            f.write_all(out.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &idx).with_context(|| format!("rename -> {}", idx.display()))?;

        if !to_ignore.is_empty() {
            self.append_proposed(&to_ignore)?;
        }
        // 同批里同一个 path 出现两次时，added/updated 会各记一次。索引本身没错（后一条生效），
        // 但报告说「写了两个文件」是假话——而这份报告是 agent 判断「还差什么」的唯一依据。
        // 两个列表各用一个集合：共用一个集合会让 added 把 updated 里的同名路径吞掉。
        let mut seen_added = std::collections::HashSet::new();
        report.added.retain(|p| seen_added.insert(p.clone()));
        let mut seen_updated = std::collections::HashSet::new();
        report.updated.retain(|p| seen_updated.insert(p.clone()));
        Ok(report)
    }

    /// 读源码现算指纹。文件不存在返回 None。
    pub fn source_hash(&self, rel: &str) -> Option<String> {
        fs::read(self.root.join(rel)).ok().map(|b| short_hash(&b))
    }
}

fn soft_gate(c: &Card, l: &Limits) -> Vec<String> {
    let mut w = Vec::new();
    let n = |s: &str| if s == "-" { 0 } else { s.chars().count() };
    if n(&c.f) > l.f {
        w.push(format!("{}: F 超过 {} 字符", c.path, l.f));
    }
    if n(&c.a) > l.a {
        w.push(format!("{}: A 超过 {} 字符", c.path, l.a));
    }
    if n(&c.s) > l.s {
        w.push(format!("{}: S 超过 {} 字符", c.path, l.s));
    }
    if c.r_paths().len() > l.r_items {
        w.push(format!("{}: R 超过 {} 条", c.path, l.r_items));
    }
    if c.char_len() > l.total {
        w.push(format!("{}: 单卡超过 {} 字符", c.path, l.total));
    }
    w
}

/// 单文件锁。O_EXCL 创建；持有者持续心跳，心跳停了超过 `LOCK_STALE` 才算陈旧。
///
/// 只看 mtime 不够：十万文件首次建卡实测 73 s，超过任何固定阈值，
/// 第二个进程就会误判持有者已死并接管，然后双方 CAS 撞车。
pub struct Lock {
    path: PathBuf,
    stop: Arc<AtomicBool>,
}

impl Lock {
    pub fn acquire(dir: &Path, timeout: Duration) -> Result<Self> {
        let path = dir.join("lock");
        let start = SystemTime::now();
        loop {
            match OpenOptions::new().write(true).create_new(true).open(&path) {
                Ok(mut f) => {
                    let _ = write!(f, "{}", std::process::id());
                    let _ = f.sync_all();
                    let stop = Arc::new(AtomicBool::new(false));
                    let hb_path = path.clone();
                    let hb_stop = Arc::clone(&stop);
                    std::thread::spawn(move || loop {
                        // 每 10 s 摸一次 mtime；stop 置位后 250 ms 内退出。
                        for _ in 0..40 {
                            if hb_stop.load(Ordering::Relaxed) {
                                return;
                            }
                            std::thread::sleep(Duration::from_millis(250));
                        }
                        if let Ok(f) = OpenOptions::new().write(true).open(&hb_path) {
                            if hb_stop.load(Ordering::Relaxed) {
                                return;
                            }
                            let _ = f.set_modified(SystemTime::now());
                        }
                    });
                    return Ok(Self { path, stop });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    // 陈旧锁：mtime 超过 60s 视为持有者已死，接管。
                    let stale = fs::metadata(&path)
                        .and_then(|m| m.modified())
                        .ok()
                        .and_then(|t| t.elapsed().ok())
                        .map(|d| d > LOCK_STALE)
                        .unwrap_or(false);
                    if stale {
                        let _ = fs::remove_file(&path);
                        continue;
                    }
                    if start.elapsed().unwrap_or_default() > timeout {
                        bail!("索引被另一个进程锁住（{}），稍后重试", path.display());
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return Err(e).with_context(|| format!("create {}", path.display())),
            }
        }
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(p.join("src")).unwrap();
        p
    }

    /// R 里写成库外路径时只给 warning、不拒收；但「库外」必须真被认出来——
    /// Windows 盘符与 URL 都带 `:`，拿「含 :」判命名空间会把它们放过（审查 P2-1）。
    #[test]
    fn put_warns_on_outside_r_but_keeps_code_hard_gate() {
        let root = tmp("store_r_outside");
        fs::write(root.join("src/a.rs"), "fn a(){}\n").unwrap();
        fs::write(root.join("src/b.rs"), "fn b(){}\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let limits = Limits::default();
        let put = |r: &str| {
            s.put(
                &[PutEntry {
                    path: "src/a.rs".into(),
                    f: Some("甲".into()),
                    r: Some(r.into()),
                    a: None,
                    s: None,
                    tags: None,
                    skip: false,
                }],
                &limits,
                PutOpts { drop_bad: false },
            )
            .unwrap()
        };

        // 库内：不吵
        let rep = put("code:src/b.rs");
        assert!(rep.warnings.is_empty(), "{:?}", rep.warnings);
        assert!(rep.rejected.is_empty());

        // 库外裸路径（含 Windows 盘符 / URL / 相对爬升）：告警但写进去
        for r in [
            "../other/a.rs",
            "D:/other/repo/x.rs",
            "https://github.com/x/y.rs",
            "/etc/passwd",
        ] {
            let rep = put(r);
            assert!(
                rep.warnings.iter().any(|w| w.contains("不是库内文件")),
                "{r} 应给 warning，实际 {:?}",
                rep.warnings
            );
            assert!(rep.rejected.is_empty(), "{r} 不该被拒收");
        }

        // code: 仍是硬门：整条拒收，不写半张卡
        let rep = put("code:D:/other/x.rs");
        assert!(rep.warnings.is_empty());
        assert!(rep.rejected.iter().any(|e| e.contains("R 指向不存在")));
    }

    #[test]
    fn put_creates_and_merges() {
        let root = tmp("store_merge");
        fs::write(root.join("src/a.rs"), "fn a(){}\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let rep = s
            .put(
                &[PutEntry {
                    path: "src/a.rs".into(),
                    f: Some("甲".into()),
                    r: Some("src/b.rs".into()),
                    a: None,
                    s: None,
                    tags: None,
                    skip: false,
                }],
                &Limits::default(),
                PutOpts::default(),
            )
            .unwrap();
        assert_eq!(rep.added.len(), 1);
        // 只改 F，R 必须保留
        s.put(
            &[PutEntry {
                path: "src/a.rs".into(),
                f: Some("乙".into()),
                r: None,
                a: None,
                s: None,
                tags: None,
                skip: false,
            }],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let (cards, errs) = s.load().unwrap();
        assert!(errs.is_empty());
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].f, "乙");
        assert_eq!(cards[0].r, "src/b.rs");
    }

    #[test]
    fn skip_removes_card_and_lands_in_ignore() {
        let root = tmp("store_skip");
        fs::write(root.join("src/gen.js"), "x\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(
            &[PutEntry {
                path: "src/gen.js".into(),
                f: Some("生成物".into()),
                r: None,
                a: None,
                s: None,
                tags: None,
                skip: false,
            }],
            &Limits::default(),
            PutOpts::default(),
        )
        .unwrap();
        let rep = s
            .put(
                &[PutEntry {
                    path: "src/gen.js".into(),
                    f: None,
                    r: None,
                    a: None,
                    s: None,
                    tags: None,
                    skip: true,
                }],
                &Limits::default(),
                PutOpts::default(),
            )
            .unwrap();
        assert_eq!(rep.removed.len(), 1);
        assert_eq!(s.load().unwrap().0.len(), 0);
        assert_eq!(s.proposed_ignore().unwrap(), vec!["src/gen.js".to_string()]);
    }

    #[test]
    fn new_entry_without_f_is_rejected() {
        let root = tmp("store_nof");
        fs::write(root.join("src/a.rs"), "x\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let rep = s
            .put(
                &[PutEntry {
                    path: "src/a.rs".into(),
                    f: None,
                    r: None,
                    a: None,
                    s: None,
                    tags: None,
                    skip: false,
                }],
                &Limits::default(),
                PutOpts::default(),
            )
            .unwrap();
        assert_eq!(rep.rejected.len(), 1);
        assert!(s.load().unwrap().0.is_empty());
    }

    #[test]
    fn lock_is_exclusive() {
        let root = tmp("store_lock");
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let _a = Lock::acquire(s.dir(), Duration::from_millis(50)).unwrap();
        let b = Lock::acquire(s.dir(), Duration::from_millis(50));
        assert!(b.is_err());
    }

    fn entry(path: &str) -> PutEntry {
        PutEntry {
            path: path.into(),
            f: Some("职责".into()),
            r: None,
            a: None,
            s: None,
            tags: None,
            skip: false,
        }
    }

    #[test]
    fn rejects_paths_that_would_break_the_index() {
        let root = tmp("store_bad_paths");
        fs::write(root.join("src/a.rs"), "x\n").unwrap();
        fs::create_dir_all(root.join("img")).unwrap();
        fs::write(root.join("img/logo@2x.png"), "x").unwrap();
        fs::write(root.join("img/plain.png"), "x").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let rep = s
            .put(
                &[
                    entry("img/logo@2x.png"),
                    entry("C:/Windows/win.ini"),
                    entry("img/plain.png"),
                    entry("src/missing.rs"),
                    entry("src/a.rs"),
                ],
                &Limits::default(),
                PutOpts::default(),
            )
            .unwrap();
        assert_eq!(rep.added, vec!["src/a.rs".to_string()]);
        assert_eq!(rep.rejected.len(), 4, "{:?}", rep.rejected);
        // 关键：索引没被写坏。写进去再让 check 报错，等于把错误推给下一个受害者。
        let (cards, errs) = s.load().unwrap();
        assert!(errs.is_empty());
        assert_eq!(cards.len(), 1);
    }

    #[test]
    fn rejects_r_pointing_nowhere() {
        let root = tmp("store_bad_r");
        fs::write(root.join("src/a.rs"), "x\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        let mut e = entry("src/a.rs");
        e.r = Some("code:src/nope.rs".into());
        let rep = s.put(&[e], &Limits::default(), PutOpts::default()).unwrap();
        assert_eq!(rep.rejected.len(), 1);
        assert!(s.load().unwrap().0.is_empty());
    }

    #[test]
    fn report_lists_do_not_double_count_a_path() {
        let root = tmp("dedup");
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        fs::write(root.join("src/a.rs"), "fn a(){}\n").unwrap();
        let mut one = entry("src/a.rs");
        one.f = Some("一".into());
        let mut two = entry("src/a.rs");
        two.f = Some("二".into());
        let rep = s
            .put(&[one, two], &Limits::default(), PutOpts::default())
            .unwrap();
        assert_eq!(rep.added.len(), 1);
        assert_eq!(rep.updated.len(), 1);
        assert!(rep.warnings.iter().any(|w| w.contains("同批出现多次")));
        assert_eq!(s.load().unwrap().0[0].f, "二", "后一条生效");
    }
    #[test]
    fn rejects_cards_whose_text_would_break_the_line() {
        let root = tmp("round_trip");
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        std::fs::write(s.root().join("src/a.rs"), "fn a() {}\n").unwrap();
        // 字段里出现「 | R:」会被行解析当成新字段起点：写进去就是永久坏行。
        let mut e = entry("src/a.rs");
        e.f = Some("甲 | R:乙".into());
        let rep = s.put(&[e], &Limits::default(), PutOpts::default()).unwrap();
        assert_eq!(rep.rejected.len(), 1, "{:?}", rep.rejected);
        assert!(rep.added.is_empty());
        assert!(s.load().unwrap().0.is_empty());
    }

    #[test]
    fn rejects_cards_with_newlines_and_keeps_the_old_card() {
        let root = tmp("newline_card");
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        std::fs::write(s.root().join("src/a.rs"), "fn a() {}\n").unwrap();
        let mut good = entry("src/a.rs");
        good.f = Some("甲".into());
        s.put(&[good], &Limits::default(), PutOpts::default())
            .unwrap();
        let mut bad = entry("src/a.rs");
        bad.f = Some("甲\n乙".into());
        let rep = s
            .put(&[bad], &Limits::default(), PutOpts::default())
            .unwrap();
        assert_eq!(rep.rejected.len(), 1);
        let (cards, errs) = s.load().unwrap();
        assert!(errs.is_empty(), "{errs:?}");
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].f, "甲", "旧卡必须保留，不能被一次坏更新吞掉");
    }
    #[test]
    fn broken_bytes_degrade_to_bad_lines_and_can_be_dropped() {
        let root = tmp("store_broken_bytes");
        fs::write(root.join("src/a.rs"), "x\n").unwrap();
        let s = Store::open(&root).unwrap();
        s.ensure_layout().unwrap();
        s.put(&[entry("src/a.rs")], &Limits::default(), PutOpts::default())
            .unwrap();
        // 往索引里塞一个非 UTF-8 字节：读不能崩，坏行要能被看见、也能被清掉。
        let mut raw = fs::read(s.index_file()).unwrap();
        raw.extend_from_slice(b"\xff\xfe bad\n");
        fs::write(s.index_file(), &raw).unwrap();
        let (cards, errs) = s.load().unwrap();
        assert_eq!(cards.len(), 1);
        assert_eq!(errs.len(), 1);
        let rep = s
            .put(
                &[entry("src/a.rs")],
                &Limits::default(),
                PutOpts { drop_bad: true },
            )
            .unwrap();
        assert_eq!(rep.bad_lines_dropped, 1);
        assert!(s.load().unwrap().1.is_empty());
    }
}
