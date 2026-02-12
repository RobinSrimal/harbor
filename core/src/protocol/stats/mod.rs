//! Stats & Monitoring API for the Protocol
//!
//! Thin API layer that delegates to StatsService.

pub(crate) mod service;
mod types;

pub(crate) use service::StatsService;
pub use types::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};
