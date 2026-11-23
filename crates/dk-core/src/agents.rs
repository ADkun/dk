//! AGENTS.md 受管块（SPEC §6.2 的 L1 层）：三层软提醒里唯一跨 harness 的一层。
//!
//! 由 `dk_init` 幂等写入：文件不存在就新建、没块就追加、有块就原地替换成当前正文。
//! 正文改动等于改治理契约 —— 权威只有一处，是 SPEC §6.2。
//!
//! 两条硬规矩，都是为了「绝不删掉用户自己写的东西」：
//! 1. 标记必须**恰好成对**。BEGIN 或 END 多一个少一个、顺序颠倒，一律判 `Broken` 并拒绝写盘
//!    （只在文件末尾追加也危险：新写的 END 会和孤儿 BEGIN 配对，把中间正文当块正文吞掉）。
//! 2. 块正文的换行不算身份：比较前把 CRLF 归一成 LF，写入时按文件多数行的风格渲染。
//!    否则同一文件第二次 init 会因换行风格判成 stale，把好好的块重写一遍。

use anyhow::{bail, Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// 受管文件（仓库根下）。
pub const FILE: &str = "AGENTS.md";
pub const BEGIN: &str = "<!-- dk:begin -->";
pub const END: &str = "<!-- dk:end -->";

/// 受管块正文（含标记）。**改它就等于改治理契约**：SPEC §6.2 是唯一权威。
pub const BLOCK: &str = "<!-- dk:begin -->
本仓库有代码索引 .dk/map.txt（一行一文件：职责 / 关系 / 契约 / 约束）。
· 改代码前：先 dk_map {task} 或 dk_get {paths}，不要搜完就动手。
· 改完：dk_put 写回你改过的文件（F、R 必写；A、S 没有就省）。
· 卡与源码对不上（stale）：先重读源码，再重写卡。
<!-- dk:end -->";

/// 块在磁盘上的现状。`check` 只报它，不改文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// 没有 AGENTS.md，或文件里没有标记。
    Missing,
    /// 标记不成对（多一个少一个、或顺序颠倒）。此时任何自动改写都会冒删掉正文的险。
    Broken,
    /// 标记成对，但正文与 `BLOCK` 不一致（旧版块，或被人手改过）。
    Stale,
    /// 块与 `BLOCK` 逐字一致（换行风格不算不一致）。
    Present,
}

impl State {
    pub fn as_str(self) -> &'static str {
        match self {
            State::Missing => "missing",
            State::Broken => "broken",
            State::Stale => "stale",
            State::Present => "present",
        }
    }
}

/// 写入结果，也是 JSON 里的 `status`。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Created,
    Appended,
    Updated,
    Unchanged,
}

impl Outcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Created => "created",
            Outcome::Appended => "appended",
            Outcome::Updated => "updated",
            Outcome::Unchanged => "unchanged",
        }
    }
}

pub fn path(root: &Path) -> PathBuf {
    root.join(FILE)
}

/// 只读判定：给 `check` 用，一个字节都不写。
pub fn state(root: &Path) -> Result<State> {
    let file = path(root);
    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::Missing),
        Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
    };
    Ok(match layout(&content) {
        Layout::None => State::Missing,
        Layout::Broken { .. } => State::Broken,
        Layout::One(s, e) => {
            if same(&content[s..e], BLOCK) {
                State::Present
            } else {
                State::Stale
            }
        }
    })
}

/// `check` 用的宽松版：文件读不出来（非 UTF-8、权限、同名目录）不拖垮整场体检，
/// 只把这一项记成 `unreadable`。
pub fn status(root: &Path) -> &'static str {
    match state(root) {
        Ok(s) => s.as_str(),
        Err(_) => "unreadable",
    }
}

/// 幂等写入：四种结果只由「文件在不在 + 标记成不成对 + 正文一样不一样」决定。
/// 标记坏掉时**一个字节都不写**，直接报错让人处理。
pub fn upsert(root: &Path) -> Result<Outcome> {
    let file = path(root);
    let content = match fs::read_to_string(&file) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            write_atomic(&file, &format!("{BLOCK}\n"))?;
            return Ok(Outcome::Created);
        }
        Err(e) => return Err(e).with_context(|| format!("read {}", file.display())),
    };

    let next = match layout(&content) {
        Layout::One(s, e) => {
            if same(&content[s..e], BLOCK) {
                return Ok(Outcome::Unchanged);
            }
            let mut out = String::with_capacity(content.len());
            out.push_str(&content[..s]);
            out.push_str(&render(&content));
            out.push_str(&content[e..]);
            (out, Outcome::Updated)
        }
        Layout::Broken { begins, ends } => {
            bail!(
                "{} 里的 dk 标记不成对（BEGIN {begin} 处 / END {end} 处）；为免按错的边界改写、\
                 删掉你自己写的正文，dk 不动这个文件。手动留成一对标记后重跑 dk init。",
                file.display(),
                begin = begins,
                end = ends
            )
        }
        Layout::None => {
            if let Some(why) = unsafe_tail(&content) {
                bail!(
                    "{} 里有没收尾的{why}，块追加进去会被读成代码、或被整段藏起来；\
                     先把它收尾，或把块要落的位置留出来，再重跑 dk init。",
                    file.display()
                )
            }
            let nl = newline_of(&content);
            let mut out = content.clone();
            if !out.is_empty() && !out.ends_with('\n') {
                out.push_str(nl);
            }
            if !out.is_empty() {
                out.push_str(nl);
            }
            out.push_str(&render(&content));
            out.push_str(nl);
            (out, Outcome::Appended)
        }
    };
    write_atomic(&file, &next.0)?;
    Ok(next.1)
}

