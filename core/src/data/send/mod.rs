//! Send protocol data layer
//!
//! Handles persistence for outgoing packets and delivery tracking.

pub mod outgoing;

// Re-export commonly used items
pub use outgoing::{
    acknowledge_and_cleanup_if_complete, acknowledge_receipt, all_recipients_acknowledged,
    cleanup_acknowledged_packets, delete_outgoing_packet, get_outgoing_packet,
    get_packets_needing_replication, get_unacknowledged_recipients, mark_replicated_to_harbor,
    store_outgoing_packet, OutgoingPacket, RecipientStatus,
};

