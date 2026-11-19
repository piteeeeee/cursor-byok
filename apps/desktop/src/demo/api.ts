import type {
  CursorHarnessStatus,
  Model,
  ProxySettings,
  StatisticsStorage,
} from "../shared/api";

const API_ROOT = "/__byok-api__/api";
const FIXED_NOW = Date.UTC(2026, 7, 27, 8, 0, 0);

const models: Model[] = [
  createModel({ hash: "mock-claude-sonnet", order: 1, name: "Claude Sonnet 4", type: "anthropic", url: "https://api.anthropic.com", modelId: "claude-sonnet-4-20250514" }),
  createModel({ hash: "mock-claude-opus", order: 2, name: "Claude Opus 4", type: "anthropic", url: "https://api.anthropic.com", modelId: "claude-opus-4-20250514" }),
  createModel({ hash: "mock-gpt", order: 3, name: "GPT-5.2", type: "openai", url: "https://api.openai.com", modelId: "gpt-5.2" }),
  createModel({ hash: "mock-o3", order: 4, name: "o3", type: "openai", url: "https://api.openai.com", modelId: "o3" }),
  createModel({ hash: "mock-deepseek-v3", order: 5, name: "DeepSeek V3.2", type: "openai", url: "https://api.deepseek.com", modelId: "deepseek-chat", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-deepseek-r1", order: 6, name: "DeepSeek R1", type: "openai", url: "https://api.deepseek.com", modelId: "deepseek-reasoner", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-gemini-pro", order: 7, name: "Gemini 2.5 Pro", type: "openai", url: "https://generativelanguage.googleapis.com", modelId: "gemini-2.5-pro", endpoint: "/v1beta/openai/chat/completions" }),
  createModel({ hash: "mock-gemini-flash", order: 8, name: "Gemini 2.5 Flash", type: "openai", url: "https://generativelanguage.googleapis.com", modelId: "gemini-2.5-flash", endpoint: "/v1beta/openai/chat/completions" }),
  createModel({ hash: "mock-qwen-max", order: 9, name: "Qwen3 Max", type: "openai", url: "https://dashscope.aliyuncs.com/compatible-mode", modelId: "qwen3-max", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-qwen-plus", order: 10, name: "Qwen Plus", type: "openai", url: "https://dashscope.aliyuncs.com/compatible-mode", modelId: "qwen-plus", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-kimi-k2", order: 11, name: "Kimi K2", type: "openai", url: "https://api.moonshot.cn", modelId: "kimi-k2-0711-preview", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-kimi-128k", order: 12, name: "Moonshot V1 128K", type: "openai", url: "https://api.moonshot.cn", modelId: "moonshot-v1-128k", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-glm-45", order: 13, name: "GLM-4.5", type: "openai", url: "https://open.bigmodel.cn", modelId: "glm-4.5", endpoint: "/api/paas/v4/chat/completions" }),
  createModel({ hash: "mock-glm-air", order: 14, name: "GLM-4.5-Air", type: "openai", url: "https://open.bigmodel.cn", modelId: "glm-4.5-air", endpoint: "/api/paas/v4/chat/completions" }),
  createModel({ hash: "mock-mistral-large", order: 15, name: "Mistral Large", type: "openai", url: "https://api.mistral.ai", modelId: "mistral-large-latest", endpoint: "/v1/chat/completions" }),
  createModel({ hash: "mock-mistral-small", order: 16, name: "Mistral Small", type: "openai", url: "https://api.mistral.ai", modelId: "mistral-small-latest", endpoint: "/v1/chat/completions" }),
];

let harnessStatus: CursorHarnessStatus = {
  platform: "macos",
  ca: "ready",
  configured_models: models.length,
  enabled_models: models.length,
  integration: "enabled",
  settings_applied: true,
  proxy_url: "http://127.0.0.1:54321",
  ca_install_command: null,
};

let portSettings = { proxy_port: 0, service_port: 0 };
let proxySettings: ProxySettings = {
  mode: "default",
  address: "",
  auth_enabled: false,
  username: "",
  has_password: false,
};
let storage: StatisticsStorage = { call_count: 24 };

export function installDemoApi() {
  const nativeFetch = window.fetch.bind(window);

  window.fetch = async (input, init) => {
    const requestUrl = input instanceof Request ? input.url : input instanceof URL ? input.href : input;
    const url = new URL(requestUrl, window.location.href);
    if (!url.pathname.startsWith(API_ROOT)) return nativeFetch(input, init);

    const path = url.pathname.slice(API_ROOT.length) || "/";
    const method = (init?.method ?? (input instanceof Request ? input.method : "GET")).toUpperCase();
    const body = await readBody(input, init);

    if (path === "/models" && method === "GET") return json(models);
    if (path === "/models" && method === "POST") return json(models);
    if (path === "/models/order") return json(models);
    if (path === "/models/discover") return json({ models: models.map((model) => model.model_id) });
    if (path === "/models/import-v0049" && method === "GET") {
      return json({ source: "demo", total: 0, new_models: 0, existing_models: 0, models: [] });
    }
    if (path === "/models/import-v0049") return json({ imported: 0, skipped: 0, total: 0 });
    if (/^\/models\/[^/]+\/test\/[^/]+$/.test(path) && method === "POST") {
      return json({ duration_ms: 1_284, first_valid_response_ms: 418, output_tokens: 42, tokens_per_second: 38.6, tokens_estimated: false, output: "Mock connectivity test passed." });
    }
    if (/^\/models\/[^/]+\/test\/[^/]+$/.test(path) || /^\/models\/[^/]+$/.test(path)) {
      return method === "DELETE" ? empty() : json(models[0]);
    }
    if (path === "/usage") return json({ today_tokens: 1200, month_tokens: 42000 });
    if (path === "/usage/export") return json("exports/demo.csv");
    if (path === "/harness/cursor/status") return json(harnessStatus);
    if (path === "/harness/cursor/ca/initialize") return json(harnessStatus);
    if (path === "/harness/cursor/enabled") {
      const enabled = Boolean((body as { enabled?: unknown } | null)?.enabled);
      harnessStatus = {
        ...harnessStatus,
        integration: enabled ? "enabled" : "disabled",
        settings_applied: enabled,
        proxy_url: enabled ? "http://127.0.0.1:54321" : null,
      };
      return json(harnessStatus);
    }
    if (path === "/settings/ports" && method === "GET") return json(portSettings);
    if (path === "/settings/ports") {
      portSettings = body as typeof portSettings;
      return json(portSettings);
    }
    if (path === "/settings/storage/statistics" && method === "GET") return json(storage);
    if (path === "/settings/storage/statistics") {
      storage = { call_count: 0 };
      return json(storage);
    }
    if (path === "/settings/proxy" && method === "GET") return json(proxySettings);
    if (path === "/settings/proxy") {
      const next = body as Partial<ProxySettings>;
      proxySettings = { ...proxySettings, ...next, has_password: Boolean(next.has_password) };
      return json(proxySettings);
    }
    if (path === "/settings/desktop" && method === "GET") return json({ silent_start: false, show_dock_icon: true });
    if (path === "/settings/desktop") return json(body);
    if (path === "/desktop/open-external-url") {
      const target = (body as { url?: string } | null)?.url;
      if (target) {
        const next = new URL(target, window.location.href);
        if (next.origin === window.location.origin && next.hash) window.location.hash = next.hash;
      }
      return empty();
    }

    return json({ message: `Unhandled demo endpoint: ${method} ${path}` }, 404);
  };
}

function createModel({ hash, order, name, type, url, modelId, endpoint = "/v1/responses" }: {
  hash: string;
  order: number;
  name: string;
  type: Model["type"];
  url: string;
  modelId: string;
  endpoint?: string;
}): Model {
  return {
    model_hash: hash,
    sort_order: order,
    display_name: name,
    group_name: null,
    type,
    base_url: url,
    use_full_url: false,
    api_key: "demo-key",
    tooltip_data: `${name} Mock 通道`,
    model_id: modelId,
    reasoning_effort: type === "openai" ? "high" : null,
    openai_endpoint: type === "openai" ? endpoint : "",
    openai_extra_params_enabled: false,
    openai_extra_params: {},
    custom_headers_enabled: false,
    custom_headers: {},
    anthropic_extra_params_enabled: false,
    anthropic_extra_params: {},
    context_window_tokens: 200_000,
    max_completion_tokens: type === "openai" ? 32_000 : null,
    anthropic_max_tokens: type === "anthropic" ? 32_000 : null,
    anthropic_thinking_effort: type === "anthropic" ? "high" : null,
    thinking_budget_tokens: null,
    created_at_ms: FIXED_NOW - order * 86_400_000,
    updated_at_ms: FIXED_NOW,
  };
}

async function readBody(input: RequestInfo | URL, init?: RequestInit): Promise<unknown> {
  const raw = init?.body ?? (input instanceof Request ? await input.clone().text() : null);
  if (typeof raw !== "string" || raw.length === 0) return null;
  try { return JSON.parse(raw) as unknown; }
  catch { return raw; }
}

function json(value: unknown, status = 200) {
  return new Response(JSON.stringify(value), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function empty() {
  return new Response(null, { status: 204 });
}
