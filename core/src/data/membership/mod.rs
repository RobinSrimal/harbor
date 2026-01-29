//! Membership data layer
//!
//! Handles persistence for topic subscriptions and member management.

pub mod topic;

// Re-export commonly used items
pub use topic::{
    add_topic_member, get_all_topics, get_joined_at, get_topic,
    get_topic_by_harbor_id, get_topic_member_count, get_topic_members,
    get_topics_for_member, is_subscribed, is_topic_member,
    remove_topic_member, set_topic_members, subscribe_topic, unsubscribe_topic,
    TopicMember, TopicSubscription,
};

