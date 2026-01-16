import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./Dashboard.css";

// Types matching the Rust structs
interface IdentityStats {
  endpoint_id: string;
  relay_url: string | null;
}

interface NetworkStats {
  is_online: boolean;
  active_connections: number;
}

interface DhtStats {
  total_nodes: number;
  active_buckets: number;
  bootstrap_connected: boolean;
}

interface TopicsStats {
  subscribed_count: number;
  total_members: number;
}

interface OutgoingStats {
  pending_count: number;
  awaiting_receipts: number;
  replicated_to_harbor: number;
}

interface HarborStats {
  packets_stored: number;
  storage_bytes: number;
  harbor_ids_served: number;
}

interface ProtocolStats {
  identity: IdentityStats;
  network: NetworkStats;
  dht: DhtStats;
  topics: TopicsStats;
  outgoing: OutgoingStats;
  harbor: HarborStats;
}

interface DhtNodeInfo {
  endpoint_id: string;
  address: string | null;
  relay_url: string | null;
  is_fresh: boolean;
}

interface DhtBucketInfo {
  bucket_index: number;
  node_count: number;
  nodes: DhtNodeInfo[];
}

interface TopicMemberInfo {
  endpoint_id: string;
  relay_url: string | null;
  is_self: boolean;
}

interface TopicDetails {
  topic_id: string;
  harbor_id: string;
  members: TopicMemberInfo[];
  harbor_node_count: number;
}

interface TopicSummary {
  topic_id: string;
  member_count: number;
}

interface DashboardProps {
  isRunning: boolean;
}

