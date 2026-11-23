//! 检索：BM25 排序 + R 扩散 + 目录聚合（SPEC §4）。
//!
//! 排序期只有词面一个通道；R 关系只在「命中之后」扩散，
//! 因为实测它在排序期零贡献、在扩散期值 +15.8pp。

use crate::card::Card;
use crate::tokenize::tokenize;
use std::collections::HashMap;

const K1: f64 = 1.2;
const B: f64 = 0.75;
/// 字面兜底的最低覆盖率：查询里至少六成汉字出现在同一张卡里才算候选。
const FALLBACK_COVER: f64 = 0.6;

pub struct Index<'a> {
    cards: &'a [Card],
    tf: Vec<HashMap<String, u32>>,
    dl: Vec<usize>,
    df: HashMap<String, u32>,
    avgdl: f64,
    /// R 图入度：被多少条卡引用。免费得来的「重要度」，替代人写 C 值。
    in_degree: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub idx: usize,
    pub score: f64,
    pub why: &'static str,
}

impl<'a> Index<'a> {
    pub fn build(cards: &'a [Card]) -> Self {
        let mut tf = Vec::with_capacity(cards.len());
        let mut dl = Vec::with_capacity(cards.len());
        let mut df: HashMap<String, u32> = HashMap::new();
        for c in cards {
            let text = format!("{} {} {} {} {}", c.path, c.f, c.r, c.a, c.s);
            let toks = tokenize(&text);
            let mut m: HashMap<String, u32> = HashMap::new();
            for t in &toks {
                *m.entry(t.clone()).or_insert(0) += 1;
            }
            for k in m.keys() {
                *df.entry(k.clone()).or_insert(0) += 1;
            }
            dl.push(toks.len().max(1));
            tf.push(m);
        }
        let avgdl = dl.iter().sum::<usize>() as f64 / cards.len().max(1) as f64;
        let mut in_degree = vec![0u32; cards.len()];
        let pos: HashMap<&str, usize> = cards
            .iter()
            .enumerate()
            .map(|(i, c)| (c.path.as_str(), i))
            .collect();
        for c in cards {
            for r in c.r_paths() {
                if let Some(&j) = pos.get(r) {
                    in_degree[j] = in_degree[j].saturating_add(1);
                }
            }
        }
        Self {
            cards,
            tf,
            dl,
            df,
            avgdl: avgdl.max(1.0),
            in_degree,
        }
    }

