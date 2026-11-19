export type ModelType = "openai" | "anthropic";

export interface Model {
  model_hash: string;
  sort_order: number;
  display_name: string;
  group_name: string | null;
  type: ModelType;
  base_url: string;
  use_full_url: boolean;
  api_key: string;
  tooltip_data: string;
  model_id: string;
  reasoning_effort: string | null;
  openai_endpoint: string;
  openai_extra_params_enabled: boolean;
  openai_extra_params: Record<string, unknown>;
  custom_headers_enabled: boolean;
  custom_headers: Record<string, string>;
  anthropic_extra_params_enabled: boolean;
  anthropic_extra_params: Record<string, unknown>;
  context_window_tokens: number | null;
  max_completion_tokens: number | null;
  anthropic_max_tokens: number | null;
  anthropic_thinking_effort: string | null;
  thinking_budget_tokens: number | null;
  created_at_ms: number;
  updated_at_ms: number;
}

export interface ModelConnectivityResult {
  duration_ms: number;
  first_valid_response_ms: number | null;
  output_tokens: number;
  tokens_per_second: number;
  tokens_estimated: boolean;
  output: string;
}

export type CaState = "missing" | "untrusted" | "ready" | "invalid";
export type IntegrationState = "disabled" | "enabled" | "degraded";
export interface CursorHarnessStatus {
  platform: string;
  ca: CaState;
  configured_models: number;
  enabled_models: number;
  integration: IntegrationState;
  settings_applied: boolean;
  proxy_url: string | null;
  ca_install_command: string | null;
}

export interface PortSettings {
  proxy_port: number;
  service_port: number;
}

export interface StatisticsStorage {
  call_count: number;
}

export type ProxyMode = "default" | "custom";

export interface ProxySettings {
  mode: ProxyMode;
  address: string;
  auth_enabled: boolean;
  username: string;
  has_password: boolean;
}

export interface DesktopSettings {
  silent_start: boolean;
  show_dock_icon: boolean;
}

export interface TokenPricingSettings {
  input_per_million: number;
  output_per_million: number;
  cache_read_per_million: number;
  cache_write_per_million: number;
}

export interface UsageSummary { today_tokens: number; month_tokens: number }

const packagedDesktop = "__TAURI_INTERNALS__" in window
  || window.location.protocol === "tauri:"
  || window.location.hostname === "tauri.localhost";
const API_ROOT = "/__byok-api__/api";

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  let response: Response;
  try {
    response = await fetch(`${API_ROOT}${path}`, {
      ...init,
      headers: init?.body ? { "content-type": "application/json", ...init.headers } : init?.headers,
    });
  } catch (cause) {
    throw new Error(t("无法连接本地管理服务"), { cause });
  }
  if (!response.ok) {
    const body = await response.text();
    let message = body;
    try {
      const parsed = JSON.parse(body) as { message?: unknown };
      if (typeof parsed.message === "string") message = parsed.message;
    } catch {
      // Plain-text errors are already suitable for display.
    }
    throw new Error(message || `${response.status} ${response.statusText}`);
  }
  if (response.status === 204) return undefined as T;
  return response.json() as Promise<T>;
}

export const api = {
  models: () => request<Model[]>("/models"),
  testModel: (hash: string, testId: string, signal?: AbortSignal) => request<ModelConnectivityResult>(`/models/${encodeURIComponent(hash)}/test/${encodeURIComponent(testId)}`, { method: "POST", signal }),
  cancelModelTest: (hash: string, testId: string) => request<void>(`/models/${encodeURIComponent(hash)}/test/${encodeURIComponent(testId)}`, { method: "DELETE" }),
  usage: () => request<UsageSummary>("/usage"),
  exportUsage: () => request<string>("/usage/export", { method: "POST" }),
  cursorHarness: () => request<CursorHarnessStatus>("/harness/cursor/status"),
  initializeCursorCa: () => request<CursorHarnessStatus>("/harness/cursor/ca/initialize", { method: "POST" }),
  openCursorCaInstallTerminal: async (command: string) => {
    if (!packagedDesktop) throw new Error(t("请在桌面应用中打开终端安装 CA"));
    const { invoke } = await import("@tauri-apps/api/core");
    await invoke("open_terminal_with_command", { command });
  },
  copyCursorText: async (text: string) => {
    if (!packagedDesktop) throw new Error(t("请在桌面应用中复制到系统剪贴板"));
    const { writeText } = await import("@tauri-apps/plugin-clipboard-manager");
    await writeText(text);
  },
  setCursorEnabled: (enabled: boolean) => request<CursorHarnessStatus>("/harness/cursor/enabled", { method: "PUT", body: JSON.stringify({ enabled }) }),
  openExternalUrl: (url: string) => request<void>("/desktop/open-external-url", { method: "POST", body: JSON.stringify({ url }) }),
  ports: () => request<PortSettings>("/settings/ports"),
  statisticsStorage: () => request<StatisticsStorage>("/settings/storage/statistics"),
  clearStatisticsStorage: () => request<StatisticsStorage>("/settings/storage/statistics", { method: "DELETE" }),
  proxySettings: () => request<ProxySettings>("/settings/proxy"),
  desktopSettings: () => request<DesktopSettings>("/settings/desktop"),
  setDesktopSettings: (settings: DesktopSettings) => request<DesktopSettings>("/settings/desktop", { method: "PUT", body: JSON.stringify(settings) }),
  pricingSettings: () => request<TokenPricingSettings>("/settings/pricing"),
  setPricingSettings: (settings: TokenPricingSettings) => request<TokenPricingSettings>("/settings/pricing", { method: "PUT", body: JSON.stringify(settings) }),
};
