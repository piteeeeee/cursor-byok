import { api } from "../../shared/api";
import { useAppStore } from "../../shared/store/appStore";
import { useMessage } from "../../shared/ui/message";
import { PageContent } from "../../shell/layout/PageContent";
import { PageActions } from "../../shell/PageActions";

export function HomePage() {
  const { usage } = useAppStore();
  const message = useMessage();
  const exportCsv = () => void api.exportUsage().then(path => message(path)).catch(error => message(String(error)));
  return <>
    <PageActions><button onClick={exportCsv}>{t("导出 CSV")}</button></PageActions>
    <PageContent title={t("数据概览")} sections={[{ key: "usage", estimatedHeight: 150, content:
      <dl>
        <dt>{t("今日 Token")}</dt><dd>{usage.today_tokens.toLocaleString()}</dd>
        <dt>{t("本月 Token")}</dt><dd>{usage.month_tokens.toLocaleString()}</dd>
      </dl>,
    }]} />
  </>;
}
