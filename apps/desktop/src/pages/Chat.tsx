import { useCallback, useEffect, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useChat, useLoadedModels, useModelLoader } from "../hooks/useInference";
import { useInstalledModels } from "../hooks/useRegistry";
import { useConversations } from "../hooks/useConversations";
import ChatMessage from "../components/ChatMessage";
import ChatInput from "../components/ChatInput";
import ConversationDrawer from "../components/ConversationDrawer";
import { DEFAULT_CHAT_PARAMS, type ChatParams } from "../components/ChatParamsPanel";
import type { DisplayMessage } from "../hooks/useInference";
import { PanelLeft, Plus, Loader, Sparkles, Globe, RefreshCw } from "lucide-react";
import { connectMeshPeer, discoverMeshPeers, type DiscoveredMeshPeer } from "../lib/invoke";
import { EmptyState } from "../components/ui";

export default function Chat() {
  const navigate = useNavigate();
  const { models: loaded, refresh: refreshLoaded } = useLoadedModels();
  const { models: installed } = useInstalledModels();
  const { loading: modelLoading, error: loadError, load } = useModelLoader();
  const [activeHandleId, setActiveHandleId] = useState<number | null>(null);
  const { messages, streaming, send, stop, regenerate, editAndResend, clear, setMessages } =
    useChat(activeHandleId);
  const [input, setInput] = useState("");
  const [chatParams, setChatParams] = useState<ChatParams>({ ...DEFAULT_CHAT_PARAMS });
  const bottomRef = useRef<HTMLDivElement>(null);

  // Mesh PC connection state
  const [showMeshModal, setShowMeshModal] = useState(false);
  const [meshPeerInput, setMeshPeerInput] = useState("192.168.0.4:7878");
  const [meshModelName, setMeshModelName] = useState("");
  const [meshConnecting, setMeshConnecting] = useState(false);
  const [meshError, setMeshError] = useState<string | null>(null);
  const [discoveredPeers, setDiscoveredPeers] = useState<DiscoveredMeshPeer[]>([]);
  const [discoveringPeers, setDiscoveringPeers] = useState(false);

  const scanMeshPeers = useCallback(async () => {
    setDiscoveringPeers(true);
    try {
      const peers = await discoverMeshPeers();
      setDiscoveredPeers(peers);
      if (peers.length > 0) {
        const first = peers[0];
        setMeshPeerInput(first.addr);
        if (first.serving_model_id) {
          setMeshModelName(first.serving_model_id);
        }
      }
    } catch {
      // ignore discovery errors
    } finally {
      setDiscoveringPeers(false);
    }
  }, []);

  useEffect(() => {
    if (showMeshModal || loaded.length === 0) {
      scanMeshPeers();
    }
  }, [showMeshModal, loaded.length, scanMeshPeers]);

  const handleConnectPeerDirect = useCallback(async (peerAddr: string, peerModel?: string | null) => {
    if (!peerAddr.trim()) return;
    setMeshConnecting(true);
    setMeshError(null);
    try {
      const res = await connectMeshPeer(peerAddr.trim(), peerModel?.trim() || undefined);
      setActiveHandleId(res.handle_id);
      await refreshLoaded();
      setShowMeshModal(false);
    } catch (err: any) {
      setMeshError(typeof err === "string" ? err : err?.message || "Error al conectar al nodo Mesh");
    } finally {
      setMeshConnecting(false);
    }
  }, [refreshLoaded]);

  const handleConnectMesh = useCallback(async () => {
    await handleConnectPeerDirect(meshPeerInput, meshModelName);
  }, [handleConnectPeerDirect, meshPeerInput, meshModelName]);

  // Conversation drawer (open by default on desktop, closed by default on mobile)
  const [drawerOpen, setDrawerOpen] = useState(() =>
    typeof window !== "undefined" ? window.innerWidth >= 768 : true
  );

  // Conversation persistence
  const {
    conversations, activeId, setActiveId, loading: convsLoading,
    create, remove, rename, getMessages, addMessage, refresh: refreshConvs,
  } = useConversations();

  // Editing state
  const [editingIndex, setEditingIndex] = useState<number | null>(null);
  const [editText, setEditText] = useState("");

  useEffect(() => {
    if (loaded.length > 0 && activeHandleId === null) {
      setActiveHandleId(loaded[0].handle_id);
    }
  }, [loaded, activeHandleId]);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  // Load messages when conversation changes
  useEffect(() => {
    if (!activeId) {
      clear();
      return;
    }
      (async () => {
        try {
          const persisted = await getMessages(activeId);
          const displayMsgs: DisplayMessage[] = persisted.map((m) => {
            const roleStr = String(m.role).toLowerCase();
            return {
              role: roleStr === "user" ? "user" : roleStr === "assistant" ? "assistant" : "system",
              content: m.content,
            };
          });
          setMessages(displayMsgs);
        } catch (e) {
          console.error("Failed to load messages:", e);
        }
      })();
  }, [activeId, getMessages, clear, setMessages]);

  const handleSend = useCallback(async (text?: string) => {
    const msgText = (text ?? input).trim();
    if (!msgText) return;
    if (!text) setInput("");

    // Auto-create conversation if none is active
    let convId = activeId;
    if (!convId && loaded.length > 0) {
      const activeModel = loaded.find((m) => m.handle_id === activeHandleId);
      const modelName = activeModel ? activeModel.model_path.split(/[/\\]/).pop() || "chat" : "chat";
      const title = msgText.length > 40 ? msgText.slice(0, 40) + "..." : msgText;
      try {
        const conv = await create(title, modelName);
        convId = conv.id;
      } catch (err) {
        console.error("Failed to create conversation:", err);
      }
    }

    // Persist user message
    if (convId) {
      try { await addMessage(convId, "user", msgText); } catch { /* ignore */ }
    }

    // Send via inference
    const assistantReply = await send(msgText, {
      temperature: chatParams.temperature,
      maxTokens: chatParams.maxTokens,
    });

    // Persist assistant reply
    if (convId && assistantReply) {
      try {
        await addMessage(convId, "assistant", assistantReply);
        refreshConvs();
      } catch { /* ignore */ }
    }
  }, [input, activeId, loaded, activeHandleId, create, addMessage, send, refreshConvs, chatParams]);

  const handleLoadModel = useCallback(async (modelId: string) => {
    const result = await load(modelId);
    if (result) { setActiveHandleId(result.handle_id); refreshLoaded(); }
  }, [load, refreshLoaded]);

  const handleNewChat = useCallback(() => {
    setActiveId(null);
    clear();
    setDrawerOpen(false);
  }, [setActiveId, clear]);

  const handleSelectConversation = useCallback((id: string) => {
    setActiveId(id);
  }, [setActiveId]);

  const handleDeleteConversation = useCallback(async (id: string) => {
    try { await remove(id); } catch { /* ignore */ }
  }, [remove]);

  const handleRenameConversation = useCallback(async (id: string, title: string) => {
    try { await rename(id, title); } catch { /* ignore */ }
  }, [rename]);

  const handleRegenerate = useCallback(async () => {
    const reply = await regenerate({
      temperature: chatParams.temperature,
      maxTokens: chatParams.maxTokens,
    });
    if (activeId && reply) {
      try {
        await addMessage(activeId, "assistant", reply);
        refreshConvs();
      } catch { /* ignore */ }
    }
  }, [regenerate, activeId, addMessage, refreshConvs, chatParams]);

  const handleStartEdit = useCallback((index: number) => {
    setEditingIndex(index);
    setEditText(messages[index].content);
  }, [messages]);

  const handleSubmitEdit = useCallback(async () => {
    if (editingIndex === null) return;
    const reply = await editAndResend(editingIndex, editText, {
      temperature: chatParams.temperature,
      maxTokens: chatParams.maxTokens,
    });
    setEditingIndex(null);
    setEditText("");
    if (activeId && reply) {
      try {
        await addMessage(activeId, "assistant", reply);
        refreshConvs();
      } catch { /* ignore */ }
    }
  }, [editingIndex, editText, editAndResend, activeId, addMessage, refreshConvs, chatParams]);

  // Derive model name for display
  const activeModel = loaded.find((m) => m.handle_id === activeHandleId);
  const modelName = activeModel ? activeModel.model_path.split("/").pop() || "Model" : null;

  const loadedModelOptions = loaded.map((m) => ({
    handle_id: m.handle_id,
    label: m.model_path.split("/").pop() || "Model",
    engine: m.engine,
  }));

  return (
    <div className="flex h-full w-full overflow-hidden">
      {/* Persistent Conversation Sidebar */}
      <ConversationDrawer
        open={drawerOpen}
        onClose={() => setDrawerOpen(false)}
        conversations={conversations}
        activeId={activeId}
        loading={convsLoading}
        onSelect={handleSelectConversation}
        onNew={handleNewChat}
        onDelete={handleDeleteConversation}
        onRename={handleRenameConversation}
      />

      {/* Main Chat Area */}
      <div className="relative flex flex-1 flex-col h-full overflow-hidden min-w-0">
        {/* Top bar */}
        <div className="flex shrink-0 items-center justify-between gap-2 border-b border-border px-3 py-2.5 sm:px-4">
          <div className="flex min-w-0 items-center gap-2">
            <button
              onClick={() => setDrawerOpen(!drawerOpen)}
              className="interactive-hover shrink-0 rounded-[var(--radius-md)] p-1.5 text-text-muted hover:bg-surface-raised hover:text-text-primary"
              title={drawerOpen ? "Ocultar panel" : "Ver historial de chats"}
            >
              <PanelLeft size={16} />
            </button>
            <h1 className="truncate text-sm font-medium text-text-primary">
              {activeId
                ? conversations.find((c) => c.id === activeId)?.title || "Chat"
                : "New Chat"}
            </h1>
          </div>
          <div className="flex shrink-0 items-center gap-1.5">
            <button
              onClick={() => setShowMeshModal(true)}
              className="interactive-hover flex items-center gap-1 rounded-[var(--radius-md)] border border-paw-500/30 bg-paw-500/10 px-2.5 py-1 text-xs font-medium text-paw-400 hover:bg-paw-500/20"
              title="Conectar a PC por Mesh"
            >
              <Globe size={13} />
              <span>PC Mesh</span>
            </button>
            <button
              onClick={handleNewChat}
              className="interactive-hover rounded-[var(--radius-md)] p-1.5 text-text-muted hover:bg-surface-raised hover:text-text-primary"
              title="New chat"
            >
              <Plus size={16} />
            </button>
          </div>
        </div>

      {/* Messages or empty state */}
      <div className="flex-1 overflow-y-auto px-3 py-4 sm:px-6 sm:py-6">
        {loaded.length === 0 ? (
          /* No model loaded */
          <div className="flex h-full items-center justify-center">
            <EmptyState
              icon={<Sparkles size={24} />}
              title="No model loaded"
              description={
                installed.length > 0
                  ? "Load one of your installed models to start chatting."
                  : "No models are installed yet. Download a GGUF model from the Model Browser to start chatting."
              }
              action={{
                label: installed.length > 0 ? "Install another model" : "Open Model Browser",
                onClick: () => navigate("/models"),
              }}
            >
              {installed.length > 0 ? (
                <div className="flex flex-wrap justify-center gap-2">
                  {installed.slice(0, 4).map((m) => (
                    <button
                      key={m.id}
                      disabled={modelLoading}
                      onClick={() => handleLoadModel(m.id)}
                      className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] border border-border px-3 py-2 text-xs text-text-secondary hover:border-paw-500/30 hover:text-paw-400 disabled:opacity-50"
                    >
                      {modelLoading ? <Loader size={12} className="animate-spin" /> : null}
                      {m.name}
                    </button>
                  ))}
                </div>
              ) : null}
              {modelLoading && (
                <p className="text-xs text-text-muted">Loading model… this can take a minute.</p>
              )}
              {loadError && <p className="text-xs text-danger">{loadError}</p>}
              <div className="mt-4 flex w-full max-w-sm flex-col items-center gap-2">
                {discoveredPeers.length > 0 && (
                  <div className="w-full space-y-1.5 rounded-[var(--radius-md)] border border-paw-500/30 bg-paw-500/5 p-2.5 text-left">
                    <span className="block text-[11px] font-semibold text-paw-400">
                      Nodos PC detectados en tu red ({discoveredPeers.length}):
                    </span>
                    {discoveredPeers.slice(0, 3).map((peer) => (
                      <div
                        key={peer.node_id}
                        className="flex items-center justify-between gap-2 rounded bg-surface-raised px-2.5 py-1.5 text-xs"
                      >
                        <div className="min-w-0 flex-1">
                          <span className="block truncate font-medium text-text-primary">
                            {peer.serving_model_id || "Modelo en PC (Auto-detectar)"}
                          </span>
                          <span className="block truncate font-mono text-[10px] text-text-muted">
                            {peer.addr} • {peer.source}
                          </span>
                        </div>
                        <button
                          disabled={meshConnecting}
                          onClick={() => handleConnectPeerDirect(peer.addr, peer.serving_model_id)}
                          className="shrink-0 rounded bg-paw-500 px-2.5 py-1 text-[11px] font-medium text-black hover:bg-paw-400 disabled:opacity-50"
                        >
                          {meshConnecting ? "..." : "Conectar"}
                        </button>
                      </div>
                    ))}
                  </div>
                )}
                <span className="text-xs text-text-muted">¿Tu modelo está corriendo en la PC?</span>
                <button
                  onClick={() => setShowMeshModal(true)}
                  className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] border border-paw-500/40 bg-paw-500/10 px-3 py-1.5 text-xs font-medium text-paw-400 hover:bg-paw-500/20"
                >
                  <Globe size={13} /> Conectar a PC vía Mesh P2P
                </button>
              </div>
            </EmptyState>
          </div>
        ) : messages.length === 0 ? (
          /* Empty conversation */
          <div className="flex h-full flex-col items-center justify-center">
            <div className="flex w-full max-w-md flex-col items-center gap-3 animate-[fade-in]">
              <div className="flex h-14 w-14 sm:h-16 sm:w-16 items-center justify-center rounded-[var(--radius-xl)] bg-paw-500/10">
                <Sparkles size={26} className="text-paw-500" />
              </div>
              <div className="text-center">
                <p className="text-sm font-medium text-text-primary">
                  {modelName ? `Chat with ${modelName}` : "What can I help you with?"}
                </p>
                <p className="mt-1 text-xs text-text-muted">
                  Try a suggestion or type your own message.
                </p>
              </div>

              <div className="mt-3 flex w-full flex-col gap-2">
                {[
                  { title: "Explain quantum computing", desc: "Break it down in simple terms" },
                  { title: "Write a Python sort function", desc: "With type hints and docstring" },
                  { title: "Compare Rust vs Go", desc: "Pros, cons, and use cases" },
                ].map((s) => (
                  <button
                    key={s.title}
                    onClick={() => handleSend(s.title + " — " + s.desc)}
                    className="interactive-hover press-scale rounded-[var(--radius-lg)] border border-border bg-surface-raised px-4 py-3 text-left hover:border-paw-500/30 hover:shadow-[var(--shadow-raised)]"
                  >
                    <span className="text-xs font-medium text-text-primary">{s.title}</span>
                    <span className="mt-0.5 block text-[11px] text-text-muted">{s.desc}</span>
                  </button>
                ))}
              </div>
            </div>
          </div>
        ) : (
          /* Messages */
          <div className="mx-auto max-w-2xl space-y-5 sm:space-y-6">
            {messages.map((msg, i) => {
              const isLastMessage = i === messages.length - 1;
              const isStreaming = streaming && isLastMessage && msg.role === "assistant";

              if (editingIndex === i && msg.role === "user") {
                return (
                  <div key={i} className="flex items-start gap-3">
                    <div className="flex h-6 w-6 shrink-0 items-center justify-center rounded-full bg-paw-500/15 text-paw-400 mt-0.5">
                      <span className="text-[10px] font-bold">You</span>
                    </div>
                    <div className="min-w-0 flex-1">
                      <textarea
                        value={editText}
                        onChange={(e) => setEditText(e.target.value)}
                        className="w-full resize-none rounded-[var(--radius-md)] border border-paw-500 bg-surface-raised px-3 py-2 text-sm outline-none focus:ring-2 focus:ring-paw-500/20"
                        rows={3}
                        autoFocus
                      />
                      <div className="mt-2 flex gap-2">
                        <button
                          onClick={handleSubmitEdit}
                          className="interactive-hover press-scale rounded-[var(--radius-md)] bg-paw-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-paw-600"
                        >
                          Save & Submit
                        </button>
                        <button
                          onClick={() => { setEditingIndex(null); setEditText(""); }}
                          className="interactive-hover rounded-[var(--radius-md)] px-3 py-1.5 text-xs text-text-muted hover:text-text-primary"
                        >
                          Cancel
                        </button>
                      </div>
                    </div>
                  </div>
                );
              }

              return (
                <ChatMessage
                  key={i}
                  message={msg}
                  streaming={isStreaming}
                  modelName={modelName || undefined}
                  onEdit={msg.role === "user" && !streaming ? () => handleStartEdit(i) : undefined}
                  onRegenerate={
                    msg.role === "assistant" && isLastMessage && !streaming
                      ? handleRegenerate
                      : undefined
                  }
                />
              );
            })}
            <div ref={bottomRef} />
          </div>
        )}
      </div>

      {/* Input */}
      {loaded.length > 0 && (
        <ChatInput
          value={input}
          onChange={setInput}
          onSend={() => handleSend()}
          onStop={stop}
          streaming={streaming}
          disabled={activeHandleId === null}
          modelName={modelName}
          loadedModels={loadedModelOptions}
          activeHandleId={activeHandleId}
          onModelChange={setActiveHandleId}
          chatParams={chatParams}
          onChatParamsChange={setChatParams}
        />
      )}
      </div>

      {showMeshModal && (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-3 sm:p-4 backdrop-blur-sm">
          <div className="max-h-[90dvh] w-full max-w-md overflow-y-auto rounded-[var(--radius-lg)] border border-border bg-surface-raised p-4 sm:p-5 shadow-xl">
            <div className="flex items-center justify-between gap-2">
              <h3 className="text-sm sm:text-base font-semibold text-text-primary flex items-center gap-2">
                <Globe size={18} className="text-paw-400 shrink-0" /> Conectar a PC por Mesh P2P
              </h3>
              <button
                onClick={scanMeshPeers}
                disabled={discoveringPeers}
                className="flex items-center gap-1 rounded border border-border bg-surface-overlay px-2 py-1 text-[11px] text-text-secondary hover:text-text-primary"
                title="Buscar PCs en la red"
              >
                <RefreshCw size={11} className={discoveringPeers ? "animate-spin" : ""} />
                <span>{discoveringPeers ? "Buscando…" : "Buscar PC"}</span>
              </button>
            </div>
            <p className="mt-1 text-xs text-text-muted">
              Usa el modelo ejecutándose en tu PC sin descargar nada ni consumir RAM en el móvil.
            </p>

            {discoveredPeers.length > 0 && (
              <div className="mt-3 space-y-1.5 rounded-[var(--radius-md)] border border-paw-500/30 bg-paw-500/5 p-2.5">
                <span className="block text-[11px] font-semibold text-paw-400">
                  Nodos PC encontrados ({discoveredPeers.length}):
                </span>
                {discoveredPeers.map((peer) => (
                  <div
                    key={peer.node_id}
                    className="flex items-center justify-between gap-2 rounded bg-surface-overlay px-2.5 py-2 text-xs"
                  >
                    <div className="min-w-0 flex-1">
                      <span className="block truncate font-medium text-text-primary">
                        {peer.serving_model_id || "Modelo en PC (Auto-detectar)"}
                      </span>
                      <span className="block truncate font-mono text-[10px] text-text-muted">
                        {peer.addr} ({peer.source})
                      </span>
                    </div>
                    <button
                      disabled={meshConnecting}
                      onClick={() => handleConnectPeerDirect(peer.addr, peer.serving_model_id)}
                      className="shrink-0 rounded bg-paw-500 px-3 py-1 text-xs font-medium text-black hover:bg-paw-400 disabled:opacity-50"
                    >
                      Conectar
                    </button>
                  </div>
                ))}
              </div>
            )}

            <div className="mt-4 space-y-3">
              <div>
                <label className="text-[11px] font-medium text-text-secondary">Dirección IP y Puerto del Nodo PC</label>
                <input
                  type="text"
                  value={meshPeerInput}
                  onChange={(e) => setMeshPeerInput(e.target.value)}
                  placeholder="ej. 192.168.0.4:7878"
                  className="mt-1 w-full rounded-[var(--radius-md)] border border-border bg-surface-overlay px-3 py-2 text-xs text-text-primary focus:border-paw-500 focus:outline-none"
                />
              </div>

              <div>
                <label className="text-[11px] font-medium text-text-secondary">
                  Modelo en PC (opcional — se detecta automáticamente si lo dejas vacío)
                </label>
                <input
                  type="text"
                  value={meshModelName}
                  onChange={(e) => setMeshModelName(e.target.value)}
                  placeholder="Auto-detectar modelo de la PC"
                  className="mt-1 w-full rounded-[var(--radius-md)] border border-border bg-surface-overlay px-3 py-2 text-xs text-text-primary focus:border-paw-500 focus:outline-none"
                />
              </div>

              {meshError && (
                <p className="text-xs text-danger">{meshError}</p>
              )}
            </div>

            <div className="mt-5 flex justify-end gap-2">
              <button
                disabled={meshConnecting}
                onClick={() => setShowMeshModal(false)}
                className="rounded-[var(--radius-md)] px-3 py-1.5 text-xs text-text-muted hover:bg-surface-overlay"
              >
                Cancelar
              </button>
              <button
                disabled={meshConnecting || !meshPeerInput.trim()}
                onClick={handleConnectMesh}
                className="flex items-center gap-1.5 rounded-[var(--radius-md)] bg-paw-500 px-4 py-1.5 text-xs font-medium text-black hover:bg-paw-400 disabled:opacity-50"
              >
                {meshConnecting ? <Loader size={12} className="animate-spin" /> : <Globe size={12} />}
                {meshConnecting ? "Conectando…" : "Conectar y Chatear"}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
