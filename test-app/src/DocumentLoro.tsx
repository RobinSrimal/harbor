import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import { LoroDoc } from "loro-crdt";
import { LoroEditor } from "./LoroEditor";
import "./Document.css";

interface DocumentProps {
  isRunning: boolean;
  endpointId: string | null;
}

interface TopicInfo {
  topic_id: string;
  members: string[];
  invite_hex: string;
}

interface SyncUpdateEvent {
  topic_id: string;
  sender_id: string;
  data: number[]; // Array of bytes
}

interface SyncRequestEvent {
  topic_id: string;
  sender_id: string;
}

interface SyncResponseEvent {
  topic_id: string;
  data: number[]; // Array of bytes
}

type AppEvent =
  | { type: "SyncUpdate" } & SyncUpdateEvent
  | { type: "SyncRequest" } & SyncRequestEvent
  | { type: "SyncResponse" } & SyncResponseEvent
  | { type: string; [key: string]: any };

function Document({ isRunning, endpointId }: DocumentProps) {
  const [currentTopic, setCurrentTopic] = useState<TopicInfo | null>(null);
  const [joinInput, setJoinInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [loroDoc, setLoroDoc] = useState<LoroDoc | null>(null);

  // Container name for the document
  const CONTAINER = "main-doc";

  // Store version vector before sending updates
  const lastSentVersionRef = useRef<any>(null);

  // Initialize Loro document when topic is created/joined
  useEffect(() => {
    if (currentTopic && !loroDoc) {
      const doc = new LoroDoc();

      // Subscribe to local changes and send updates
      doc.subscribe((event) => {
        if (!currentTopic) return;

        // Export delta since last sent version
        const currentVersion = doc.oplogVersion();
        const delta = doc.export({ mode: "update", from: lastSentVersionRef.current });

        if (delta.length > 0) {
          // Send update to Harbor
          invoke("sync_send_update", {
            topicId: currentTopic.topic_id,
            data: Array.from(delta),
          }).catch((e) => {
            console.error("Failed to send sync update:", e);
          });

          // Update last sent version
          lastSentVersionRef.current = currentVersion;
        }
      });

      setLoroDoc(doc);

      // Request initial sync if joining existing topic
      invoke("sync_request", { topicId: currentTopic.topic_id }).catch((e) => {
        console.error("Failed to request sync:", e);
      });
    }
  }, [currentTopic, loroDoc]);

  // Poll for Harbor events
  useEffect(() => {
    if (!isRunning || !currentTopic || !loroDoc) return;

    const interval = setInterval(async () => {
      try {
        const events = await invoke<AppEvent[]>("poll_events");

        for (const event of events) {
          if (event.type === "SyncUpdate" && event.topic_id === currentTopic.topic_id) {
            // Apply the update to our Loro document
            const updateBytes = new Uint8Array(event.data);
            loroDoc.import(updateBytes);
          } else if (event.type === "SyncRequest" && event.topic_id === currentTopic.topic_id) {
            // Someone is requesting our state - export and send
            const snapshot = loroDoc.export({ mode: "snapshot" });
            await invoke("sync_respond", {
              topicId: event.topic_id,
              requesterId: event.sender_id,
              data: Array.from(snapshot),
            });
          } else if (event.type === "SyncResponse" && event.topic_id === currentTopic.topic_id) {
            // Received full state in response to our request
            const snapshotBytes = new Uint8Array(event.data);
            loroDoc.import(snapshotBytes);
          }
        }
      } catch (e) {
        console.error("Event poll error:", e);
      }
    }, 200); // Poll every 200ms

    return () => clearInterval(interval);
  }, [isRunning, currentTopic, loroDoc]);

  const createTopic = async () => {
    try {
      setError(null);
      const topic = await invoke<TopicInfo>("create_topic");
      setCurrentTopic(topic);
      setLoroDoc(null); // Will be recreated in useEffect
    } catch (e) {
      setError(String(e));
    }
  };

  const joinTopic = async () => {
    if (!joinInput.trim()) return;
    try {
      setError(null);
      const topic = await invoke<TopicInfo>("join_topic", {
        inviteHex: joinInput.trim(),
      });
      setCurrentTopic(topic);
      setJoinInput("");
      setLoroDoc(null); // Will be recreated in useEffect
    } catch (e) {
      setError(String(e));
    }
  };

  const leaveTopic = async () => {
    if (!currentTopic) return;
    try {
      await invoke("leave_topic", { topicId: currentTopic.topic_id });
      setCurrentTopic(null);
      setLoroDoc(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const copyInvite = () => {
    if (currentTopic) {
      navigator.clipboard.writeText(currentTopic.invite_hex);
    }
  };

  const shortId = (id: string) => id.slice(0, 8) + "...";

  return (
    <div className="document-page">
      {error && <div className="error">{error}</div>}

      <aside className="sidebar">
        <div className="info-section">
          <label>Your ID</label>
          <code>{shortId(endpointId || "")}</code>
        </div>

        <div className="topic-section">
          <h3>Document Topic</h3>
          {currentTopic ? (
            <div className="current-topic">
              <div className="topic-id">
                <label>Topic ID</label>
                <code>{shortId(currentTopic.topic_id)}</code>
              </div>
              <div className="topic-actions">
                <button className="btn small" onClick={copyInvite}>
                  Copy Invite
                </button>
                <button className="btn small danger" onClick={leaveTopic}>
                  Leave
                </button>
              </div>
              <div className="members">
                <label>Members ({currentTopic.members.length})</label>
                {currentTopic.members.map((m) => (
                  <div key={m} className="member">
                    {shortId(m)}
                    {m === endpointId && " (you)"}
                  </div>
                ))}
              </div>
            </div>
          ) : (
            <div className="no-topic">
              <button className="btn" onClick={createTopic}>
                Create Document
              </button>
              <div className="divider">or</div>
              <input
                type="text"
                placeholder="Paste invite..."
                value={joinInput}
                onChange={(e) => setJoinInput(e.target.value)}
              />
              <button className="btn" onClick={joinTopic}>
                Join
              </button>
            </div>
          )}
        </div>
      </aside>

      <main className="editor-area">
        {currentTopic && loroDoc ? (
          <div className="editor-container">
            <div className="editor-header">
              <h2>📝 Collaborative Document</h2>
            </div>
            <LoroEditor doc={loroDoc} containerName={CONTAINER} />
            <div className="editor-footer">
              <span className="info">Syncing via Harbor Protocol with Loro CRDT</span>
            </div>
          </div>
        ) : (
          <div className="no-document">
            <div className="no-document-content">
              <span className="icon">📄</span>
              <h2>No Document Open</h2>
              <p>Create a new document or join an existing one to start collaborating</p>
            </div>
          </div>
        )}
      </main>
    </div>
  );
}

export default Document;
