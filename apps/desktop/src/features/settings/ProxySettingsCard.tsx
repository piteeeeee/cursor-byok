import type { ProxySettings } from "../../shared/api";
import { TitledCard } from "../../shared/ui/TitledCard";
import styles from "./ProxySettingsCard.module.scss";

/** 出站代理只读展示：值来自配置文件里的 server.outbound_proxy。 */
export function ProxySettingsCard({ settings }: { settings: ProxySettings | null }) {
  return <TitledCard title={t("代理设置")}>
    <div className={styles.content}>
      <div className={styles.row}><strong>{t("代理方式")}</strong><span className={styles.value}>{settings ? (settings.mode === "custom" ? t("自定义") : t("默认")) : t("加载中…")}</span></div>
      {settings?.mode === "custom" && <div className={styles.customFields}>
        <div className={styles.row}><strong>{t("代理地址")}</strong><span className={styles.value}>{settings.address}</span></div>
        <div className={styles.row}><strong>{t("认证")}</strong><span className={styles.value}>{settings.auth_enabled ? t("已启用") : t("未启用")}</span></div>
      </div>}
      <div className={styles.row}><strong>{t("配置文件")}</strong><span className={styles.value}>{t("修改 ~/.cursor-byok-lite/config.toml 的 [server] outbound_proxy 后重启生效。")}</span></div>
    </div>
  </TitledCard>;
}
