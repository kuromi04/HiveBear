import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAccount, useUsage, useApiKeys } from "../hooks/useAccount";
import { getKarmaStatus, type KarmaStatus } from "../lib/invoke";
import {
  UserCircle,
  KeyRound,
  Shield,
  Copy,
  Trash2,
  Plus,
  Fingerprint,
  Mail,
  Crown,
  Eye,
  EyeOff,
  Zap,
  Globe,
  Wifi,
  Check,
} from "lucide-react";

// ── Tier badge ──────────────────────────────────────────────────────

function TierBadge({ tier }: { tier: string }) {
  const styles: Record<string, string> = {
    community: "bg-zinc-700/50 text-zinc-300 border-zinc-600",
    contributor: "bg-emerald-900/30 text-emerald-300 border-emerald-700",
    pro: "bg-amber-900/30 text-amber-300 border-amber-700",
    vip_alpha: "bg-amber-900/30 text-amber-300 border-amber-700",
    team: "bg-blue-900/30 text-blue-300 border-blue-700",
    enterprise: "bg-purple-900/30 text-purple-300 border-purple-700",
  };

  const icons: Record<string, typeof Crown> = {
    pro: Shield,
    vip_alpha: Crown,
    team: KeyRound,
    enterprise: Crown,
  };

  const key = tier.toLowerCase();
  const Icon = icons[key];

  return (
    <span
      className={`inline-flex items-center gap-1 rounded-full border px-2.5 py-0.5 text-xs font-medium uppercase ${styles[key] ?? styles.community}`}
    >
      {Icon && <Icon size={12} />}
      {tier}
    </span>
  );
}

// ── Live Karma & Economy Section (always visible on Account page) ───