    pub fn cards(&self) -> &'a [Card] {
        self.cards
    }

    pub fn in_degree(&self, i: usize) -> u32 {
        self.in_degree[i]
    }

    /// BM25 打分，返回按分数降序的 (下标, 分)。
    pub fn bm25(&self, query: &str) -> Vec<(usize, f64)> {
        let n = self.cards.len() as f64;
        let q: Vec<String> = {
            let mut seen = HashMap::new();
            let mut v = Vec::new();
            for t in tokenize(query) {
                if seen.insert(t.clone(), ()).is_none() {
                    v.push(t);
                }
            }
            v
        };
        if q.is_empty() || n == 0.0 {
            return Vec::new();
        }
        let mut scores = vec![0.0f64; self.cards.len()];
        for t in &q {
            let Some(&d) = self.df.get(t) else { continue };
            let idf = ((n - d as f64 + 0.5) / (d as f64 + 0.5) + 1.0).ln();
            for (i, m) in self.tf.iter().enumerate() {
                let Some(&f) = m.get(t) else { continue };
                let f = f as f64;
                let denom = f + K1 * (1.0 - B + B * self.dl[i] as f64 / self.avgdl);
                scores[i] += idf * f * (K1 + 1.0) / denom;
            }
        }
        let mut out: Vec<(usize, f64)> = scores
            .into_iter()
            .enumerate()
            .filter(|(_, s)| *s > 0.0)
            .collect();
        out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        out
    }

    /// 主检索：BM25 → 取前 k → R 一跳扩散（按入度截断）。
    /// `query` 参与 BM25；`fallback_query` 只用于零命中时的字面兜底。
    /// 两者分开是因为 hints 会把字符袋稀释掉——辅助词不该拖累主词。
    pub fn retrieve(&self, query: &str, fallback_query: &str, k: usize, spread: usize) -> Vec<Hit> {
        let ranked = self.bm25(query);
        let mut hits: Vec<Hit> = if ranked.is_empty() {
            self.char_fallback(fallback_query, k)
        } else {
            ranked
                .iter()
                .take(k)
                .map(|(i, s)| Hit {
                    idx: *i,
                    score: *s,
                    why: "bm25",
                })
                .collect()
        };
        if spread == 0 || hits.is_empty() {
            return hits;
        }
        let mut have: HashMap<usize, ()> = hits.iter().map(|h| (h.idx, ())).collect();
        let pos: HashMap<&str, usize> = self
            .cards
            .iter()
            .enumerate()
            .map(|(i, c)| (c.path.as_str(), i))
            .collect();
        // R 是双向邻接：命中卡指向谁，以及谁指向命中卡
        let mut neighbours: Vec<usize> = Vec::new();
        for h in hits.clone() {
            for r in self.cards[h.idx].r_paths() {
                if let Some(&j) = pos.get(r) {
                    neighbours.push(j);
                }
            }
        }
        for (i, c) in self.cards.iter().enumerate() {
            if have.contains_key(&i) {
                continue;
            }
            if c.r_paths()
                .iter()
                .any(|r| hits.iter().any(|h| self.cards[h.idx].path == *r))
            {
                neighbours.push(i);
            }
        }
        neighbours.sort_by_key(|&i| std::cmp::Reverse(self.in_degree[i]));
        neighbours.dedup();
        for j in neighbours.into_iter().take(spread) {
            if have.contains_key(&j) {
                continue;
            }
            have.insert(j, ());
            let base = hits.iter().map(|h| h.score).fold(0.0f64, f64::max);
            hits.push(Hit {
                idx: j,
                score: base * 0.5,
                why: "spread",
            });
        }
        hits
    }

    /// 零命中兜底：查询里的汉字当「字符袋」，卡片覆盖得越全分越高。
    ///
    /// 中文任务常是「优化锁逻辑」这种短句，而卡片写的是「文件锁」——二元组词表对不上
    /// 就是零命中。兜底只在 BM25 全灭时启用：宁可给一个明显弱的答案并说明，也不假装查无此物。
    fn char_fallback(&self, query: &str, k: usize) -> Vec<Hit> {
        let mut want: Vec<char> = Vec::new();
        for c in query.chars() {
            if crate::tokenize::is_unspaced(c) && !want.contains(&c) {
                want.push(c);
            }
        }
        if want.is_empty() {
            return Vec::new();
        }
        let mut scored: Vec<(usize, f64)> = Vec::new();
        for (i, c) in self.cards.iter().enumerate() {
            let text = format!("{} {} {} {} {}", c.path, c.f, c.r, c.a, c.s);
            let hit = want.iter().filter(|w| text.contains(**w)).count();
            let cover = hit as f64 / want.len() as f64;
            if cover >= FALLBACK_COVER {
                scored.push((i, cover));
            }
        }
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(i, s)| Hit {
                idx: i,
                score: s,
                why: "cjk",
            })
            .collect()
    }

    /// 目录聚合：命中点所在目录下的同族卡片，一行摘要用（SPEC §4.2 R3）。
    pub fn siblings(&self, idx: usize) -> Vec<usize> {
        let dir = parent_dir(&self.cards[idx].path);
        self.cards
            .iter()
            .enumerate()
            .filter(|(i, c)| *i != idx && parent_dir(&c.path) == dir)
            .map(|(i, _)| i)
            .collect()
    }
}

pub fn parent_dir(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(path: &str, f: &str, r: &str) -> Card {
        Card {
            path: path.into(),
            src_hash: "deadbeef".into(),
            tags: None,
            f: f.into(),
            r: r.into(),
            a: "-".into(),
            s: "-".into(),
        }
    }

    #[test]
    fn zero_hit_falls_back_to_chars() {
        let cards = vec![
            card("src/lock.rs", "文件锁与接管", "-"),
            card("src/web.rs", "HTTP 路由", "-"),
        ];
        let idx = Index::build(&cards);
        // 「锁」是单字，卡片里是「文件锁」：二元组对不上，但字面兜底要能找回来。
        assert!(idx.bm25("锁").is_empty());
        let hits = idx.retrieve("锁", "锁", 3, 0);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].idx, 0);
        assert_eq!(hits[0].why, "cjk");
    }

    #[test]
    fn bm25_finds_card() {
        let cards = vec![
            card("src/tokenize.rs", "分词与倒排构建", "src/search.rs"),
            card("src/render.rs", "终端渲染", "-"),
            card("src/store.rs", "文件存储与锁", "src/tokenize.rs"),
        ];
        let idx = Index::build(&cards);
        let hits = idx.retrieve("分词", "分词", 3, 0);
        assert_eq!(cards[hits[0].idx].path, "src/tokenize.rs");
    }

    #[test]
    fn spread_pulls_neighbours() {
        let cards = vec![
            card("a.rs", "alpha", "b.rs"),
            card("b.rs", "beta", "-"),
            card("c.rs", "gamma", "-"),
        ];
        let idx = Index::build(&cards);
        let hits = idx.retrieve("alpha", "alpha", 1, 3);
        assert!(hits
            .iter()
            .any(|h| cards[h.idx].path == "b.rs" && h.why == "spread"));
    }

    #[test]
    fn in_degree_is_free_importance() {
        let cards = vec![
            card("a.rs", "x", "c.rs"),
            card("b.rs", "y", "c.rs"),
            card("c.rs", "z", "-"),
        ];
        let idx = Index::build(&cards);
        assert_eq!(idx.in_degree(2), 2);
        assert_eq!(idx.in_degree(0), 0);
    }
}
