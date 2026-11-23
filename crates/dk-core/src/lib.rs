//! dk-core：卡片、存储、检索、体检。所有逻辑在这里，壳只负责转发。
//!
//! 模块职责见 SPEC §9.1。

pub mod agents;
pub mod card;
pub mod check;
pub mod ignore_rules;
pub mod managed;
pub mod mcp;
pub mod search;
pub mod serve;
pub mod store;
pub mod sweep;
pub mod tokenize;

pub use card::Card;
pub use store::{Lock, PutEntry, PutReport, Store};
