import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
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

interface SyncStatus {
  enabled: boolean;
  has_pending_changes: boolean;
  version: string;
}

function Document({ isRunning, endpointId }: DocumentProps) {
  const [currentTopic, setCurrentTopic] = useState<TopicInfo | null>(null);
  const [joinInput, setJoinInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [syncStatus, setSyncStatus] = useState<SyncStatus | null>(null);
  const [documentText, setDocumentText] = useState("");
  const [isTyping, setIsTyping] = useState(false);
  const textareaRef = useRef<HTMLTextAreaElement>(null);
  const typingTimeoutRef = useRef<number | null>(null);

  // Container name for the document
  const CONTAINER = "main-doc";

  // Poll for sync status and remote changes
  useEffect(() => {
    if (!isRunning || !currentTopic) return;

    const interval = setInterval(async () => {
      try {
        // Get sync status
        const status = await invoke<SyncStatus>("sync_get_status", {
          topicId: currentTopic.topic_id,
        });
        setSyncStatus(status);

        // Only fetch remote text if we're not actively typing
        if (!isTyping) {
          const text = await invoke<string>("sync_get_text", {
            topicId: currentTopic.topic_id,
            container: CONTAINER,
          });
          
          // Only update if different from current
          if (text !== documentText) {
            setDocumentText(text);
          }
        }
      } catch (e) {
        console.error("Sync poll error:", e);
      }
    }, 500);

    return () => clearInterval(interval);
  }, [isRunning, currentTopic, isTyping, documentText]);

  const createTopic = async () => {
    try {
      setError(null);
      const topic = await invoke<TopicInfo>("create_topic");
      setCurrentTopic(topic);
      setDocumentText("");
      
      // Enable sync for the new topic
      await invoke("sync_enable", { topicId: topic.topic_id });
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
      
      // Enable sync (will trigger initial sync if document is empty)
      await invoke("sync_enable", { topicId: topic.topic_id });
      
      // Fetch current document state
      const text = await invoke<string>("sync_get_text", {
        topicId: topic.topic_id,
        container: CONTAINER,
      });
      setDocumentText(text);
    } catch (e) {
      setError(String(e));
    }
  };

  const leaveTopic = async () => {
    if (!currentTopic) return;
    try {
      await invoke("leave_topic", { topicId: currentTopic.topic_id });
      setCurrentTopic(null);
      setDocumentText("");
      setSyncStatus(null);
    } catch (e) {
      setError(String(e));
    }
  };

  const copyInvite = () => {
    if (currentTopic) {
      navigator.clipboard.writeText(currentTopic.invite_hex);
    }
  };

  // Handle text changes with debouncing
  const handleTextChange = useCallback(async (newText: string) => {
    if (!currentTopic) return;
    
    setDocumentText(newText);
    setIsTyping(true);
    
    // Clear existing timeout
    if (typingTimeoutRef.current) {
      clearTimeout(typingTimeoutRef.current);
    }
    
    // Debounce: wait 300ms after user stops typing
    typingTimeoutRef.current = window.setTimeout(async () => {
      try {
        // For simplicity, we'll replace the entire text
        // A real implementation would compute and send deltas
        await invoke("sync_replace_text", {
          topicId: currentTopic.topic_id,
          container: CONTAINER,
          text: newText,
        });
      } catch (e) {
        console.error("Failed to sync text:", e);
        setError(String(e));
      }
      setIsTyping(false);
    }, 300);
  }, [currentTopic]);

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
              
              {syncStatus && (
                <div className="sync-status">
                  <label>Sync Status</label>
                  <div className="sync-info">
                    <span className={`sync-dot ${syncStatus.enabled ? "enabled" : "disabled"}`} />
                    <span>{syncStatus.enabled ? "Syncing" : "Disabled"}</span>
                  </div>
                  {syncStatus.has_pending_changes && (
                    <div className="pending-indicator">
                      ⏳ Pending changes...
                    </div>
                  )}
                  <div className="version">
                    <label>Version</label>
                    <code>{syncStatus.version.slice(0, 16)}...</code>
                  </div>
                </div>
              )}
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
        {currentTopic ? (
          <div className="editor-container">
            <div className="editor-header">
              <h2>📝 Collaborative Document</h2>
              <div className="editor-status">
                {isTyping && <span className="typing-indicator">Typing...</span>}
                {syncStatus?.has_pending_changes && (
                  <span className="syncing-indicator">Syncing...</span>
                )}
              </div>
            </div>
            <textarea
              ref={textareaRef}
              className="document-editor"
              value={documentText}
              onChange={(e) => handleTextChange(e.target.value)}
              placeholder="Start typing... Your changes will sync automatically with other members."
              spellCheck={false}
            />
            <div className="editor-footer">
              <span className="char-count">{documentText.length} characters</span>
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

