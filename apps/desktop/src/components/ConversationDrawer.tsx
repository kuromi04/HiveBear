import type { Conversation } from "../types";
import { Plus, Search, X, MessageSquare, Pencil, Trash2 } from "lucide-react";
import { useCallback, useState } from "react";

interface ConversationDrawerProps {
  open: boolean;
  onClose: () => void;
  conversations: Conversation[];
  activeId: string | null;
  loading: boolean;
  onSelect: (id: string) => void;
  onNew: () => void;
  onDelete: (id: string) => void;
  onRename: (id: string, title: string) => void;
}

export default function ConversationDrawer({
  open,
  onClose,
  conversations,
  activeId,
  loading,
  onSelect,
  onNew,
  onDelete,
  onRename,
}: ConversationDrawerProps) {
  const [searchQuery, setSearchQuery] = useState("");
  const [renamingId, setRenamingId] = useState<string | null>(null);
  const [renameText, setRenameText] = useState("");

  const filtered = searchQuery
    ? conversations.filter((c) => c.title.toLowerCase().includes(searchQuery.toLowerCase()))
    : conversations;

  const handleStartRename = useCallback((id: string, currentTitle: string, e: React.MouseEvent) => {
    e.stopPropagation();
    setRenamingId(id);
    setRenameText(currentTitle);
  }, []);

  const handleSubmitRename = useCallback(
    (id: string) => {
      if (renameText.trim()) {
        onRename(id, renameText.trim());
      }
      setRenamingId(null);
      setRenameText("");
    },
    [onRename, renameText],
  );

  const handleDelete = useCallback(
    (id: string, e: React.MouseEvent) => {
      e.stopPropagation();
      onDelete(id);
    },
    [onDelete],
  );

  if (!open) return null;

  return (
    <>
      <div
        className="fixed inset-0 z-30 bg-black/50 md:hidden"
        onClick={onClose}
      />
      <aside className="fixed inset-y-0 left-0 z-40 flex h-full w-64 max-w-[82vw] shrink-0 flex-col border-r border-border bg-surface select-none animate-[fade-in] md:relative md:z-10">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-3 py-2.5">
          <span className="text-[11px] font-bold uppercase tracking-wider text-text-muted">
            Historial de Chats
          </span>
          <div className="flex items-center gap-1">
            <button
              onClick={() => {
                onNew();
                if (typeof window !== "undefined" && window.innerWidth < 768) {
                  onClose();
                }
              }}
              className="interactive-hover rounded-[var(--radius-md)] p-1 text-text-muted hover:bg-surface-raised hover:text-text-primary"
              title="Nuevo chat"
            >
              <Plus size={14} />
            </button>
            <button
              onClick={onClose}
              className="interactive-hover rounded-[var(--radius-md)] p-1 text-text-muted hover:bg-surface-raised hover:text-text-primary"
              title="Ocultar historial"
            >
              <X size={14} />
            </button>
          </div>
        </div>

        {/* Search */}
        <div className="border-b border-border px-3 py-2">
          <div className="relative">
            <Search size={12} className="absolute left-2.5 top-1/2 -translate-y-1/2 text-text-muted" />
            <input
              type="text"
              value={searchQuery}
              onChange={(e) => setSearchQuery(e.target.value)}
              placeholder="Buscar..."
              className="w-full rounded-[var(--radius-md)] border border-border bg-surface-raised py-1.5 pl-8 pr-2.5 text-xs outline-none placeholder:text-text-muted focus:border-paw-500 focus:ring-2 focus:ring-paw-500/20"
            />
          </div>
        </div>

        {/* List */}
        <div className="flex-1 overflow-y-auto py-1 pb-16 md:pb-1">
          {loading ? (
            <div className="px-4 py-8 text-center text-xs text-text-muted">Cargando...</div>
          ) : filtered.length === 0 ? (
            <div className="px-4 py-8 text-center text-xs text-text-muted">
              {searchQuery ? "Sin coincidencias." : "Aún no hay conversaciones."}
            </div>
          ) : (
            filtered.map((conv) => (
              <div
                key={conv.id}
                onClick={() => {
                  onSelect(conv.id);
                  if (typeof window !== "undefined" && window.innerWidth < 768) {
                    onClose();
                  }
                }}
                className={[
                  "group mx-1.5 mb-0.5 flex cursor-pointer items-center gap-2 rounded-[var(--radius-md)] px-3 py-2 text-xs interactive-hover",
                  activeId === conv.id
                    ? "bg-paw-500/10 text-paw-400"
                    : "text-text-secondary hover:bg-surface-raised hover:text-text-primary",
                ].join(" ")}
              >
                <MessageSquare size={14} className="shrink-0" />
                <div className="min-w-0 flex-1">
                  {renamingId === conv.id ? (
                    <input
                      type="text"
                      value={renameText}
                      onChange={(e) => setRenameText(e.target.value)}
                      onBlur={() => handleSubmitRename(conv.id)}
                      onKeyDown={(e) => {
                        if (e.key === "Enter") handleSubmitRename(conv.id);
                        if (e.key === "Escape") {
                          setRenamingId(null);
                          setRenameText("");
                        }
                      }}
                      className="w-full rounded border border-paw-500 bg-surface-raised px-1 py-0.5 text-xs outline-none"
                      autoFocus
                      onClick={(e) => e.stopPropagation()}
                    />
                  ) : (
                    <span className="block truncate">{conv.title}</span>
                  )}
                  <span className="block text-[10px] text-text-muted">
                    {conv.message_count} mensaje{conv.message_count !== 1 ? "s" : ""}
                  </span>
                </div>
                {renamingId !== conv.id && (
                  <div className="flex shrink-0 items-center gap-0.5 opacity-100 md:opacity-0 md:group-hover:opacity-100">
                    <button
                      onClick={(e) => handleStartRename(conv.id, conv.title, e)}
                      className="interactive-hover rounded p-1 hover:bg-surface-overlay"
                      title="Renombrar"
                    >
                      <Pencil size={11} />
                    </button>
                    <button
                      onClick={(e) => handleDelete(conv.id, e)}
                      className="interactive-hover rounded p-1 hover:bg-surface-overlay hover:text-danger"
                      title="Eliminar"
                    >
                      <Trash2 size={11} />
                    </button>
                  </div>
                )}
              </div>
            ))
          )}
        </div>
      </aside>
    </>
  );
}
