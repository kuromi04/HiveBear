import { useCallback, useEffect, useState } from "react";
import { getMeshStatus, saveMeshConfig, getMeshConfig, getMeshConnectionStatus, joinMesh, leaveMesh, connectMeshPeer, discoverMeshPeers } from "../lib/invoke";
import { useNavigate } from "react-router-dom";
import type { MeshConnectionStatus } from "../lib/invoke";
import type { MeshStatus as MeshStatusType } from "../types";
import { Link } from "react-router-dom";
import { Network, Settings, Power, Shield, Activity, Wifi, WifiOff, AlertTriangle } from "lucide-react";
import { Card, Button, Badge, Surface } from "../components/ui";

/**
 * `MeshConnectionStatus` from invoke.ts plus the fields the backend now returns
 * to explain a degraded mesh (kept local until invoke.ts declares them).
 */
type MeshConnection = MeshConnectionStatus & {
  known_peers?: number;
  registered?: boolean;
  last_error?: string | null;
  coordinator_url?: string;
};

export default function MeshStatus() {
  const [status, setStatus] = useState<MeshStatusType | null>(null);
  const [connection, setConnection] = useState<MeshConnection | null>(null);
  const [loading, setLoading] = useState(true);
  const [toggling, setToggling] = useState(false);
  const [joining, setJoining] = useState(false);

  const refresh = useCallback(async () => {
    try {
      const [s, c] = await Promise.all([getMeshStatus(), getMeshConnectionStatus()]);
      setStatus(s);
      setConnection(c);
    } catch { /* ignore */ }
    finally { setLoading(false); }
  }, []);

  useEffect(() => { refresh(); }, [refresh]);

  // Poll connection status often enough to show a registration failure quickly.
  useEffect(() => {
    const interval = setInterval(async () => {
      try { setConnection(await getMeshConnectionStatus()); } catch { /* ignore */ }
    }, 10_000);
    return () => clearInterval(interval);
  }, []);

  const toggleEnabled = useCallback(async () => {
    if (!status) return;
    setToggling(true);
    try {
      const config = await getMeshConfig();
      config.enabled = !config.enabled;
      await saveMeshConfig(config);
      await refresh();
    } catch { /* ignore */ }
    finally { setToggling(false); }
  }, [status, refresh]);

  if (loading) {
    return <Surface><div className="flex h-full items-center justify-center text-text-muted text-sm">Loading mesh status...</div></Surface>;
  }

  if (!status) {
    return <Surface><div className="text-sm text-danger">Failed to load mesh status.</div></Surface>;
  }

  return (
    <Surface>
      <div className="space-y-6">
        <div className="flex items-center justify-between">
          <h1 className="text-xl font-semibold">P2P Mesh</h1>
          <Link to="/settings"
            className="interactive-hover flex items-center gap-1.5 rounded-[var(--radius-md)] border border-border px-3 py-1.5 text-xs text-text-secondary hover:text-text-primary">
            <Settings size={12} /> Configure
          </Link>
        </div>

        {/* Status header */}
        <Card padding="lg">
          <div className="flex flex-wrap items-center gap-3 sm:gap-4">
            <div className={[
              "flex h-12 w-12 shrink-0 items-center justify-center rounded-[var(--radius-lg)]",
              status.enabled ? "bg-success/10 text-success" : "bg-surface-overlay text-text-muted",
            ].join(" ")}>
              <Network size={24} />
            </div>
            <div className="min-w-[180px] flex-1">
              <div className="flex items-center gap-2">
                <span className="text-base font-medium">
                  Mesh {status.enabled ? "Enabled" : "Disabled"}
                </span>
                <Badge variant={status.tier === "paid" ? "accent" : "default"}>
                  {status.tier === "paid" ? "Paid" : "Free"}
                </Badge>
              </div>
              <p className="mt-0.5 text-sm text-text-muted">
                {status.enabled
                  ? `Listening on port ${status.port}`
                  : "Enable to join the mesh network and share compute."}
              </p>
            </div>
            <Button
              variant={status.enabled ? "secondary" : "primary"}
              onClick={toggleEnabled}
              disabled={toggling}
            >
              <Power size={14} />
              {status.enabled ? "Disable" : "Enable"}
            </Button>
          </div>
        </Card>

        {/* Config cards */}
        <div className="grid grid-cols-1 gap-4 md:grid-cols-3">
          <StatCard icon={<Network size={14} />} label="Coordination Server" value={status.coordination_server.replace(/^https?:\/\//, "")} />
          <StatCard icon={<Shield size={14} />} label="Min Trust Score" value={`${(status.min_reputation * 100).toFixed(0)}%`} />
          <StatCard icon={<Activity size={14} />} label="Verification Rate" value={`${(status.verification_rate * 100).toFixed(0)}%`} />
        </div>

        {/* Contribution */}
        <Card>
          <h2 className="mb-3 text-sm font-semibold">Resource Contribution</h2>
          <div className="flex flex-wrap items-center gap-4">
            <div className="min-w-[180px] flex-1">
              <div className="mb-1 flex justify-between text-xs">
                <span className="text-text-muted">Share limit</span>
                <span className="font-mono text-text-secondary">
                  {(status.max_contribution_percent * 100).toFixed(0)}%
                </span>
              </div>
              <div className="h-2 overflow-hidden rounded-full bg-surface-overlay">
                <div className="h-full rounded-full bg-paw-500 interactive-hover" style={{ width: `${status.max_contribution_percent * 100}%` }} />
              </div>
            </div>
            <p className="text-xs text-text-muted sm:max-w-48">
              Maximum percentage of local resources shared with the mesh network.
            </p>
          </div>
        </Card>

        {/* Connection status */}
        {status.enabled && connection && (
          <Card padding="lg">
            <div className="flex flex-wrap items-center gap-3 sm:gap-4">
              <div className={[
                "flex h-12 w-12 shrink-0 items-center justify-center rounded-[var(--radius-lg)]",
                connection.running ? "bg-success/10 text-success" : "bg-surface-overlay text-text-muted",
              ].join(" ")}>
                {connection.running ? <Wifi size={24} /> : <WifiOff size={24} />}
              </div>
              <div className="min-w-[180px] flex-1">
                <div className="flex items-center gap-2">
                  <span className="text-base font-medium">
                    {!connection.running
                      ? "Not Started"
                      : connection.registered
                        ? "Connected to Hive"
                        : "LAN-only (coordinator unreachable)"}
                  </span>
                  {connection.running && (
                    <Badge variant={connection.registered ? "default" : "accent"}>
                      {connection.peer_count} peer{connection.peer_count !== 1 ? "s" : ""}
                    </Badge>
                  )}
                </div>
                <p className="mt-0.5 text-sm text-text-muted">
                  {!connection.running
                    ? "Click Join to start the mesh node."
                    : connection.registered
                      ? (connection.peer_count > 0
                          ? `Node ${connection.node_id?.slice(0, 12)}… registered on ${connection.coordinator_url ?? status.coordination_server} and connected.`
                          : `Node ${connection.node_id?.slice(0, 12)}… is registered on ${connection.coordinator_url ?? status.coordination_server}. No other node is connected right now — 0 peers is expected while you are the only node online.`)
                      : `Node ${connection.node_id?.slice(0, 12)}… is running without a coordination-server registration; only LAN peers can be found.`}
                </p>
              </div>
              <Button
                variant={connection.running ? "secondary" : "primary"}
                disabled={joining}
                onClick={async () => {
                  setJoining(true);
                  try {
                    if (connection.running) {
                      await leaveMesh();
                    } else {
                      if (!status.enabled) {
                        const cfg = await getMeshConfig();
                        cfg.enabled = true;
                        await saveMeshConfig(cfg);
                      }
                      await joinMesh();
                      // The node starts in the background; give it a moment so
                      // the status below reflects reality instead of "Not Started".
                      await new Promise((resolve) => setTimeout(resolve, 1500));
                    }
                    await refresh();
                  } catch (err: any) {
                    console.error("Failed to join/leave mesh:", err);
                  } finally {
                    setJoining(false);
                  }
                }}
              >
                {connection.running ? <WifiOff size={14} /> : <Wifi size={14} />}
                {joining ? "…" : connection.running ? "Leave" : "Join"}
              </Button>
            </div>

            {/* Real, actionable error instead of a silent zero-peer state.
                A secondary-source warning (e.g. the LAN port is taken) is muted
                while the node is registered and connected. */}
            {connection.running && connection.last_error && (
              <div className={[
                "mt-4 flex items-start gap-2 rounded-[var(--radius-md)] border p-3",
                !connection.registered || connection.peer_count === 0
                  ? "border-danger/30 bg-danger/5"
                  : "border-border bg-surface-overlay",
              ].join(" ")}>
                <AlertTriangle size={14} className={[
                  "mt-0.5 shrink-0",
                  !connection.registered || connection.peer_count === 0 ? "text-danger" : "text-text-muted",
                ].join(" ")} />
                <p className="text-xs text-text-secondary">
                  <span className={[
                    "font-medium",
                    !connection.registered || connection.peer_count === 0 ? "text-danger" : "text-text-muted",
                  ].join(" ")}>
                    {connection.registered && connection.peer_count > 0 ? "Mesh warning: " : "Mesh degraded: "}
                  </span>
                  {connection.last_error}
                </p>
              </div>
            )}
          </Card>
        )}

        {/* Direct P2P Local WiFi / LAN Connection */}
        <DirectPeerConnectCard />

        {/* Live Network Karma & Peers Leaderboard */}
        <NetworkKarmaLeaderboard coordinatorUrl={connection?.coordinator_url ?? status.coordination_server} />
      </div>
    </Surface>
  );
}

function DirectPeerConnectCard() {
  const navigate = useNavigate();
  const [peerInput, setPeerInput] = useState(() => {
    return localStorage.getItem("last_mesh_peer") || "192.168.0.4:7878";
  });
  const [connecting, setConnecting] = useState(false);
  const [statusMsg, setStatusMsg] = useState<{ isError: boolean; text: string } | null>(null);

  const handleConnect = useCallback(async () => {
    const target = peerInput.trim();
    if (!target) return;
    setConnecting(true);
    setStatusMsg(null);
    try {
      localStorage.setItem("last_mesh_peer", target);
      const res = await connectMeshPeer(target);
      setStatusMsg({
        isError: false,
        text: `¡Conectado exitosamente con ${res.model_path}! Redirigiendo al chat...`,
      });
      setTimeout(() => navigate("/chat"), 600);
    } catch (err: any) {
      setStatusMsg({
        isError: true,
        text: typeof err === "string" ? err : err?.message || "Error al conectar al nodo PC",
      });
    } finally {
      setConnecting(false);
    }
  }, [peerInput, navigate]);

  return (
    <Card padding="lg">
      <div className="flex flex-col gap-3">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border pb-3">
          <div className="flex items-center gap-2">
            <h2 className="text-sm font-semibold flex items-center gap-1.5">
              <span>🔗 Conectar a Nodo PC (WiFi / LAN)</span>
            </h2>
            <Badge variant="accent">P2P Directo</Badge>
          </div>
          <span className="text-[11px] text-text-muted">Sin necesidad de internet</span>
        </div>

        <p className="text-xs text-text-muted">
          Ingresa la IP local de tu PC para chatear usando su procesador y modelo de IA directamente:
        </p>

        <div className="flex flex-col sm:flex-row gap-2 mt-1">
          <input
            type="text"
            value={peerInput}
            onChange={(e) => setPeerInput(e.target.value)}
            placeholder="ej. 192.168.0.4:7878"
            className="flex-1 rounded-[var(--radius-md)] border border-border bg-surface-overlay px-3 py-2 text-xs font-mono text-text-primary focus:border-paw-500 focus:outline-none"
          />
          <Button
            variant="primary"
            disabled={connecting || !peerInput.trim()}
            onClick={handleConnect}
          >
            {connecting ? "Conectando…" : "Conectar y Chatear"}
          </Button>
        </div>

        {statusMsg && (
          <p className={`text-xs ${statusMsg.isError ? "text-danger" : "text-success"}`}>
            {statusMsg.text}
          </p>
        )}
      </div>
    </Card>
  );
}

function StatCard({ icon, label, value }: { icon: React.ReactNode; label: string; value: string }) {
  return (
    <Card>
      <div className="mb-2 flex items-center gap-2 text-text-muted">
        {icon}
        <span className="text-[11px] uppercase tracking-wider">{label}</span>
      </div>
      <p className="truncate text-sm font-medium">{value}</p>
    </Card>
  );
}

interface CoordinatorNode {
  node_id: string;
  addr: string;
  tier: string;
  ram_bytes: number;
  vram_bytes: number;
  serving_model_id?: string | null;
  karma?: number;
}

function NetworkKarmaLeaderboard({ coordinatorUrl }: { coordinatorUrl: string }) {
  const navigate = useNavigate();
  const [nodes, setNodes] = useState<CoordinatorNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const fetchDashboard = useCallback(async () => {
    setLoading(true);
    let merged: CoordinatorNode[] = [];
    let coordErr: string | null = null;

    try {
      const lanPeers = await discoverMeshPeers();
      for (const p of lanPeers) {
        merged.push({
          node_id: p.node_id,
          addr: p.addr,
          tier: p.source,
          ram_bytes: 0,
          vram_bytes: 0,
          serving_model_id: p.serving_model_id,
          karma: 0,
        });
      }
    } catch {
      // ignore LAN discovery error
    }

    try {
      const target = coordinatorUrl.replace(/\/+$/, "");
      const res = await fetch(`${target}/dashboard`);
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const data = await res.json();
      const coordNodes: CoordinatorNode[] = data.nodes || [];
      for (const cn of coordNodes) {
        const existingIdx = merged.findIndex((m) => m.node_id === cn.node_id);
        if (existingIdx >= 0) {
          merged[existingIdx] = {
            ...cn,
            addr: merged[existingIdx].addr || cn.addr,
            serving_model_id: merged[existingIdx].serving_model_id || cn.serving_model_id,
          };
        } else {
          merged.push(cn);
        }
      }
      setError(null);
    } catch (e) {
      coordErr = e instanceof Error ? e.message : "Error connecting to coordinator";
      if (merged.length === 0) {
        setError(coordErr);
      } else {
        setError(null);
      }
    } finally {
      setNodes(merged);
      setLoading(false);
    }
  }, [coordinatorUrl]);

  useEffect(() => {
    fetchDashboard();
    const interval = setInterval(fetchDashboard, 15000);
    return () => clearInterval(interval);
  }, [fetchDashboard]);

  return (
    <Card>
      <div className="flex flex-wrap items-center justify-between gap-2 mb-4 border-b border-border pb-3">
        <div>
          <h2 className="text-sm font-semibold flex flex-wrap items-center gap-2">
            <span>⚡ Network Swarm & Karma Leaderboard</span>
            <span className="text-[10px] bg-paw-500/10 text-paw-500 border border-paw-500/30 px-2 py-0.5 rounded-full font-mono">
              LAN + Cloud Coordinator
            </span>
          </h2>
          <p className="text-xs text-text-muted mt-0.5">
            Active peer nodes sharing compute across WiFi LAN and the decentralized mesh.
          </p>
        </div>
        <Button variant="ghost" size="sm" onClick={fetchDashboard} disabled={loading}>
          {loading ? "Refreshing…" : "Refresh"}
        </Button>
      </div>

      {nodes.length === 0 ? (
        <div className="rounded-[var(--radius-md)] bg-surface-overlay p-5 text-center text-xs text-text-muted space-y-1.5">
          <p className="font-medium text-text-secondary">Modo Red Local WiFi (P2P)</p>
          <p>
            No se detectaron otros nodos automáticamente por broadcast.
            Usa el cuadro de arriba para conectarte directamente a tu PC (ej. <span className="font-mono text-paw-400">192.168.0.4:7878</span>).
          </p>
          {error && (
            <p className="text-[11px] text-text-muted/70 mt-1">
              (Coordinador en la nube no disponible; operando 100% en red local)
            </p>
          )}
        </div>
      ) : (
        <div className="overflow-x-auto">
          <table className="w-full text-left text-xs">
            <thead>
              <tr className="border-b border-border/60 text-text-muted">
                <th className="pb-2 pr-2 font-medium">Node ID</th>
                <th className="pb-2 pr-2 font-medium">Address</th>
                <th className="pb-2 pr-2 font-medium">Model</th>
                <th className="pb-2 pr-2 font-medium">RAM / VRAM</th>
                <th className="pb-2 pr-2 font-medium text-right">Karma</th>
                <th className="pb-2 font-medium text-right">Action</th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border/30 font-mono">
              {nodes.map((node) => {
                const k = node.karma ?? 0;
                return (
                  <tr key={node.node_id} className="hover:bg-surface-overlay/40 transition-colors">
                    <td className="py-2.5 pr-2 font-medium text-text-primary">
                      {node.node_id.slice(0, 12)}…
                    </td>
                    <td className="py-2.5 pr-2 text-text-muted">
                      {node.addr}
                    </td>
                    <td className="py-2.5 pr-2 font-sans text-paw-400">
                      {node.serving_model_id || "Auto-detect"}
                    </td>
                    <td className="py-2.5 pr-2 text-text-secondary">
                      {node.ram_bytes > 0
                        ? `${(node.ram_bytes / (1024 * 1024 * 1024)).toFixed(1)} GB / ${(node.vram_bytes / (1024 * 1024 * 1024)).toFixed(1)} GB`
                        : node.tier}
                    </td>
                    <td className="py-2.5 pr-2 text-right font-bold text-success">
                      +{k} pts
                    </td>
                    <td className="py-2.5 text-right font-sans">
                      <button
                        onClick={async () => {
                          try {
                            await connectMeshPeer(node.addr, node.serving_model_id || undefined);
                            navigate("/chat");
                          } catch (e) {
                            console.error(e);
                          }
                        }}
                        className="rounded-[var(--radius-md)] border border-paw-500/30 bg-paw-500/10 px-2.5 py-1 text-[11px] font-medium text-paw-400 hover:bg-paw-500/20"
                      >
                        Chatear
                      </button>
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      )}
    </Card>
  );
}
