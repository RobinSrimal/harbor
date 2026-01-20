import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import "./App.css";
import Dashboard from "./Dashboard";
import Document from "./Document";

interface Message {
  topic_id: string;
  sender_id: string;
  payload: string;
  timestamp: number;
}

interface TopicInfo {
  topic_id: string;
  members: string[];
  invite_hex: string;
}

// File sharing types
interface ShareResponse {
  hash: string;
  display_name: string;
  total_size: number;
  total_chunks: number;
}

// Combined event type (matching Rust's #[serde(tag = "type")])
type AppEvent =
  | { type: "Message"; topic_id: string; sender_id: string; payload: string; timestamp: number }
  | { type: "FileAnnounced"; topic_id: string; source_id: string; hash: string; display_name: string; total_size: number; total_chunks: number; timestamp: number }
  | { type: "FileProgress"; hash: string; chunks_complete: number; total_chunks: number }
  | { type: "FileComplete"; hash: string; display_name: string; total_size: number };

// Track file transfer state
interface FileTransfer {
  hash: string;
  topic_id: string;
  source_id: string;
  display_name: string;
  total_size: number;
  total_chunks: number;
  chunks_complete: number;
  timestamp: number;
  isComplete: boolean;
}

// Chat item can be message or file
type ChatItem =
  | { type: "message"; data: Message }
  | { type: "file"; data: FileTransfer };

type Tab = "chat" | "document" | "dashboard";

