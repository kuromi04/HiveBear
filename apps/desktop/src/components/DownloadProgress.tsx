import type { DownloadProgress as DownloadProgressData } from "../types";
import { formatBytes } from "../types";

interface DownloadProgressProps {
  modelId: string;
  progress: DownloadProgressData | null;
}

export default function DownloadProgress({ modelId, progress }: DownloadProgressProps) {
  if (!progress) {
    return (
      <div className="rounded-lg border border-border bg-surface-raised px-4 py-3 text-sm animate-pulse">
        Starting download for <span className="font-medium">{modelId}</span>...
      </div>
    );
  }

  const rawPct = progress.total_bytes != null && progress.total_bytes > 0
    ? (progress.bytes_downloaded / progress.total_bytes) * 100 : null;
  
  const pct = rawPct != null ? Math.min(100, Math.max(0, rawPct)) : null;

  return (
    <div className="rounded-lg border border-paw-700/30 bg-surface-raised px-4 py-3">
      <div className="mb-2 flex items-baseline justify-between text-xs">
        <span className="font-medium truncate max-w-[200px]">{modelId}</span>
        <span className="font-mono text-text-muted">
          {formatBytes(progress.bytes_downloaded)}
          {progress.total_bytes != null && ` / ${formatBytes(progress.total_bytes)}`}{" "}
          · {formatBytes(progress.bytes_per_sec)}/s
        </span>
      </div>
      <div className="h-1.5 overflow-hidden rounded-full bg-surface-overlay">
        <div className="h-full rounded-full bg-paw-500 transition-[width] duration-150 ease-out"
          style={{ width: pct != null ? `${pct.toFixed(1)}%` : "30%" }} />
      </div>
    </div>
  );
}