function KarmaEconomySection({
  karmaStatus,
  onUpgrade,
}: {
  karmaStatus: KarmaStatus | null;
  onUpgrade?: (plan: string) => void;
}) {
  const [copiedId, setCopiedId] = useState(false);
  const karma = karmaStatus?.karma ?? 0;
  const tier = karmaStatus?.tier ?? "COMMUNITY";
  const progressPct = Math.min(100, Math.round((Math.max(0, karma) / 2000) * 100));

  const copyNodeId = () => {
    if (!karmaStatus?.node_id) return;
    navigator.clipboard.writeText(karmaStatus.node_id);
    setCopiedId(true);
    setTimeout(() => setCopiedId(false), 2000);
  };

  return (
    <div className="space-y-4">
      {/* Live Karma Wallet Card */}
      <section className="rounded-xl border border-paw-500/30 bg-gradient-to-br from-paw-500/10 via-surface-raised to-surface-raised p-5">
        <div className="flex flex-wrap items-start justify-between gap-3">
          <div>
            <div className="flex items-center gap-2">
              <span className="text-xs font-semibold uppercase tracking-wider text-paw-400">
                ⚖️ Billetera Karma P2P (Proof-of-Inference)
              </span>
              <TierBadge tier={tier} />
            </div>
            <p className="mt-2 text-3xl font-bold text-text-primary">
              +{karma.toLocaleString()} <span className="text-sm font-normal text-text-secondary">Karma pts</span>
            </p>
            <p className="mt-1 text-xs text-text-secondary">
              {tier === "VIP_ALPHA"
                ? "Prioridad máxima desbloqueada en toda la red P2P global."
                : `Progreso a VIP_ALPHA (2,000 pts): ${progressPct}% · Ganas +1 Karma por cada 10 tokens servidos.`}
            </p>
          </div>

          {karmaStatus?.node_id && (
            <button
              onClick={copyNodeId}
              className="flex items-center gap-1.5 rounded-lg border border-border bg-surface px-3 py-1.5 font-mono text-xs text-text-secondary hover:border-paw-500/40 hover:text-text-primary"
              title="Copiar tu Node ID Ed25519"
            >
              <Fingerprint size={13} className="text-paw-400" />
              <span>{karmaStatus.node_id.slice(0, 12)}…</span>
              {copiedId ? <Check size={12} className="text-success" /> : <Copy size={12} />}
            </button>
          )}
        </div>

        {/* Progress bar to VIP_ALPHA */}
        <div className="mt-3 h-2 w-full overflow-hidden rounded-full bg-surface-overlay">
          <div
            className="h-full rounded-full bg-paw-500 transition-all duration-500"
            style={{ width: `${Math.max(4, progressPct)}%` }}
          />
        </div>

        {/* 3 Economy Rules */}
        <div className="mt-4 grid grid-cols-1 gap-2.5 sm:grid-cols-3">
          <div className="rounded-lg border border-border bg-surface/80 p-3">
            <div className="flex items-center gap-1.5 text-xs font-semibold text-success">
              <Zap size={13} /> Minería P2P (+1 / 10 tok)
            </div>
            <p className="mt-1 text-[11px] text-text-secondary">
              Comparte la GPU/CPU de tu PC con la red y recibe recibos criptográficos Ed25519 por cada respuesta.
            </p>
          </div>
          <div className="rounded-lg border border-border bg-surface/80 p-3">
            <div className="flex items-center gap-1.5 text-xs font-semibold text-paw-400">
              <Wifi size={13} /> Wi-Fi Local LAN (0 Karma)
            </div>
            <p className="mt-1 text-[11px] text-text-secondary">
              Conectar tu APK móvil a tu propia PC por Wi-Fi local es 100% gratis e ilimitado siempre.
            </p>
          </div>
          <div className="rounded-lg border border-border bg-surface/80 p-3">
            <div className="flex items-center gap-1.5 text-xs font-semibold text-amber-300">
              <Globe size={13} /> Enjambre Global (1 / 20 tok)
            </div>
            <p className="mt-1 text-[11px] text-text-secondary">
              Usa GPUs remotas de la comunidad gastando Karma ganado o con pase directo VIP Pro.
            </p>
          </div>
        </div>
      </section>

      {/* Monetization Plans Grid */}
      <section className="rounded-xl border border-border bg-surface-raised p-5">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div>
            <h3 className="text-sm font-semibold text-text-primary">
              💎 Economía Dual y Planes de HiveBear
            </h3>
            <p className="text-xs text-text-secondary">
              Sube a VIP gratis aportando cómputo P2P, o adquiere prioridad instantánea si solo usas móvil/laptop sin GPU.
            </p>
          </div>
        </div>

        <div className="grid grid-cols-1 gap-3 sm:grid-cols-3">
          {/* Plan 1: Community / Contributor Miner */}
          <div className="flex flex-col justify-between rounded-xl border border-border bg-surface p-4">
            <div>
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase text-text-secondary">Community Miner</span>
                <span className="rounded-full bg-success/15 px-2 py-0.5 text-[10px] font-semibold text-success">
                  Activo
                </span>
              </div>
              <p className="mt-2 text-xl font-bold text-text-primary">
                $0 <span className="text-xs font-normal text-text-muted">/ siempre</span>
              </p>
              <ul className="mt-3 space-y-1.5 text-xs text-text-secondary">
                <li>✓ Modelos locales GGUF ilimitados (PC y APK)</li>
                <li>✓ Conexión P2P LAN Móvil ↔ PC 100% gratis</li>
                <li>✓ Sube a VIP_ALPHA minando 2,000 Karma</li>
              </ul>
            </div>
          </div>

          {/* Plan 2: Pro Pass */}
          <div className="flex flex-col justify-between rounded-xl border border-paw-500/50 bg-paw-500/5 p-4">
            <div>
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase text-paw-400">Pro Pass (VIP)</span>
                <Crown size={14} className="text-paw-400" />
              </div>
              <p className="mt-2 text-xl font-bold text-text-primary">
                $9.99 <span className="text-xs font-normal text-text-muted">/ mes</span>
              </p>
              <ul className="mt-3 space-y-1.5 text-xs text-text-secondary">
                <li>✓ Rango <strong>VIP_ALPHA</strong> directo sin minar</li>
                <li>✓ +5,000 Karma/mes para GPUs del enjambre</li>
                <li>✓ Cola de prioridad #1 en modelos 9B–70B</li>
              </ul>
            </div>
            {onUpgrade && (
              <button
                onClick={() => onUpgrade("pro")}
                className="mt-4 w-full rounded-lg bg-paw-500 px-3 py-2 text-xs font-semibold text-black hover:bg-paw-400"
              >
                Obtener Pro Pass
              </button>
            )}
          </div>

          {/* Plan 3: Team / Enterprise Hive */}
          <div className="flex flex-col justify-between rounded-xl border border-border bg-surface p-4">
            <div>
              <div className="flex items-center justify-between">
                <span className="text-xs font-semibold uppercase text-blue-300">Team Hive</span>
                <Shield size={14} className="text-blue-300" />
              </div>
              <p className="mt-2 text-xl font-bold text-text-primary">
                $19 <span className="text-xs font-normal text-text-muted">/ seat / mes</span>
              </p>
              <ul className="mt-3 space-y-1.5 text-xs text-text-secondary">
                <li>✓ Coordinador P2P privado aislado (Zero-Leak)</li>
                <li>✓ Pool de Karma compartido entre el equipo</li>
                <li>✓ API compatible con OpenAI/Ollama + SLA</li>
              </ul>
            </div>
            {onUpgrade && (
              <button
                onClick={() => onUpgrade("team")}
                className="mt-4 w-full rounded-lg border border-border px-3 py-2 text-xs font-semibold text-text-primary hover:bg-surface-overlay"
              >
                Solicitar Team Hive
              </button>
            )}
          </div>
        </div>
      </section>
    </div>
  );
}