function App() {
  const [isRunning, setIsRunning] = useState(false);
  const [endpointId, setEndpointId] = useState<string | null>(null);
  const [currentTopic, setCurrentTopic] = useState<TopicInfo | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [fileTransfers, setFileTransfers] = useState<Map<string, FileTransfer>>(new Map());
  const [chatItems, setChatItems] = useState<ChatItem[]>([]);
  const [messageInput, setMessageInput] = useState("");
  const [joinInput, setJoinInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState("Stopped");
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Poll for events (messages + files)
  useEffect(() => {
    if (!isRunning) return;

    const interval = setInterval(async () => {
      try {
        const events = await invoke<AppEvent[]>("poll_events");
        
        for (const event of events) {
          switch (event.type) {
            case "Message":
              setMessages((prev) => {
                const combined = [...prev, {
                  topic_id: event.topic_id,
                  sender_id: event.sender_id,
                  payload: event.payload,
                  timestamp: event.timestamp,
                }];
                combined.sort((a, b) => a.timestamp - b.timestamp);
                return combined;
              });
              break;
              
            case "FileAnnounced":
              setFileTransfers((prev) => {
                const next = new Map(prev);
                next.set(event.hash, {
                  hash: event.hash,
                  topic_id: event.topic_id,
                  source_id: event.source_id,
                  display_name: event.display_name,
                  total_size: event.total_size,
                  total_chunks: event.total_chunks,
                  chunks_complete: 0,
                  timestamp: event.timestamp,
                  isComplete: false,
                });
                return next;
              });
              break;
              
            case "FileProgress":
              setFileTransfers((prev) => {
                const transfer = prev.get(event.hash);
                if (transfer) {
                  const next = new Map(prev);
                  next.set(event.hash, {
                    ...transfer,
                    chunks_complete: event.chunks_complete,
                  });
                  return next;
                }
                return prev;
              });
              break;
              
            case "FileComplete":
              setFileTransfers((prev) => {
                const transfer = prev.get(event.hash);
                if (transfer) {
                  const next = new Map(prev);
                  next.set(event.hash, {
                    ...transfer,
                    chunks_complete: transfer.total_chunks,
                    isComplete: true,
                  });
                  return next;
                }
                return prev;
              });
              break;
          }
        }
      } catch (e) {
        console.error("Poll error:", e);
      }
    }, 500);

    return () => clearInterval(interval);
  }, [isRunning]);

  // Combine messages and file transfers into chat items
  useEffect(() => {
    const items: ChatItem[] = [
      ...messages.map((m): ChatItem => ({ type: "message", data: m })),
      ...Array.from(fileTransfers.values()).map((f): ChatItem => ({ type: "file", data: f })),
    ];
    // Sort by timestamp
    items.sort((a, b) => {
      const aTime = a.type === "message" ? a.data.timestamp : a.data.timestamp;
      const bTime = b.type === "message" ? b.data.timestamp : b.data.timestamp;
      return aTime - bTime;
    });
    setChatItems(items);
  }, [messages, fileTransfers]);

  // Auto-scroll to bottom
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  const startProtocol = async () => {
    try {
      setError(null);
      setStatus("Starting...");
      const result = await invoke<{ endpoint_id: string }>("start_protocol");
      setEndpointId(result.endpoint_id);
      setIsRunning(true);
      setStatus("Running");
    } catch (e) {
      setError(String(e));
      setStatus("Error");
    }
  };

  const stopProtocol = async () => {
    try {
      await invoke("stop_protocol");
      setIsRunning(false);
      setEndpointId(null);
      setCurrentTopic(null);
      setMessages([]);
      setFileTransfers(new Map());
      setStatus("Stopped");
    } catch (e) {
      setError(String(e));
    }
  };

  const createTopic = async () => {
    try {
      setError(null);
      const topic = await invoke<TopicInfo>("create_topic");
      setCurrentTopic(topic);
      setMessages([]);
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
      setMessages([]);
      setJoinInput("");
    } catch (e) {
      setError(String(e));
    }
  };

  const leaveTopic = async () => {
    if (!currentTopic) return;
    try {
      await invoke("leave_topic", { topicId: currentTopic.topic_id });
      setCurrentTopic(null);
      setMessages([]);
    } catch (e) {
      setError(String(e));
    }
  };

  const sendMessage = async (e: React.FormEvent) => {
    e.preventDefault();
    if (!messageInput.trim() || !currentTopic) return;

    try {
      setError(null);
      await invoke("send_message", {
        topicId: currentTopic.topic_id,
        message: messageInput,
      });
      // Add our own message to the list
      setMessages((prev) => [
        ...prev,
        {
          topic_id: currentTopic.topic_id,
          sender_id: endpointId || "",
          payload: messageInput,
          timestamp: Math.floor(Date.now() / 1000),
        },
      ]);
      setMessageInput("");
    } catch (e) {
      setError(String(e));
    }
  };

  // File sharing functions
  const shareFile = async () => {
    if (!currentTopic) return;
    
    try {
      const selected = await open({
        multiple: false,
        title: "Select file to share",
      });
      
      if (!selected) return;
      
      setError(null);
      const result = await invoke<ShareResponse>("share_file", {
        topicId: currentTopic.topic_id,
        filePath: selected,
      });
      
      // Add to our own file transfers
      setFileTransfers((prev) => {
        const next = new Map(prev);
        next.set(result.hash, {
          hash: result.hash,
          topic_id: currentTopic.topic_id,
          source_id: endpointId || "",
          display_name: result.display_name,
          total_size: result.total_size,
          total_chunks: result.total_chunks,
          chunks_complete: result.total_chunks, // We have all chunks
          timestamp: Math.floor(Date.now() / 1000),
          isComplete: true,
        });
        return next;
      });
    } catch (e) {
      setError(String(e));
    }
  };

  const exportFile = async (hash: string, displayName: string) => {
    try {
      const destPath = await save({
        title: "Save file",
        defaultPath: displayName,
      });
      
      if (!destPath) return;
      
      await invoke("export_file", {
        hash,
        destination: destPath,
      });
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

  const formatTime = (timestamp: number) => {
    const date = new Date(timestamp * 1000);
    return date.toLocaleTimeString();
  };

  const formatBytes = (bytes: number) => {
    if (bytes < 1024) return `${bytes} B`;
    if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  const isOwnMessage = (senderId: string) => senderId === endpointId;

  return (
    <div className="app">
      <header className="header">
        <h1>⚓ Harbor</h1>
        {isRunning && (
          <nav className="tabs">
            <button
              className={`tab ${activeTab === "chat" ? "active" : ""}`}
              onClick={() => setActiveTab("chat")}
            >
              💬 Chat
            </button>
            <button
              className={`tab ${activeTab === "document" ? "active" : ""}`}
              onClick={() => setActiveTab("document")}
            >
              📝 Document
            </button>
            <button
              className={`tab ${activeTab === "dashboard" ? "active" : ""}`}
              onClick={() => setActiveTab("dashboard")}
            >
              📊 Dashboard
            </button>
          </nav>
        )}
        <div className="status">
          <span className={`status-dot ${isRunning ? "running" : "stopped"}`} />
          <span>{status}</span>
        </div>
      </header>

      {error && <div className="error">{error}</div>}

      {!isRunning ? (
        <div className="start-panel">
          <p>Start the Harbor Protocol to begin messaging</p>
          <button className="btn primary" onClick={startProtocol}>
            Start Protocol
          </button>
        </div>
      ) : activeTab === "dashboard" ? (
        <Dashboard isRunning={isRunning} />
      ) : activeTab === "document" ? (
        <Document isRunning={isRunning} endpointId={endpointId} />
      ) : (
        <div className="main-content">
          <aside className="sidebar">
            <div className="info-section">
              <label>Your ID</label>
              <code>{shortId(endpointId || "")}</code>
            </div>

            <div className="topic-section">
              <h3>Topic</h3>
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
                    Create Topic
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

            <button className="btn danger stop-btn" onClick={stopProtocol}>
              Stop Protocol
            </button>
          </aside>

          <main className="chat-area">
            {currentTopic ? (
              <>
                <div className="messages">
                  {chatItems.length === 0 ? (
                    <div className="empty-messages">
                      No messages yet. Send one!
                    </div>
                  ) : (
                    chatItems.map((item, i) => {
                      if (item.type === "message") {
                        const msg = item.data;
                        return (
                          <div
                            key={`msg-${msg.timestamp}-${i}`}
                            className={`message ${isOwnMessage(msg.sender_id) ? "own" : ""}`}
                          >
                            <div className="message-header">
                              <span className="sender">
                                {isOwnMessage(msg.sender_id)
                                  ? "You"
                                  : shortId(msg.sender_id)}
                              </span>
                              <span className="time">{formatTime(msg.timestamp)}</span>
                            </div>
                            <div className="message-body">{msg.payload}</div>
                          </div>
                        );
                      } else {
                        const file = item.data;
                        const progress = file.total_chunks > 0 
                          ? Math.round((file.chunks_complete / file.total_chunks) * 100)
                          : 0;
                        return (
                          <div
                            key={`file-${file.hash}`}
                            className={`message file-message ${isOwnMessage(file.source_id) ? "own" : ""}`}
                          >
                            <div className="message-header">
                              <span className="sender">
                                {isOwnMessage(file.source_id)
                                  ? "You"
                                  : shortId(file.source_id)}
                              </span>
                              <span className="time">{formatTime(file.timestamp)}</span>
                            </div>
                            <div className="file-content">
                              <div className="file-icon">📄</div>
                              <div className="file-info">
                                <div className="file-name">{file.display_name}</div>
                                <div className="file-size">{formatBytes(file.total_size)}</div>
                                {!file.isComplete && (
                                  <div className="file-progress">
                                    <div 
                                      className="file-progress-bar"
                                      style={{ width: `${progress}%` }}
                                    />
                                    <span className="file-progress-text">{progress}%</span>
                                  </div>
                                )}
                                {file.isComplete && (
                                  <button 
                                    className="btn small file-save-btn"
                                    onClick={() => exportFile(file.hash, file.display_name)}
                                  >
                                    💾 Save
                                  </button>
                                )}
                              </div>
                            </div>
                          </div>
                        );
                      }
                    })
                  )}
                  <div ref={messagesEndRef} />
                </div>

                <form className="message-form" onSubmit={sendMessage}>
                  <button 
                    type="button" 
                    className="btn attach-btn"
                    onClick={shareFile}
                    title="Share file"
                  >
                    📎
                  </button>
                  <input
                    type="text"
                    placeholder="Type a message..."
                    value={messageInput}
                    onChange={(e) => setMessageInput(e.target.value)}
                    autoFocus
                  />
                  <button type="submit" className="btn primary">
                    Send
                  </button>
                </form>
              </>
            ) : (
              <div className="no-topic-selected">
                <p>Create or join a topic to start chatting</p>
              </div>
            )}
          </main>
        </div>
      )}
    </div>
  );
}

export default App;
