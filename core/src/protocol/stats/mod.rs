//! Stats & Monitoring API for the Protocol
//!
//! Thin API layer that delegates to StatsService.

mod types;
pub(crate) mod service;

pub use types::{
    DhtBucketInfo, DhtNodeInfo, DhtStats, HarborStats, IdentityStats, NetworkStats, OutgoingStats,
    ProtocolStats, TopicDetails, TopicMemberInfo, TopicSummary, TopicsStats,
};
pub(crate) use service::StatsService;