function Dashboard({ isRunning }: DashboardProps) {
  const [stats, setStats] = useState<ProtocolStats | null>(null);
  const [dhtBuckets, setDhtBuckets] = useState<DhtBucketInfo[]>([]);
  const [topics, setTopics] = useState<TopicSummary[]>([]);
  const [selectedTopic, setSelectedTopic] = useState<TopicDetails | null>(null);
  const [expandedBuckets, setExpandedBuckets] = useState<Set<number>>(new Set());
  const [lastUpdate, setLastUpdate] = useState<Date | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Poll stats every 5 seconds
  useEffect(() => {
    if (!isRunning) {
      setStats(null);
      setDhtBuckets([]);
      setTopics([]);
      setSelectedTopic(null);
      return;
    }

    const fetchStats = async () => {
      try {
        const [statsResult, bucketsResult, topicsResult] = await Promise.all([
          invoke<ProtocolStats>("get_stats"),
          invoke<DhtBucketInfo[]>("get_dht_buckets"),
          invoke<TopicSummary[]>("list_topic_summaries"),
        ]);
        setStats(statsResult);
        setDhtBuckets(bucketsResult);
        setTopics(topicsResult);
        setLastUpdate(new Date());
        setError(null);
      } catch (e) {
        setError(String(e));
      }
    };

    fetchStats();
    const interval = setInterval(fetchStats, 5000);
    return () => clearInterval(interval);
  }, [isRunning]);

  const fetchTopicDetails = async (topicId: string) => {
    try {
      const details = await invoke<TopicDetails>("get_topic_details", { topicId });
      setSelectedTopic(details);
    } catch (e) {
      setError(String(e));
    }
  };

  const toggleBucket = (index: number) => {
    setExpandedBuckets((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  };

  const shortId = (id: string) => id.slice(0, 12) + "...";
  
  const formatBytes = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  if (!isRunning) {
    return (
      <div className="dashboard">
        <div className="dashboard-empty">
          <p>Start the protocol to view stats</p>
        </div>
      </div>
    );
  }

  return (
    <div className="dashboard">
      {error && <div className="dashboard-error">{error}</div>}
      
      {lastUpdate && (
        <div className="last-update">
          Last updated: {lastUpdate.toLocaleTimeString()}
        </div>
      )}

      {stats && (
        <div className="stats-grid">
          {/* Identity Card */}
          <div className="stat-card identity">
            <h3>🔑 Identity</h3>
            <div className="stat-row">
              <label>Endpoint ID</label>
              <code title={stats.identity.endpoint_id}>
                {shortId(stats.identity.endpoint_id)}
              </code>
            </div>
            <div className="stat-row">
              <label>Relay</label>
              <span className={stats.identity.relay_url ? "connected" : "disconnected"}>
                {stats.identity.relay_url ? "✅ Connected" : "❌ Not connected"}
              </span>
            </div>
            {stats.identity.relay_url && (
              <div className="stat-row small">
                <code>{stats.identity.relay_url}</code>
              </div>
            )}
          </div>

          {/* Network Card */}
          <div className="stat-card network">
            <h3>🌐 Network</h3>
            <div className="stat-row">
              <label>Status</label>
              <span className={stats.network.is_online ? "online" : "offline"}>
                {stats.network.is_online ? "🟢 Online" : "🔴 Offline"}
              </span>
            </div>
          </div>

          {/* DHT Card */}
          <div className="stat-card dht">
            <h3>🗂️ DHT</h3>
            <div className="stat-row">
              <label>Total Nodes</label>
              <span className="number">{stats.dht.total_nodes}</span>
            </div>
            <div className="stat-row">
              <label>Active Buckets</label>
              <span className="number">{stats.dht.active_buckets}</span>
            </div>
            <div className="stat-row">
              <label>Bootstrap</label>
              <span className={stats.dht.bootstrap_connected ? "connected" : "disconnected"}>
                {stats.dht.bootstrap_connected ? "✅ Connected" : "⏳ Pending"}
              </span>
            </div>
          </div>

          {/* Topics Card */}
          <div className="stat-card topics">
            <h3>💬 Topics</h3>
            <div className="stat-row">
              <label>Subscribed</label>
              <span className="number">{stats.topics.subscribed_count}</span>
            </div>
            <div className="stat-row">
              <label>Total Members</label>
              <span className="number">{stats.topics.total_members}</span>
            </div>
          </div>

          {/* Outgoing Card */}
          <div className="stat-card outgoing">
            <h3>📤 Outgoing</h3>
            <div className="stat-row">
              <label>Pending</label>
              <span className="number">{stats.outgoing.pending_count}</span>
            </div>
            <div className="stat-row">
              <label>Awaiting Receipts</label>
              <span className="number">{stats.outgoing.awaiting_receipts}</span>
            </div>
          </div>

          {/* Harbor Card */}
          <div className="stat-card harbor">
            <h3>⚓ Harbor</h3>
            <div className="stat-row">
              <label>Packets Stored</label>
              <span className="number">{stats.harbor.packets_stored}</span>
            </div>
            <div className="stat-row">
              <label>Storage Used</label>
              <span className="number">{formatBytes(stats.harbor.storage_bytes)}</span>
            </div>
            <div className="stat-row">
              <label>Harbor IDs</label>
              <span className="number">{stats.harbor.harbor_ids_served}</span>
            </div>
          </div>
        </div>
      )}

      {/* DHT Buckets Section */}
      <div className="section">
        <h2>🗂️ DHT Routing Table</h2>
        {dhtBuckets.length === 0 ? (
          <p className="empty">No nodes in DHT yet</p>
        ) : (
          <div className="buckets-list">
            {dhtBuckets.map((bucket) => (
              <div key={bucket.bucket_index} className="bucket">
                <div
                  className="bucket-header"
                  onClick={() => toggleBucket(bucket.bucket_index)}
                >
                  <span className="bucket-index">Bucket {bucket.bucket_index}</span>
                  <span className="bucket-count">{bucket.node_count} nodes</span>
                  <span className="expand-icon">
                    {expandedBuckets.has(bucket.bucket_index) ? "▼" : "▶"}
                  </span>
                </div>
                {expandedBuckets.has(bucket.bucket_index) && (
                  <div className="bucket-nodes">
                    {bucket.nodes.map((node) => (
                      <div key={node.endpoint_id} className="node">
                        <code className="node-id">{shortId(node.endpoint_id)}</code>
                        <span className={`freshness ${node.is_fresh ? "fresh" : "stale"}`}>
                          {node.is_fresh ? "🟢" : "🟡"}
                        </span>
                        {node.address && (
                          <span className="node-addr">{node.address}</span>
                        )}
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Topics Section */}
      <div className="section">
        <h2>💬 Subscribed Topics</h2>
        {topics.length === 0 ? (
          <p className="empty">No topics subscribed</p>
        ) : (
          <div className="topics-list">
            {topics.map((topic) => (
              <div
                key={topic.topic_id}
                className={`topic-item ${selectedTopic?.topic_id === topic.topic_id ? "selected" : ""}`}
                onClick={() => fetchTopicDetails(topic.topic_id)}
              >
                <code className="topic-id">{shortId(topic.topic_id)}</code>
                <span className="member-count">{topic.member_count} members</span>
              </div>
            ))}
          </div>
        )}

        {selectedTopic && (
          <div className="topic-details">
            <h3>Topic Details</h3>
            <div className="detail-row">
              <label>Topic ID</label>
              <code>{selectedTopic.topic_id}</code>
            </div>
            <div className="detail-row">
              <label>Harbor ID</label>
              <code>{shortId(selectedTopic.harbor_id)}</code>
            </div>
            <div className="members-list">
              <label>Members ({selectedTopic.members.length})</label>
              {selectedTopic.members.map((member) => (
                <div key={member.endpoint_id} className="member-item">
                  <code>{shortId(member.endpoint_id)}</code>
                  {member.is_self && <span className="self-badge">YOU</span>}
                  {member.relay_url && (
                    <span className="relay-badge" title={member.relay_url}>
                      📡
                    </span>
                  )}
                </div>
              ))}
            </div>
          </div>
        )}
      </div>
    </div>
  );
}

export default Dashboard;