/// 标记的排布。只认「恰好一对」，其余一律 `Broken`：宁可停下，也不猜边界。
enum Layout {
    /// 一个标记都没有。
    None,
    /// 恰好一对，`(start, end)` 是含两端标记的字节区间。
    One(usize, usize),
    Broken {
        begins: usize,
        ends: usize,
    },
}

fn layout(content: &str) -> Layout {
    let begins = content.matches(BEGIN).count();
    let ends = content.matches(END).count();
    if begins == 0 && ends == 0 {
        return Layout::None;
    }
    if begins != 1 || ends != 1 {
        return Layout::Broken { begins, ends };
    }
    let (Some(start), Some(end)) = (content.find(BEGIN), content.find(END)) else {
        return Layout::Broken { begins, ends };
    };
    if end < start {
        return Layout::Broken { begins, ends };
    }
    Layout::One(start, end + END.len())
}

/// 两段正文是否一致：换行风格不计入身份（CRLF 与 LF 视为同一份块）。
fn same(a: &str, b: &str) -> bool {
    a.replace("\r\n", "\n") == b.replace("\r\n", "\n")
}

/// 文件的多数行用什么换行：新增／重写的块跟随它，免得整篇 CRLF 里混进 LF 块。
fn newline_of(content: &str) -> &'static str {
    let crlf = content.matches("\r\n").count();
    let bare = content.matches('\n').count().saturating_sub(crlf);
    if crlf > bare {
        "\r\n"
    } else {
        "\n"
    }
}

fn render(content: &str) -> String {
    match newline_of(content) {
        "\r\n" => BLOCK.replace('\n', "\r\n"),
        _ => BLOCK.to_string(),
    }
}

/// 末尾有没有会把块「吞掉」的构造：没闭合的代码围栏（``` 或 ~~~）、没闭合的 HTML 注释。
///
/// 块落进去等于没写——要么渲染成代码，要么整段不可见，而且第二次 init 会判
/// `Unchanged` 把漏洞固化，人永远不知道那段指令没被读到。这类构造是开放集合，
/// 堵不完；这里挡住最常见的三种（含 BOM 开头的围栏行）。
fn unsafe_tail(content: &str) -> Option<&'static str> {
    let mut fences = 0usize;
    let mut tildes = 0usize;
    for line in content.lines() {
        // BOM 与缩进都不该让围栏行隐形成普通文字。
        let l = line.trim_start_matches([' ', '\t', '\u{feff}']);
        if l.starts_with("```") {
            fences += 1;
        } else if l.starts_with("~~~") {
            tildes += 1;
        }
    }
    if fences % 2 == 1 || tildes % 2 == 1 {
        return Some("代码围栏（``` 或 ~~~）");
    }
    // BEGIN/END 标记本身含 <!-- 与 -->，所以只在「没有块」这条路上用它。
    if let Some(open) = content.rfind("<!--") {
        if !content[open + 4..].contains("-->") {
            return Some("HTML 注释（<!--）");
        }
    }
    None
}

