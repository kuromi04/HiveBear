import type { HardwareProfile } from "../types";

export type TierId = "light" | "balanced" | "powerful" | "too_big";

export interface DeviceBudget {
  isMobile: boolean;
  totalRamGb: number;
  vramGb: number;
  /** Memory (GB) a model may occupy on this device. */
  budgetGb: number;
  /** Largest Q4-class model (billions of params) the device can hold. */
  maxParamsB: number;
  label: string;
}

export interface TierInfo {
  id: TierId;
  label: string;
  hint: string;
  badge: "success" | "accent" | "warning" | "danger";
}

const GB = 1024 ** 3;
/** Q4_K_M ≈ 0.6 GB per billion parameters (+ ~0.4 GB runtime / KV cache). */
const GB_PER_B = 0.6;
const OVERHEAD_GB = 0.4;

export function estimateModelGb(paramsBillions: number): number {
  return paramsBillions * GB_PER_B + OVERHEAD_GB;
}

export function deviceBudget(profile: HardwareProfile | null, viewportMobile: boolean): DeviceBudget {
  const isMobile = viewportMobile || !!profile?.platform.is_mobile;
  const totalRamGb = profile ? profile.memory.total_bytes / GB : isMobile ? 4 : 8;
  const vramGb = profile ? Math.max(0, ...profile.gpus.map((g) => g.vram_bytes / GB)) : 0;
  const budgetGb = isMobile
    ? Math.max(0.8, totalRamGb * 0.55)
    : Math.max(vramGb * 0.9, totalRamGb * 0.7);
  const maxParamsB = Math.max(0.5, (budgetGb - OVERHEAD_GB) / GB_PER_B);
  const label = isMobile
    ? `${totalRamGb.toFixed(1)} GB RAM · móvil`
    : vramGb >= 2
      ? `${totalRamGb.toFixed(0)} GB RAM · ${vramGb.toFixed(0)} GB VRAM`
      : `${totalRamGb.toFixed(0)} GB RAM · CPU`;
  return { isMobile, totalRamGb, vramGb, budgetGb, maxParamsB, label };
}

export function classifyModel(paramsBillions: number, budget: DeviceBudget): TierInfo {
  const ratio = estimateModelGb(paramsBillions) / budget.budgetGb;
  if (ratio <= 0.35) {
    return { id: "light", label: "Ligero", hint: "Muy fluido en este dispositivo", badge: "success" };
  }
  if (ratio <= 0.65) {
    return { id: "balanced", label: "Equilibrado", hint: "Buena calidad y velocidad", badge: "accent" };
  }
  if (ratio <= 1.0) {
    return {
      id: "powerful",
      label: "Potente",
      hint: "Usa casi toda la memoria; puede ir lento",
      badge: "warning",
    };
  }
  return budget.isMobile
    ? { id: "too_big", label: "Solo PC", hint: "Úsalo desde tu PC vía Mesh", badge: "danger" }
    : { id: "too_big", label: "Demasiado grande", hint: "Supera la memoria disponible", badge: "danger" };
}

export const TIER_ORDER: TierId[] = ["light", "balanced", "powerful", "too_big"];

export function tierTitle(id: TierId, isMobile: boolean): string {
  switch (id) {
    case "light":
      return "⚡ Ligeros · máxima fluidez";
    case "balanced":
      return "⚖️ Equilibrados · recomendados";
    case "powerful":
      return "🚀 Potentes · al límite de tu equipo";
    default:
      return isMobile ? "🖥️ Solo PC · usa Mesh" : "⛔ Demasiado grandes";
  }
}
