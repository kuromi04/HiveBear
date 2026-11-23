import type { SearchResult } from "../types";
import { formatBytes } from "../types";
import { Download, Trash2, Check, Loader, MessageSquare, Network } from "lucide-react";
import { Badge } from "./ui";
import { estimateModelGb, type TierInfo } from "../lib/modelTiers";

interface ModelCardProps {
  result: SearchResult;
  installing: string | null;
  removing: string | null;
  loadingId?: string | null;
  onInstall: (modelId: string) => void;
  onRemove: (modelId: string) => void;
  onUse?: (modelId: string) => void;
  /** Hardware-aware tier for this model on the current device. */
  tier?: TierInfo;
  /** Open the Mesh page (used for models too big to run locally on mobile). */
  onMesh?: () => void;
}

export default function ModelCard({
  result,
  installing,
  removing,
  loadingId,
  onInstall,
  onRemove,
  onUse,
  tier,
  onMesh,
}: ModelCardProps) {
  const { metadata: m, is_installed, compatibility_score } = result;
  const isThisInstalling = installing === m.id;
  const isThisRemoving = removing === m.id;
  const isThisLoading = loadingId === m.id;
  const busy = isThisInstalling || isThisRemoving || isThisLoading;
  const tooBig = tier?.id === "too_big";
  const sizeGb = estimateModelGb(m.params_billions);

  return (
    <div className={[
      "rounded-[var(--radius-lg)] border bg-surface-raised p-4 interactive-hover",
      is_installed
        ? "border-l-[3px] border-l-paw-500 border-t-border border-r-border border-b-border"
        : isThisInstalling
          ? "border-paw-500/40 shadow-[0_0_12px_-4px] shadow-paw-500/20"
          : "border-border",
    ].join(" ")}>
      <div className="flex flex-col gap-3 sm:flex-row sm:items-start sm:justify-between">
        <div className="min-w-0 flex-1">
          <h3 className="truncate text-sm font-semibold sm:font-medium">{m.name}</h3>
          <p className="mt-0.5 flex items-center gap-1.5 text-xs text-text-muted">
            <span>{m.params_billions}B params</span>
            <span>·</span>
            <Badge variant="default">{m.category}</Badge>
            <span>·</span>
            <span>ctx {m.context_length.toLocaleString()}</span>
          </p>
          {m.description && (
            <p className="mt-1.5 line-clamp-2 text-xs text-text-secondary">{m.description}</p>
          )}
        </div>
        <div className="flex shrink-0 items-center gap-1.5 [&>button]:min-h-10 sm:[&>button]:min-h-0 [&>button:first-child]:flex-1 sm:[&>button:first-child]:flex-none [&>button]:justify-center">
          {is_installed ? (
            <>
              {onUse && (
                <button
                  disabled={busy}
                  onClick={() => onUse(m.id)}
                  className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] bg-paw-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-paw-600 disabled:opacity-50"
                >
                  {isThisLoading ? <Loader size={12} className="animate-spin" /> : <MessageSquare size={12} />}
                  Chatear
                </button>
              )}
              <button disabled={busy} onClick={() => onRemove(m.id)}
                className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] border border-border px-2.5 py-1.5 text-xs text-text-secondary hover:border-danger hover:text-danger disabled:opacity-50"
                title="Eliminar modelo">
                {isThisRemoving ? <Loader size={12} className="animate-spin" /> : <Trash2 size={12} />}
              </button>
            </>
          ) : tooBig && onMesh ? (
            <button onClick={onMesh}
              className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] border border-paw-500/40 px-3 py-1.5 text-xs font-medium text-paw-400 hover:bg-paw-500/10">
              <Network size={12} />
              Usar desde PC
            </button>
          ) : (
            <button disabled={busy} onClick={() => onInstall(m.id)}
              className="interactive-hover press-scale flex items-center gap-1.5 rounded-[var(--radius-md)] bg-paw-500 px-3 py-1.5 text-xs font-medium text-white hover:bg-paw-600 disabled:opacity-50">
              {isThisInstalling ? <Loader size={12} className="animate-spin" /> : <Download size={12} />}
              Install
            </button>
          )}
        </div>
      </div>
      <div className="mt-3 flex flex-wrap items-center gap-x-3 gap-y-1.5 text-xs text-text-muted">
        {tier && (
          <Badge variant={tier.badge} title={tier.hint}>{tier.label} · ~{sizeGb.toFixed(1)} GB</Badge>
        )}
        {!tier && compatibility_score != null && (
          <Badge variant={compatibility_score >= 0.7 ? "success" : compatibility_score >= 0.4 ? "warning" : "danger"}>
            {(compatibility_score * 100).toFixed(0)}% compatible
          </Badge>
        )}
        {m.installed && (
          <Badge variant="success">
            <Check size={10} />{formatBytes(m.installed.size_bytes)}
          </Badge>
        )}
        {m.downloads_count != null && <span>{m.downloads_count.toLocaleString()} downloads</span>}
        {m.tags.length > 0 && <span className="truncate">{m.tags.slice(0, 3).join(" · ")}</span>}
      </div>
    </div>
  );
}
