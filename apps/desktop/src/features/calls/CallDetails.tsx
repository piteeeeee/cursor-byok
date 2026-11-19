import type { CallDetail } from "../../shared/api";
import styles from "./CallDetails.module.scss";

const show = (value: string | number | null) => value ?? "-";
const timing = (value: number | null) => value == null ? "-" : `${value} ms`;

export function CallDetails({ detail }: { detail: CallDetail }) {
  const { call } = detail;
  const fields: Array<[string, string | number]> = [
    ["Call ID", call.call_id],
    [t("调用类型"), call.call_kind === "cursor_official" ? t("Cursor 官方") : "LLM"],
    [t("路由"), call.route === "cursor_official" ? t("Cursor 官方") : "BYOK"],
    ["Run ID", call.run_id],
    ["Conversation ID", call.conversation_id],
    [t("上游调用序号"), call.provider_call_index],
    ["Model Hash", show(call.model_hash)],
    [t("上游类型"), call.provider_type],
    [t("上游地址"), call.provider_url],
    [t("最终请求类型"), call.request_type],
    [t("最终请求地址"), call.request_url],
    ["Model ID", call.model_id],
    [t("显示名称"), call.display_name],
    [t("思考强度"), show(call.reasoning_effort)],
    ["Fast", call.fast == null ? "-" : call.fast ? t("是") : t("否")],
    [t("状态"), call.status],
    ["Finish Reason", show(call.finish_reason)],
    ["HTTP Status", show(call.http_status)],
    ["Created At", `${call.created_at_ms} · ${new Date(call.created_at_ms).toLocaleString()}`],
    [t("耗时"), timing(call.duration_ms)],
    ["TTFB", timing(call.ttfb_ms)],
    ["TTFR", timing(call.ttfr_ms)],
    ["TTFT", timing(call.ttft_ms)],
    ["Input Token", show(call.input_tokens)],
    ["Output Token", show(call.output_tokens)],
    ["Total Token", show(call.total_tokens)],
    ["Cache Read Token", show(call.cache_read_tokens)],
    ["Cache Write Token", show(call.cache_write_tokens)],
    ["Reasoning Token", show(call.reasoning_tokens)],
    [t("消息数"), call.message_count],
    [t("工具数"), call.tool_count],
    ["Error Kind", show(call.error_kind)],
    ["Error Message", show(call.error_message)],
  ];

  return <div className={styles.root}>
    <section>
      <dl className={styles.details}>{fields.map(([label, value]) => <div key={label}><dt>{label}</dt><dd><code>{value}</code></dd></div>)}</dl>
    </section>
  </div>;
}
