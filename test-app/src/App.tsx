import { useState, useEffect, useRef } from "react";
import { invoke } from "@tauri-apps/api/core";
import "./App.css";
import Dashboard from "./Dashboard";

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

type Tab = "chat" | "dashboard";

function App() {
  const [isRunning, setIsRunning] = useState(false);
  const [endpointId, setEndpointId] = useState<string | null>(null);
  const [currentTopic, setCurrentTopic] = useState<TopicInfo | null>(null);
  const [messages, setMessages] = useState<Message[]>([]);
  const [messageInput, setMessageInput] = useState("");
  const [joinInput, setJoinInput] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [status, setStatus] = useState("Stopped");
  const [activeTab, setActiveTab] = useState<Tab>("chat");
  const messagesEndRef = useRef<HTMLDivElement>(null);

  // Poll for messages
  useEffect(() => {
    if (!isRunning) return;

    const interval = setInterval(async () => {
      try {
        const newMessages = await invoke<Message[]>("poll_messages");
        if (newMessages.length > 0) {
          setMessages((prev) => {
            const combined = [...prev, ...newMessages];
            // Sort by timestamp
            combined.sort((a, b) => a.timestamp - b.timestamp);
            return combined;
          });
        }
      } catch (e) {
        console.error("Poll error:", e);
      }
    }, 500);

    return () => clearInterval(interval);
  }, [isRunning]);

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
                  {messages.length === 0 ? (
                    <div className="empty-messages">
                      No messages yet. Send one!
                    </div>
                  ) : (
                    messages.map((msg, i) => (
                      <div
                        key={`${msg.timestamp}-${i}`}
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
                    ))
                  )}
                  <div ref={messagesEndRef} />
                </div>

                <form className="message-form" onSubmit={sendMessage}>
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
