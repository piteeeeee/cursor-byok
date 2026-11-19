import { useSyncExternalStore } from "react";
import { api, type CursorHarnessStatus, type Model, type UsageSummary, type PortSettings, type TokenPricingSettings } from "../api";
import { applyTheme, isThemeId, type ThemeId } from "../theme/theme";

export const DEFAULT_TOKEN_PRICING: TokenPricingSettings = {
  input_per_million: 5.0,
  output_per_million: 25.0,
  cache_read_per_million: 0.5,
  cache_write_per_million: 6.25,
};

export type AppSnapshot = {
  models: Model[];
  usage: UsageSummary;
  ports: PortSettings;
  pricing: TokenPricingSettings;
  busy: boolean;
  error: string | null;
  theme: ThemeId;
  cursorHarness: CursorHarnessStatus | null;
  cursorBusy: boolean;
};

const savedTheme = (): ThemeId => {
  const saved = localStorage.getItem("cursor-byok.theme");
  return isThemeId(saved) ? saved : "default-dark";
};

let snapshot: AppSnapshot = {
  models: [],
  usage: { today_tokens: 0, month_tokens: 0 },
  ports: { proxy_port: 0, service_port: 0 },
  pricing: DEFAULT_TOKEN_PRICING,
  busy: false,
  error: null,
  theme: savedTheme(),
  cursorHarness: null,
  cursorBusy: false,
};

const listeners = new Set<() => void>();

function update(patch: Partial<AppSnapshot>) {
  snapshot = { ...snapshot, ...patch };
  listeners.forEach((listener) => listener());
}

export const appStore = {
  subscribe(listener: () => void) {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
  getSnapshot: () => snapshot,

  async refresh() {
    update({ busy: true, error: null });
    try {
      const [models, usage, ports, pricing, cursorHarness] = await Promise.all([
        api.models(),
        api.usage(),
        api.ports(),
        api.pricingSettings(),
        api.cursorHarness(),
      ]);
      update({ models, usage, ports, pricing, cursorHarness });
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
    } finally {
      update({ busy: false });
    }
  },

  async initializeCursorCa() {
    update({ cursorBusy: true, error: null });
    try {
      const status = await api.initializeCursorCa();
      update({ cursorHarness: status });
      return status;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return null;
    } finally { update({ cursorBusy: false }); }
  },
  async setCursorEnabled(enabled: boolean) {
    update({ cursorBusy: true, error: null });
    try { update({ cursorHarness: await api.setCursorEnabled(enabled) }); }
    catch (cause) { update({ error: cause instanceof Error ? cause.message : String(cause) }); }
    finally { update({ cursorBusy: false }); }
  },
  async updatePricingSettings(pricing: TokenPricingSettings) {
    try {
      update({ error: null });
      update({ pricing: await api.setPricingSettings(pricing) });
      return true;
    } catch (cause) {
      update({ error: cause instanceof Error ? cause.message : String(cause) });
      return false;
    }
  },
  selectTheme(theme: ThemeId) {
    localStorage.setItem("cursor-byok.theme", theme);
    applyTheme(theme);
    update({ theme });
  },
};

export function useAppStore() {
  return useSyncExternalStore(appStore.subscribe, appStore.getSnapshot);
}
