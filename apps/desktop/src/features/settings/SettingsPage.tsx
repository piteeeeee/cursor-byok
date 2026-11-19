import { useEffect, useState } from "react";
import { api, type ProxySettings, type StatisticsStorage } from "../../shared/api";
import { PageContent } from "../../shell/layout/PageContent";
import { AppLifecycleSettingsCard } from "./AppLifecycleSettingsCard";
import { PricingSettingsCard } from "./PricingSettingsCard";
import { ProxySettingsCard } from "./ProxySettingsCard";
import { ConfirmDialog } from "../../shared/ui/ConfirmDialog";
import { Select } from "../../shared/ui/Select";
import { TitledCard } from "../../shared/ui/TitledCard";
import { setLocalePreference, useI18n, type LocalePreference } from "../../i18n/store";
import { useMessage } from "../../shared/ui/message";
import { appStore, useAppStore } from "../../shared/store/appStore";
import { themeOptions } from "../../shared/theme/theme";
import styles from "./SettingsPage.module.scss";

export function SettingsPage() {
  const { ports, theme } = useAppStore();
  const { preference, locale } = useI18n();
  const message = useMessage();
  const [storage, setStorage] = useState<StatisticsStorage | null>(null);
  const [confirmClear, setConfirmClear] = useState(false);
  const [clearing, setClearing] = useState(false);
  const [outboundProxy, setOutboundProxy] = useState<ProxySettings | null>(null);
  useEffect(() => {
    const report = (cause: unknown) => message(cause instanceof Error ? cause.message : String(cause));
    void api.statisticsStorage().then(setStorage).catch(report);
    void api.proxySettings().then(setOutboundProxy).catch(report);
  }, [message]);
  const clearStorage = async () => {
    try {
      setClearing(true);
      setStorage(await api.clearStatisticsStorage());
      setConfirmClear(false);
      await appStore.refresh();
      message(t("调用统计已清理"));
    } catch (cause) {
      message(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setClearing(false);
    }
  };
  const clearTitle = t("确定要清理统计数据吗？");
  const clearDescription = t("所有调用汇总都会被删除。模型配置、CA 和应用设置不会受到影响，此操作无法撤销。");
  const content = (
    <div className={styles.page}>
      <TitledCard title={t("端口设置")}>
        <div className={styles.portSettings}>
          <div className={styles.portFields}>
            <div className={styles.portValue}><strong>{t("代理端口")}</strong><span>{ports.proxy_port}</span></div>
            <div className={styles.portValue}><strong>{t("服务端口")}</strong><span>{ports.service_port}</span></div>
          </div>
          <div className={styles.portFooter}>
            <small>{t("端口由配置文件决定，修改 ~/.cursor-byok-lite/config.toml 后重启生效。")}</small>
          </div>
        </div>
      </TitledCard>
      <ProxySettingsCard settings={outboundProxy} />
      <PricingSettingsCard />
      <AppLifecycleSettingsCard />
      <TitledCard title={t("模型")}>
        <div className={styles.settingRow}>
          <div>
            <strong>{t("配置文件")}</strong>
            <small>{t("模型与端口现在由 ~/.cursor-byok-lite/config.toml 提供；修改后重启生效。")}</small>
          </div>
        </div>
      </TitledCard>
      <TitledCard title={t("语言")}>
        <div className={styles.settingRow}>
          <div>
            <strong>{t("界面语言")}</strong>
            <small>{t("默认跟随操作系统；不支持的系统语言使用英文。当前：{language}", { language: locale === "zh-CN" ? "简体中文" : locale === "pt-BR" ? "Português (Brasil)" : "English" })}</small>
          </div>
          <div className={styles.languageControl}>
            <Select
              value={preference}
              ariaLabel={t("界面语言")}
              options={[
                { value: "system", label: t("跟随系统") },
                { value: "zh-CN", label: "简体中文" },
                { value: "en-US", label: "English" },
                { value: "pt-BR", label: "Português (Brasil)" },
              ]}
              onChange={(value) => setLocalePreference(value as LocalePreference)}
            />
          </div>
        </div>
      </TitledCard>
      <TitledCard title={t("主题")}>
        <div className={styles.themeActions}>
          {themeOptions.map(({ id }) => (
            <button
              className={theme === id ? styles.selected : ""}
              key={id}
              onClick={() => appStore.selectTheme(id)}
            >
              {id === "default-dark" ? t("默认暗色") : t("默认亮色")}
            </button>
          ))}
        </div>
      </TitledCard>
      <TitledCard title={t("存储管理")}>
        <div className={styles.storageRow}>
          <div>
            <strong>{t("统计数据")}</strong>
            <small>{storage ? t("调用记录 {calls} 条", { calls: storage.call_count }) : t("计算中…")}</small>
          </div>
          <button
            type="button"
            className={styles.textButton}
            onClick={() => setConfirmClear(true)}
          >
            {t("清理存储空间")}
          </button>
        </div>
      </TitledCard>
      <ConfirmDialog
        open={confirmClear}
        title={clearTitle}
        busy={clearing}
        cancelLabel={t("取消")}
        confirmLabel={t("确认清理")}
        onCancel={() => setConfirmClear(false)}
        onConfirm={() => void clearStorage()}
      >
        <div className={styles.confirmContent}>
          <small>{clearDescription}</small>
        </div>
      </ConfirmDialog>
    </div>
  );
  return <PageContent title={t("设置")} sections={[{ key: "settings", estimatedHeight: 1200, content }]} />;
}
