//! 一次性测量：全量 stat 与全量 blake3 在本机的真实耗时。
//! 用来回答「每次工具调用都核对文件系统会不会很慢」。

use std::path::Path;
use std::time::Instant;

fn walk(dir: &Path, out: &mut Vec<(std::path::PathBuf, u64)>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        let name = e.file_name().to_string_lossy().to_string();
        if name == "node_modules" || name == ".git" || name == "target" || name == ".dk" {
            continue;
        }
        let Ok(md) = e.metadata() else { continue };
        if md.is_dir() {
            walk(&p, out);
        } else if md.is_file() {
            out.push((p, md.len()));
        }
    }
}

fn main() {
    let root = std::env::args().nth(1).unwrap_or_else(|| ".".into());
    let mut files = Vec::new();
    let t0 = Instant::now();
    walk(Path::new(&root), &mut files);
    let walk_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let total: u64 = files.iter().map(|(_, s)| *s).sum();

    // 全量 stat：读元数据，判断 mtime/size 是否变化
    let t1 = Instant::now();
    let mut n = 0;
    for (p, _) in &files {
        if let Ok(md) = std::fs::metadata(p) {
            if md.len() > 0 {
                n += 1;
            }
        }
    }
    let stat_ms = t1.elapsed().as_secs_f64() * 1000.0;

    // 全量 hash：真读内容
    let t2 = Instant::now();
    let mut bytes = 0u64;
    for (p, _) in &files {
        if let Ok(b) = std::fs::read(p) {
            bytes += b.len() as u64;
            let _ = dk_core::card::short_hash(&b);
        }
    }
    let hash_ms = t2.elapsed().as_secs_f64() * 1000.0;

    println!("root={root}");
    println!(
        "files={} total={:.1} MiB",
        files.len(),
        total as f64 / 1048576.0
    );
    println!("walk+readdir      {:8.1} ms", walk_ms);
    println!("stat all ({n})    {:8.1} ms", stat_ms);
    println!(
        "hash all ({:.1} MiB) {:8.1} ms",
        bytes as f64 / 1048576.0,
        hash_ms
    );
}