// ── Auth choice (when not logged in) ────────────────────────────────

function AuthChoice({
  onEmail,
  onDevice,
  loading,
  error,
}: {
  onEmail: () => void;
  onDevice: () => void;
  loading: boolean;
  error: string | null;
}) {
  return (
    <div className="space-y-4">
      <div className="text-center">
        <h2 className="text-base font-semibold text-text-primary">Vincular Identidad de Cuenta</h2>
        <p className="mt-0.5 text-xs text-text-secondary">
          Activa tu llave anónima Ed25519 en un toque o inicia sesión con correo para sincronizar planes y llaves API.
        </p>
      </div>

      {error && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
          {error}
        </div>
      )}

      <div className="grid grid-cols-1 gap-3 sm:grid-cols-2">
        <button
          onClick={onDevice}
          disabled={loading}
          className="flex flex-col items-center gap-2.5 rounded-xl border border-paw-500/40 bg-surface-raised p-5 text-center transition-colors hover:border-paw-500 hover:bg-surface-overlay"
        >
          <Fingerprint size={28} className="text-paw-500" />
          <div>
            <h3 className="text-sm font-medium text-text-primary">Llave de Dispositivo (Recomendado)</h3>
            <p className="mt-1 text-xs text-text-secondary">
              Privacidad máxima con tu llave criptográfica Ed25519. Sin correo ni contraseñas.
            </p>
          </div>
        </button>

        <button
          onClick={onEmail}
          disabled={loading}
          className="flex flex-col items-center gap-2.5 rounded-xl border border-border bg-surface-raised p-5 text-center transition-colors hover:border-paw-500/50 hover:bg-surface-overlay"
        >
          <Mail size={28} className="text-paw-500" />
          <div>
            <h3 className="text-sm font-medium text-text-primary">Cuenta con Email</h3>
            <p className="mt-1 text-xs text-text-secondary">
              Ideal para equipos, facturación Pro/Team y recuperación multi-dispositivo.
            </p>
          </div>
        </button>
      </div>
    </div>
  );
}

// ── Email login/register form ───────────────────────────────────────

