import { useEffect, useState } from "react";
import { getMeshConnectionStatus, type MeshConnectionStatus } from "../lib/invoke";

/**
 * `MeshConnectionStatus` from invoke.ts plus the diagnostics the backend returns
 * to explain a degraded mesh (kept local until invoke.ts declares them).
 */
type MeshConnectionDiagnostics = MeshConnectionStatus & {
  last_error?: string | null;
  registered?: boolean;
  known_peers?: number;
};

export interface MeshPulse {
  status: MeshConnectionStatus | null;
  loading: boolean;
  error: boolean;
  /** Backend explanation for a degraded mesh, if any (unreachable coordinator, ...). */
  lastError: string | null;
}

/**
 * Polls the mesh connection status on an interval.
 * Errors are swallowed — the coordinator may be transiently unreachable,
 * and UI callers display idle/dim state when status is null.
 */
export function useMeshPulse(intervalMs = 15_000): MeshPulse {
  const [status, setStatus] = useState<MeshConnectionStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    const tick = async () => {
      try {
        const s = await getMeshConnectionStatus();
        if (cancelled) return;
        setStatus(s);
        setLastError((s as MeshConnectionDiagnostics).last_error ?? null);
        setError(false);
      } catch {
        if (cancelled) return;
        setError(true);
      } finally {
        if (!cancelled) setLoading(false);
      }
    };

    tick();
    const id = setInterval(tick, intervalMs);
    return () => {
      cancelled = true;
      clearInterval(id);
    };
  }, [intervalMs]);

  return { status, loading, error, lastError };
}
