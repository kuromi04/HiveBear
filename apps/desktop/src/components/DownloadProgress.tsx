import type { DownloadProgress as DownloadProgressData } from "../types";
import { formatBytes } from "../types";
import { X, RefreshCw } from "lucide-react";

interface DownloadProgressProps {
  modelId: string;
  progress: DownloadProgressData | null;
  onCancel?: () => void;
  onRetry?: () => void;
  error?: string | null;
}

export default function DownloadProgress({ modelId, progress, onCancel, onRetry, error }: DownloadProgressProps) {
  if (error) {
    return (
      <div className="rounded-lg border border-danger/40 bg-danger/10 px-4 py-3 text-sm flex items-center justify-between gap-3">
        <div className="flex-1 truncate text-danger">
          <span className="font-semibold">Error in {modelId}:</span> {error}
        </div>
        <div className="flex items-center gap-2 shrink-0">
          {onRetry && (
            <button onClick={onRetry} className="interactive-hover px-2.5 py-1 rounded bg-danger/20 text-danger hover:bg-danger/30 text-xs font-medium flex items-center gap-1">
              <RefreshCw size={12} /> Retry
            </button>
          )}
          {onCancel && (
            <button onClick={onCancel} className="interactive-hover p-1 rounded text-text-muted hover:text-text-primary">
              <X size={14} />
            </button>
          )}
        </div>
      </div>
    );
  }

  if (!progress) {
    return (
      <div className="rounded-lg border border-border bg-surface-raised px-4 py-3 text-sm flex items-center justify-between animate-pulse">
        <span>Starting download for <span className="font-medium">{modelId}</span>...</span>
        {onCancel && (
          <button onClick={onCancel} title="Cancel download" className="interactive-hover p-1 rounded text-text-muted hover:text-danger">
            <X size={16} />
          </button>
        )}
      </div>
    );
  }

  const rawPct = progress.total_bytes != null && progress.total_bytes > 0
    ? (progress.bytes_downloaded / progress.total_bytes) * 100 : null;
  
  const pct = rawPct != null ? Math.min(100, Math.max(0, rawPct)) : null;

  return (
    <div className="rounded-lg border border-paw-700/30 bg-surface-raised px-4 py-3">
      <div className="mb-2 flex items-center justify-between text-xs">
        <span className="font-medium truncate max-w-[220px]">{modelId}</span>
        <div className="flex items-center gap-3">
          <span className="font-mono text-text-muted">
            {formatBytes(progress.bytes_downloaded)}
            {progress.total_bytes != null && ` / ${formatBytes(progress.total_bytes)}`}{" "}
            · {formatBytes(progress.bytes_per_sec)}/s
          </span>
          {onCancel && (
            <button onClick={onCancel} title="Cancel download" className="interactive-hover p-1 rounded text-text-muted hover:text-danger">
              <X size={14} />
            </button>
          )}
        </div>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-surface-overlay">
        <div className="h-full rounded-full bg-paw-500 transition-[width] duration-150 ease-out"
          style={{ width: pct != null ? `${pct.toFixed(1)}%` : "30%" }} />
      </div>
    </div>
  );
}
