import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useNavigate } from "react-router-dom";
import { useInstalledModels, useModelInstall, useModelRemove, useModelSearch } from "../hooks/useRegistry";
import { useModelLoader } from "../hooks/useInference";
import ModelCard from "../components/ModelCard";
import DownloadProgress from "../components/DownloadProgress";
import { Surface, EmptyState } from "../components/ui";
import { Search as SearchIcon, Package, Cpu } from "lucide-react";
import { useProfile } from "../hooks/useProfile";
import { useIsMobile } from "../hooks/useDevice";
import { classifyModel, deviceBudget, TIER_ORDER, tierTitle, type TierId } from "../lib/modelTiers";
import type { SearchResult } from "../types";

type TierFilter = "all" | "recommended" | TierId;

export default function ModelBrowser() {
  const navigate = useNavigate();
  const [query, setQuery] = useState("");
  const { results, loading: searching, error: searchError, search } = useModelSearch();
  const { models: installed, refresh } = useInstalledModels();
  const { installing, progress, error: installError, install, cancel, clearError } = useModelInstall();
  const { removing, remove } = useModelRemove();
  const { load, error: loadError } = useModelLoader();
  const [loadingModelId, setLoadingModelId] = useState<string | null>(null);
  const [tab, setTab] = useState<"search" | "installed">("search");
  const [tierFilter, setTierFilter] = useState<TierFilter>("recommended");
  const { profile } = useProfile();
  const viewportMobile = useIsMobile();
  const budget = useMemo(() => deviceBudget(profile, viewportMobile), [profile, viewportMobile]);
  const goMesh = useCallback(() => navigate("/mesh"), [navigate]);

  const tiered = useMemo(() => {
    const withTier = results.map((r) => ({ r, tier: classifyModel(r.metadata.params_billions, budget) }));
    const counts: Record<TierId, number> = { light: 0, balanced: 0, powerful: 0, too_big: 0 };
    withTier.forEach((x) => { counts[x.tier.id] += 1; });
    const visible = withTier.filter(({ tier }) => {
      if (tierFilter === "all") return true;
      if (tierFilter === "recommended") return tier.id === "light" || tier.id === "balanced";
      return tier.id === tierFilter;
    });
    const groups = TIER_ORDER.map((id) => ({
      id,
      items: visible
        .filter((x) => x.tier.id === id)
        .sort((a, b) => b.r.metadata.params_billions - a.r.metadata.params_billions),
    })).filter((g) => g.items.length > 0);
    return { counts, groups, total: visible.length };
  }, [results, budget, tierFilter]);
  const debounceRef = useRef<ReturnType<typeof setTimeout> | undefined>(undefined);

  // Load top models on initial mount
  useEffect(() => {
    search("");
  }, [search]);

  const handleSearch = useCallback((e: React.FormEvent) => {
    e.preventDefault();
    search(query);
  }, [query, search]);

  // Debounced search-as-you-type
  const handleQueryChange = useCallback((val: string) => {
    setQuery(val);
    if (debounceRef.current) clearTimeout(debounceRef.current);
    if (val.trim().length >= 2) {
      debounceRef.current = setTimeout(() => search(val), 300);
    }
  }, [search]);

  useEffect(() => {
    return () => { if (debounceRef.current) clearTimeout(debounceRef.current); };
  }, []);

  const handleInstall = useCallback(async (modelId: string) => {
    const result = await install(modelId);
    if (result) {
      await refresh();
      setTab("installed");
    }
  }, [install, refresh]);

  const handleRemove = useCallback(async (modelId: string) => {
    const ok = await remove(modelId);
    if (ok) refresh();
  }, [remove, refresh]);

  const handleUseModel = useCallback(async (modelId: string) => {
    setLoadingModelId(modelId);
    try {
      const res = await load(modelId);
      if (res) {
        try { localStorage.setItem("preferred_handle_id", String(res.handle_id)); } catch { /* ignore */ }
        navigate("/chat");
      }
    } finally {
      setLoadingModelId(null);
    }
  }, [load, navigate]);

  return (
    <Surface>
      <div className="flex h-full flex-col -m-4 sm:-m-6">
        <div className="shrink-0 space-y-3 border-b border-border p-4 pb-3 sm:space-y-4 sm:p-6 sm:pb-4">
          <h1 className="text-lg font-semibold sm:text-xl">Models</h1>

          {/* Tabs */}
          <div className="flex gap-1 rounded-[var(--radius-md)] bg-surface-overlay p-0.5">
            {(["search", "installed"] as const).map((t) => (
              <button key={t} onClick={() => setTab(t)}
                className={[
                  "flex-1 rounded-[var(--radius-sm)] px-3 py-1.5 text-sm capitalize interactive-hover",
                  tab === t ? "bg-surface-raised text-text-primary font-medium shadow-[var(--shadow-raised)]" : "text-text-muted hover:text-text-secondary",
                ].join(" ")}>
                {t} {t === "installed" && `(${installed.length})`}
              </button>
            ))}
          </div>

          {/* Search */}
          {tab === "search" && (
            <form onSubmit={handleSearch} className="flex gap-2">
              <div className="relative flex-1 min-w-0">
                <SearchIcon size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-text-muted" />
                <input type="text" value={query} onChange={(e) => handleQueryChange(e.target.value)}
                  placeholder="Search models (e.g. qwen, smollm2, llama)..."
                  className="w-full rounded-[var(--radius-md)] border border-border bg-surface py-2 pl-9 pr-3 text-sm outline-none placeholder:text-text-muted focus:border-paw-500 focus:ring-2 focus:ring-paw-500/20" />
              </div>
              <button type="submit" className="interactive-hover press-scale shrink-0 rounded-[var(--radius-md)] bg-paw-500 px-3.5 py-2 text-sm font-medium text-white hover:bg-paw-600 sm:px-4">
                Search
              </button>
            </form>
          )}

          {tab === "search" && (
            <>
              <div className="flex items-center gap-2 rounded-[var(--radius-md)] border border-border bg-surface px-3 py-2 text-xs text-text-secondary">
                <Cpu size={14} className="shrink-0 text-paw-400" />
                <span className="min-w-0">
                  Tu equipo: <strong className="text-text-primary">{budget.label}</strong> · modelos de hasta ~{budget.maxParamsB.toFixed(1)}B parámetros
                </span>
              </div>
              <div className="-mx-1 flex gap-1.5 overflow-x-auto px-1 pb-1 [scrollbar-width:none]">
                {([
                  ["recommended", `Recomendados (${tiered.counts.light + tiered.counts.balanced})`],
                  ["light", `⚡ Ligeros (${tiered.counts.light})`],
                  ["balanced", `⚖️ Equilibrados (${tiered.counts.balanced})`],
                  ["powerful", `🚀 Potentes (${tiered.counts.powerful})`],
                  ["too_big", budget.isMobile ? `🖥️ Solo PC (${tiered.counts.too_big})` : `⛔ Muy grandes (${tiered.counts.too_big})`],
                  ["all", `Todos (${results.length})`],
                ] as [TierFilter, string][]).map(([id, label]) => (
                  <button key={id} onClick={() => setTierFilter(id)}
                    className={[
                      "shrink-0 whitespace-nowrap rounded-full border px-3 py-1.5 text-xs interactive-hover",
                      tierFilter === id
                        ? "border-paw-500 bg-paw-500/15 font-medium text-paw-400"
                        : "border-border text-text-muted hover:text-text-secondary",
                    ].join(" ")}>
                    {label}
                  </button>
                ))}
              </div>
            </>
          )}
        </div>

        {/* Download progress & error handling */}
        {(installing || installError) && (
          <div className="shrink-0 px-4 pt-3 sm:px-6 sm:pt-4">
            <DownloadProgress 
              modelId={installing || "Model"} 
              progress={progress} 
              error={installError}
              onCancel={() => { cancel(); clearError(); }}
              onRetry={installing ? undefined : () => { const id = installing; clearError(); if (id) install(id); }}
            />
          </div>
        )}

        {(searchError || loadError) && (
          <div className="mx-4 mt-3 rounded-[var(--radius-md)] border border-danger/30 bg-danger/10 px-4 py-2 text-sm text-danger sm:mx-6 sm:mt-4">
            {searchError || loadError}
          </div>
        )}

        {/* Results */}
        <div className="flex-1 overflow-y-auto p-4 pt-3 sm:p-6 sm:pt-4">
          {tab === "search" ? (
            searching ? (
              <p className="text-sm text-text-muted">Searching...</p>
            ) : results.length === 0 ? (
              <EmptyState
                icon={<SearchIcon size={24} />}
                title={query ? "No results found" : "Search for models"}
                description={query ? "Try a different search term." : "Discover models from HuggingFace and install them locally."}
              >
                {!query && (
                  <div className="mt-2 flex flex-wrap justify-center gap-2">
                    {["qwen", "smollm2", "llama", "tinyllama", "phi"].map((q) => (
                      <button
                        key={q}
                        onClick={() => { setQuery(q); search(q); }}
                        className="interactive-hover rounded-[var(--radius-md)] border border-border px-3 py-1.5 text-xs text-text-secondary hover:border-paw-500/30 hover:text-paw-400"
                      >
                        {q}
                      </button>
                    ))}
                  </div>
                )}
              </EmptyState>
            ) : (
              <div className="space-y-5">
                {tiered.groups.length === 0 && (
                  <p className="text-sm text-text-muted">
                    Ningún modelo en esta categoría. Prueba con otro segmento o busca algo distinto.
                  </p>
                )}
                {tiered.groups.map((g) => (
                  <section key={g.id} className="space-y-2">
                    <h2 className="text-xs font-semibold uppercase tracking-wide text-text-secondary">
                      {tierTitle(g.id, budget.isMobile)} <span className="text-text-muted">({g.items.length})</span>
                    </h2>
                    <div className="grid gap-3">
                      {g.items.map(({ r, tier }: { r: SearchResult; tier: ReturnType<typeof classifyModel> }) => (
                        <ModelCard key={r.metadata.id} result={r} installing={installing} tier={tier} onMesh={goMesh}
                          removing={removing} loadingId={loadingModelId} onInstall={handleInstall} onRemove={handleRemove} onUse={handleUseModel} />
                      ))}
                    </div>
                  </section>
                ))}
              </div>
            )
          ) : installed.length === 0 ? (
            <EmptyState
              icon={<Package size={24} />}
              title="No models installed"
              description="Search and install a model to get started."
              action={{ label: "Browse Models", onClick: () => setTab("search") }}
            />
          ) : (
            <div className="grid gap-3">
              {installed.map((m) => (
                <ModelCard key={m.id} result={{ metadata: m, is_installed: true, compatibility_score: null }}
                  installing={installing} removing={removing} loadingId={loadingModelId} onInstall={handleInstall} onRemove={handleRemove} onUse={handleUseModel} />
              ))}
            </div>
          )}
        </div>
      </div>
    </Surface>
  );
}