/// 同目录临时文件 + rename：写到一半崩掉也不会留下半截 AGENTS.md。
fn write_atomic(file: &Path, text: &str) -> Result<()> {
    let dir = file.parent().unwrap_or_else(|| Path::new("."));
    let tmp = dir.join(format!(".{FILE}.dk-tmp"));
    fs::write(&tmp, text).with_context(|| format!("write {}", tmp.display()))?;
    if let Err(e) = fs::rename(&tmp, file) {
        let _ = fs::remove_file(&tmp);
        return Err(e).with_context(|| format!("rename {} -> {}", tmp.display(), file.display()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let p = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp")
            .join(tag);
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn creates_then_is_idempotent() {
        let root = tmp("agents_create");
        assert_eq!(state(&root).unwrap(), State::Missing);
        assert_eq!(upsert(&root).unwrap(), Outcome::Created);
        assert_eq!(state(&root).unwrap(), State::Present);
        let once = fs::read_to_string(path(&root)).unwrap();
        assert_eq!(upsert(&root).unwrap(), Outcome::Unchanged);
        assert_eq!(once, fs::read_to_string(path(&root)).unwrap());
    }

    #[test]
    fn appends_after_existing_prose() {
        let root = tmp("agents_append");
        fs::write(root.join(FILE), "# 约定\n\n- 先读 README\n").unwrap();
        assert_eq!(upsert(&root).unwrap(), Outcome::Appended);
        let text = fs::read_to_string(path(&root)).unwrap();
        assert!(text.starts_with("# 约定\n\n- 先读 README\n\n<!-- dk:begin -->"));
        assert!(text.ends_with("<!-- dk:end -->\n"));
        assert_eq!(upsert(&root).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn replaces_a_stale_block_in_place() {
        let root = tmp("agents_stale");
        fs::write(
            root.join(FILE),
            "# 约定\n\n<!-- dk:begin -->\n旧正文\n<!-- dk:end -->\n\n## 后记\n",
        )
        .unwrap();
        assert_eq!(state(&root).unwrap(), State::Stale);
        assert_eq!(upsert(&root).unwrap(), Outcome::Updated);
        assert_eq!(state(&root).unwrap(), State::Present);
        let text = fs::read_to_string(path(&root)).unwrap();
        assert!(text.starts_with("# 约定\n\n<!-- dk:begin -->"));
        assert!(text.ends_with("<!-- dk:end -->\n\n## 后记\n"));
    }

    #[test]
    fn crlf_file_keeps_crlf_and_stays_unchanged() {
        let root = tmp("agents_crlf");
        fs::write(root.join(FILE), "# 约定\r\n\r\n- 先读 README\r\n").unwrap();
        assert_eq!(upsert(&root).unwrap(), Outcome::Appended);
        assert_eq!(upsert(&root).unwrap(), Outcome::Unchanged);
        let text = fs::read_to_string(path(&root)).unwrap();
        // 块与分隔行都是 CRLF：不留裸 LF，整篇风格一致。
        assert_eq!(text.matches('\n').count(), text.matches("\r\n").count());
        assert_eq!(state(&root).unwrap(), State::Present);
    }

    #[test]
    fn a_later_bare_lf_line_does_not_flip_the_state() {
        let root = tmp("agents_flip");
        fs::write(root.join(FILE), "# 约定\r\n").unwrap();
        upsert(&root).unwrap();
        let mut text = fs::read_to_string(path(&root)).unwrap();
        text.push_str("尾部一行\n");
        fs::write(path(&root), text).unwrap();
        assert_eq!(state(&root).unwrap(), State::Present);
        assert_eq!(upsert(&root).unwrap(), Outcome::Unchanged);
    }

    #[test]
    fn orphan_begin_is_refused_without_touching_the_file() {
        let root = tmp("agents_orphan");
        let before = "# 我的约定\n\n<!-- dk:begin -->\n这段是用户自己写的话，不能丢。\n";
        fs::write(root.join(FILE), before).unwrap();
        assert_eq!(state(&root).unwrap(), State::Broken);
        assert_eq!(status(&root), "broken");
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn duplicate_blocks_are_refused() {
        let root = tmp("agents_dupe");
        let before = format!("{BLOCK}\n\n{BLOCK}\n");
        fs::write(root.join(FILE), &before).unwrap();
        assert_eq!(state(&root).unwrap(), State::Broken);
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn refuses_to_append_inside_an_unclosed_fence() {
        let root = tmp("agents_fence");
        let before = "# 约定\n\n```md\n示例还没写完\n";
        fs::write(root.join(FILE), before).unwrap();
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn unreadable_file_is_reported_not_fatal() {
        let root = tmp("agents_unreadable");
        fs::write(root.join(FILE), [0xff, 0xfe, 0x00, 0x41]).unwrap();
        assert_eq!(status(&root), "unreadable");
        assert!(state(&root).is_err());
    }

    #[test]
    fn refuses_to_append_after_a_tilde_fence() {
        let root = tmp("agents_tilde");
        let before = "# 约定\n\n~~~md\n示例还没写完\n";
        fs::write(root.join(FILE), before).unwrap();
        assert!(unsafe_tail(before).is_some());
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn a_bom_does_not_hide_the_fence() {
        let root = tmp("agents_bom_fence");
        let before = "\u{feff}# 约定\n\n```md\n示例还没写完\n";
        fs::write(root.join(FILE), before).unwrap();
        assert!(unsafe_tail(before).is_some());
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn refuses_to_append_inside_an_unclosed_html_comment() {
        let root = tmp("agents_comment");
        let before = "# 约定\n\n<!-- 这段注释我还没写完\n";
        fs::write(root.join(FILE), before).unwrap();
        assert!(unsafe_tail(before).is_some());
        assert!(upsert(&root).is_err());
        assert_eq!(fs::read_to_string(path(&root)).unwrap(), before);
    }

    #[test]
    fn a_closed_fence_or_comment_does_not_block_the_append() {
        let root = tmp("agents_fence_closed");
        let before = "# 约定\n\n```md\n示例\n```\n\n<!-- 注 -->\n";
        fs::write(root.join(FILE), before).unwrap();
        assert_eq!(upsert(&root).unwrap(), Outcome::Appended);
        assert!(fs::read_to_string(path(&root)).unwrap().starts_with(before));
    }

    #[test]
    fn leaves_no_temp_file_behind() {
        let root = tmp("agents_atomic");
        upsert(&root).unwrap();
        let names: Vec<String> = fs::read_dir(&root)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec![FILE.to_string()], "{names:?}");
    }
}