function EmailForm({
  onBack,
  onLogin,
  onRegister,
  loading,
  error,
}: {
  onBack: () => void;
  onLogin: (email: string, password: string) => Promise<void>;
  onRegister: (email: string, password: string, name: string) => Promise<void>;
  loading: boolean;
  error: string | null;
}) {
  const [mode, setMode] = useState<"login" | "register">("login");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [name, setName] = useState("");
  const [showPassword, setShowPassword] = useState(false);

  const handleSubmit = async (e: React.FormEvent) => {
    e.preventDefault();
    if (mode === "login") {
      await onLogin(email, password);
    } else {
      await onRegister(email, password, name);
    }
  };

  return (
    <div className="mx-auto max-w-sm space-y-4">
      <button
        onClick={onBack}
        className="text-sm text-text-secondary hover:text-text-primary"
      >
        &larr; Volver
      </button>

      <div className="flex rounded-lg border border-border bg-surface p-0.5">
        <button
          onClick={() => setMode("login")}
          className={`flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${mode === "login" ? "bg-surface-overlay text-text-primary" : "text-text-secondary"}`}
        >
          Iniciar Sesión
        </button>
        <button
          onClick={() => setMode("register")}
          className={`flex-1 rounded-md px-3 py-1.5 text-sm font-medium transition-colors ${mode === "register" ? "bg-surface-overlay text-text-primary" : "text-text-secondary"}`}
        >
          Registrarse
        </button>
      </div>

      {error && (
        <div className="rounded-lg border border-danger/30 bg-danger/10 px-4 py-3 text-sm text-danger">
          {error}
        </div>
      )}

      <form onSubmit={handleSubmit} className="space-y-3">
        {mode === "register" && (
          <input
            type="text"
            placeholder="Nombre o alias"
            value={name}
            onChange={(e) => setName(e.target.value)}
            className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm text-text-primary placeholder:text-text-muted"
            required
          />
        )}
        <input
          type="email"
          placeholder="Email"
          value={email}
          onChange={(e) => setEmail(e.target.value)}
          className="w-full rounded-lg border border-border bg-surface px-3 py-2 text-sm text-text-primary placeholder:text-text-muted"
          required
        />
        <div className="relative">
          <input
            type={showPassword ? "text" : "password"}
            placeholder="Contraseña (mín. 8 caracteres)"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
            className="w-full rounded-lg border border-border bg-surface px-3 py-2 pr-10 text-sm text-text-primary placeholder:text-text-muted"
            required
            minLength={8}
          />
          <button
            type="button"
            onClick={() => setShowPassword(!showPassword)}
            className="absolute right-2 top-2 text-text-muted hover:text-text-secondary"
          >
            {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
          </button>
        </div>
        <button
          type="submit"
          disabled={loading}
          className="w-full rounded-lg bg-paw-500 px-4 py-2 text-sm font-medium text-white transition-colors hover:bg-paw-600 disabled:opacity-50"
        >
          {loading ? "..." : mode === "login" ? "Entrar" : "Crear Cuenta"}
        </button>
      </form>
    </div>
  );
}

// ── Authenticated account view ──────────────────────────────────────

function AccountDashboard({
  account,
  karmaStatus,
  onLogout,
  onUpgrade,
}: {
  account: NonNullable<ReturnType<typeof useAccount>["account"]>;
  karmaStatus: KarmaStatus | null;
  onLogout: () => void;
  onUpgrade: (plan: string) => void;
}) {
  const { summary, loading: usageLoading, refresh: refreshUsage } = useUsage();
  const { keys, loading: keysLoading, refresh: refreshKeys, create, revoke } = useApiKeys();
  const [newKeyLabel, setNewKeyLabel] = useState("");
  const [newKey, setNewKey] = useState<string | null>(null);

  useEffect(() => {
    refreshUsage();
    refreshKeys();
  }, [refreshUsage, refreshKeys]);

  const handleCreateKey = async () => {
    const label = newKeyLabel.trim() || "default";
    const result = await create(label);
    setNewKey(result.key);
    setNewKeyLabel("");
  };

  const displayPubkey = account.pubkey || karmaStatus?.node_id;

  return (
    <div className="space-y-4">
      {/* Identity section */}
      <section className="rounded-xl border border-border bg-surface-raised p-5">
        <div className="flex flex-wrap items-center justify-between gap-3">
          <div className="flex items-center gap-3">
            {account.auth_mode === "device" ? (
              <Fingerprint size={24} className="text-paw-500" />
            ) : (
              <UserCircle size={24} className="text-paw-500" />
            )}
            <div>
              {account.auth_mode === "email" ? (
                <>
                  <p className="font-medium text-text-primary">{account.display_name || "Usuario HiveBear"}</p>
                  <p className="text-sm text-text-secondary">{account.email}</p>
                </>
              ) : (
                <>
                  <p className="font-medium text-text-primary">Nodo Anónimo Ed25519</p>
                  <p className="font-mono text-xs text-text-secondary">
                    {displayPubkey ? `${displayPubkey.slice(0, 20)}…` : "Identidad local activa"}
                  </p>
                </>
              )}
            </div>
          </div>
          <div className="flex items-center gap-3">
            <TierBadge tier={karmaStatus?.tier ?? account.tier} />
            <button
              onClick={onLogout}
              className="text-xs text-text-secondary hover:text-danger"
            >
              Cerrar sesión
            </button>
          </div>
        </div>
      </section>

      <KarmaEconomySection karmaStatus={karmaStatus} onUpgrade={onUpgrade} />

      {/* Usage section */}
      <section className="rounded-xl border border-border bg-surface-raised p-5">
        <h3 className="mb-3 text-sm font-medium text-text-secondary">Actividad del Nodo</h3>
        {usageLoading ? (
          <p className="text-sm text-text-muted">Cargando métricas…</p>
        ) : summary ? (
          <div className="grid grid-cols-3 gap-4">
            <div>
              <p className="text-2xl font-bold text-text-primary">{summary.cloud_requests_today}</p>
              <p className="text-xs text-text-secondary">
                Conversaciones activas
                {summary.cloud_requests_limit && ` / ${summary.cloud_requests_limit}`}
              </p>
            </div>
            <div>
              <p className="text-2xl font-bold text-text-primary">
                {Math.round(summary.mesh_seconds_contributed / 60)}m
              </p>
              <p className="text-xs text-text-secondary">Cómputo P2P aportado</p>
            </div>
            <div>
              <p className="text-2xl font-bold text-text-primary">{summary.model_downloads_this_month}</p>
              <p className="text-xs text-text-secondary">Modelos locales instalados</p>
            </div>
          </div>
        ) : (
          <p className="text-sm text-text-muted">Sin datos de uso registrados aún.</p>
        )}
      </section>

      {/* API Keys section */}
      <section className="rounded-xl border border-border bg-surface-raised p-5">
        <div className="mb-3 flex flex-wrap items-center justify-between gap-2">
          <div>
            <h3 className="text-sm font-medium text-text-primary">Llaves API Locales (OpenAI / Ollama)</h3>
            <p className="text-xs text-text-secondary">
              Úsalas para conectar VS Code, Cursor o apps externas al servidor local de HiveBear.
            </p>
          </div>
          <div className="flex items-center gap-2">
            <input
              type="text"
              placeholder="Etiqueta (ej. Cursor)"
              value={newKeyLabel}
              onChange={(e) => setNewKeyLabel(e.target.value)}
              className="rounded-md border border-border bg-surface px-2.5 py-1.5 text-xs text-text-primary placeholder:text-text-muted"
            />
            <button
              onClick={handleCreateKey}
              className="flex items-center gap-1 rounded-md bg-paw-500 px-3 py-1.5 text-xs font-medium text-black hover:bg-paw-400"
            >
              <Plus size={12} /> Crear
            </button>
          </div>
        </div>

        {newKey && (
          <div className="mb-3 rounded-lg border border-success/30 bg-success/10 p-3">
            <p className="mb-1 text-xs font-medium text-success">
              Nueva llave API creada — cópiala ahora:
            </p>
            <div className="flex items-center gap-2">
              <code className="flex-1 break-all rounded bg-surface px-2 py-1 font-mono text-xs text-text-primary">
                {newKey}
              </code>
              <button
                onClick={() => navigator.clipboard.writeText(newKey)}
                className="text-text-secondary hover:text-text-primary"
              >
                <Copy size={14} />
              </button>
            </div>
          </div>
        )}

        {keysLoading ? (
          <p className="text-sm text-text-muted">Cargando llaves…</p>
        ) : keys.length === 0 ? (
          <p className="text-xs text-text-muted">No has creado llaves API todavía.</p>
        ) : (
          <div className="space-y-2">
            {keys.map((key) => (
              <div
                key={key.id}
                className="flex items-center justify-between rounded-lg border border-border bg-surface px-3 py-2"
              >
                <div>
                  <code className="text-xs text-text-primary">{key.key_prefix}…</code>
                  <span className="ml-2 text-xs text-text-secondary">{key.label}</span>
                </div>
                <button
                  onClick={() => revoke(key.id)}
                  className="text-text-muted hover:text-danger"
                >
                  <Trash2 size={14} />
                </button>
              </div>
            ))}
          </div>
        )}
      </section>
    </div>
  );
}

// ── Main Account page ───────────────────────────────────────────────

export default function Account() {
  const { account, loading, error, login, register, activateDevice, logout } = useAccount();
  const [showEmail, setShowEmail] = useState(false);
  const [submitting, setSubmitting] = useState(false);
  const [karmaStatus, setKarmaStatus] = useState<KarmaStatus | null>(null);

  useEffect(() => {
    let active = true;
    getKarmaStatus()
      .then((ks) => {
        if (active) setKarmaStatus(ks);
      })
      .catch(() => {});
    return () => {
      active = false;
    };
  }, []);

  const handleUpgrade = async (plan: string) => {
    try {
      const url = await invoke<string>("create_checkout", { plan });
      window.open(url, "_blank", "noopener,noreferrer");
    } catch {
      window.open(`https://kuromi04.github.io/HiveBear/#economy-${plan}`, "_blank", "noopener,noreferrer");
    }
  };

  if (loading) {
    return (
      <div className="flex h-full items-center justify-center">
        <p className="text-text-muted">Cargando cuenta y economía Karma…</p>
      </div>
    );
  }

  if (account) {
    return (
      <div className="mx-auto max-w-3xl space-y-4 p-4 sm:p-6">
        <h1 className="text-lg font-semibold text-text-primary">Cuenta & Economía Karma</h1>
        <AccountDashboard
          account={account}
          karmaStatus={karmaStatus}
          onLogout={logout}
          onUpgrade={handleUpgrade}
        />
      </div>
    );
  }

  // Not authenticated yet — still show live Karma Economy + quick Device/Email activation
  return (
    <div className="mx-auto max-w-3xl space-y-6 p-4 sm:p-6">
      <h1 className="text-lg font-semibold text-text-primary">Cuenta & Economía Karma</h1>
      <KarmaEconomySection karmaStatus={karmaStatus} onUpgrade={handleUpgrade} />
      {showEmail ? (
        <EmailForm
          onBack={() => setShowEmail(false)}
          onLogin={async (e, p) => {
            setSubmitting(true);
            try {
              await login(e, p);
            } finally {
              setSubmitting(false);
            }
          }}
          onRegister={async (e, p, n) => {
            setSubmitting(true);
            try {
              await register(e, p, n);
            } finally {
              setSubmitting(false);
            }
          }}
          loading={submitting}
          error={error}
        />
      ) : (
        <AuthChoice
          onEmail={() => setShowEmail(true)}
          onDevice={async () => {
            setSubmitting(true);
            try {
              await activateDevice();
            } finally {
              setSubmitting(false);
            }
          }}
          loading={submitting}
          error={error}
        />
      )}
    </div>
  );
}
