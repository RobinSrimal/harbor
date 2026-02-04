//! Membership data layer
//!
//! Handles persistence for topic subscriptions, member management, and epoch keys.

pub mod epoch;
pub mod topic;

// Re-export commonly used items
pub use topic::{
    add_topic_member, get_all_topics, get_joined_at, get_topic,
    get_topic_admin, get_topic_by_harbor_id, get_topic_member_count, get_topic_members,
    get_topics_for_member, is_subscribed, is_topic_admin, is_topic_member,
    remove_topic_member, set_topic_members, subscribe_topic, subscribe_topic_with_admin,
    unsubscribe_topic, TopicMember, TopicSubscription,
};

// Re-export epoch key functions
pub use epoch::{
    cleanup_expired_epoch_keys, delete_epoch_keys_for_topic, get_all_epoch_keys,
    get_current_epoch, get_current_epoch_key, get_epoch_key, store_epoch_key,
    EpochKey, EPOCH_KEY_LIFETIME_SECS,
};
