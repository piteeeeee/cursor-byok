import { useEffect, useRef, useState } from "react";
import { api } from "../../shared/api";
import { CursorCaGate, CursorCaProvider } from "./CursorGates";
import { CursorModelCards, cursorModelGroups, type CursorModelGrouping } from "./CursorModelCards";
import { CursorModelTestResult, type CursorModelTestState } from "./CursorModelTestResult";
import styles from "./CursorSettings.module.scss";
import { PageContent } from "../../shell/layout/PageContent";
import { ConfirmDialog } from "../../shared/ui/ConfirmDialog";
import { Switch } from "../../shared/ui/Switch";
import { TooltipTrigger } from "../../shared/ui/TooltipTrigger";
import { useMessage } from "../../shared/ui/message";
import { PageActions } from "../../shell/PageActions";
import { appStore, useAppStore } from "../../shared/store/appStore";

export function CursorSettingsPage() {
  const { models, cursorHarness, cursorBusy } = useAppStore();
  const message = useMessage();
  const [caCommand, setCaCommand] = useState<string | null>(null);
  const [waitingForCaRefresh, setWaitingForCaRefresh] = useState(false);
  const [confirmDisableTakeover, setConfirmDisableTakeover] = useState(false);
  const [testingModelHashes, setTestingModelHashes] = useState<Set<string>>(() => new Set());
  const [modelTestResults, setModelTestResults] = useState<Map<string, CursorModelTestState>>(() => new Map());
  const [batchTesting, setBatchTesting] = useState(false);
  const [grouping, setGrouping] = useState<CursorModelGrouping>("flat");
  const activeModelTests = useRef(new Map<string, { testId: string; controller: AbortController; cancelling: boolean }>());
  const caReady = cursorHarness?.ca === "ready";
  const cursorTakenOver = cursorHarness?.settings_applied ?? false;
  const takeoverLabel = cursorTakenOver ? t("关闭接管Cursor") : t("开启接管Cursor");
  const testTargets = models.map((model) => ({
    model_hash: model.model_hash,
    display_name: model.display_name,
  }));
  const providerGroups = cursorModelGroups(models, "provider");
  const typeGroups = cursorModelGroups(models, "type");
  const canGroupByProvider = providerGroups.length > 1;
  const canGroupByType = typeGroups.length > 1;

  useEffect(() => {
    if ((grouping === "provider" && !canGroupByProvider) || (grouping === "type" && !canGroupByType)) {
      setGrouping("flat");
    }
  }, [canGroupByProvider, canGroupByType, grouping]);

  useEffect(() => {
    if (caCommand) void api.copyCursorText(caCommand);
  }, [caCommand]);

  const initializeCa = async () => {
    const status = await appStore.initializeCursorCa();
    if (status?.ca === "untrusted" && status.ca_install_command) setCaCommand(status.ca_install_command);
  };
  const cancelModelTest = async (modelHash: string) => {
    const active = activeModelTests.current.get(modelHash);
    if (!active || active.cancelling) return;
    active.cancelling = true;
    active.controller.abort();
    try {
      await api.cancelModelTest(modelHash, active.testId);
    } catch (cause) {
      message(t("取消测试失败：{error}", { error: errorText(cause) }), { duration: 5000 });
    }
  };
  const cancelAllModelTests = async () => {
    await Promise.all([...activeModelTests.current.keys()].map((modelHash) => cancelModelTest(modelHash)));
  };
  const testModel = async (model: { model_hash: string; display_name: string }, notify = true): Promise<"success" | "failure" | "cancelled"> => {
    if (activeModelTests.current.has(model.model_hash)) {
      await cancelModelTest(model.model_hash);
      return "cancelled";
    }
    const active = { testId: crypto.randomUUID(), controller: new AbortController(), cancelling: false };
    activeModelTests.current.set(model.model_hash, active);
    setTestingModelHashes((current) => new Set(current).add(model.model_hash));
    try {
      const result = await api.testModel(model.model_hash, active.testId, active.controller.signal);
      setModelTestResults((current) => new Map(current).set(model.model_hash, { status: "success", result }));
      if (notify) message(t("模型 {model} 连通性测试成功（{duration} ms）", { model: model.display_name, duration: result.duration_ms }));
      return "success";
    } catch (cause) {
      if (active.cancelling || active.controller.signal.aborted) {
        setModelTestResults((current) => new Map(current).set(model.model_hash, { status: "cancelled" }));
        return "cancelled";
      }
      const error = errorText(cause);
      setModelTestResults((current) => new Map(current).set(model.model_hash, { status: "error", error }));
      if (notify) message(t("连通性测试失败：{error}", { error }), { duration: 5000 });
      return "failure";
    } finally {
      if (activeModelTests.current.get(model.model_hash) === active) activeModelTests.current.delete(model.model_hash);
      setTestingModelHashes((current) => {
        const next = new Set(current);
        next.delete(model.model_hash);
        return next;
      });
    }
  };
  const testAllModels = async () => {
    if (!testTargets.length || batchTesting) return;
    setBatchTesting(true);
    try {
      const results = await Promise.all(testTargets.map((model) => testModel(model, false)));
      const successful = results.filter((result) => result === "success").length;
      const failed = results.filter((result) => result === "failure").length;
      const cancelled = results.filter((result) => result === "cancelled").length;
      message(cancelled > 0
        ? t("连通性测试已取消：成功 {successful}，失败 {failed}", { successful, failed })
        : failed === 0
          ? t("全部 {count} 个模型连通性测试成功", { count: testTargets.length })
          : t("连通性测试完成：成功 {successful}，失败 {failed}", { successful, failed }),
      { duration: failed === 0 && cancelled === 0 ? 2400 : 5000 });
    } finally {
      setBatchTesting(false);
    }
  };

  const list = <CursorModelCards
    models={models}
    grouping={grouping}
    disabled={cursorBusy}
    testingModelHashes={testingModelHashes}
    testResults={modelTestResults}
    onTest={(model) => void testModel(model)}
  />;

  const refreshCa = async () => {
    await appStore.refresh();
    if (appStore.getSnapshot().cursorHarness?.ca !== "ready") setWaitingForCaRefresh(false);
  };
  const openCaTerminal = () => {
    if (caCommand) void api.openCursorCaInstallTerminal(caCommand).catch((cause) => message(errorText(cause)));
    setCaCommand(null);
    setWaitingForCaRefresh(true);
  };
  const content = <CursorCaProvider><CursorCaGate busy={cursorBusy} waitingForRefresh={waitingForCaRefresh} onInitialize={() => void initializeCa()} onRefresh={() => void refreshCa()}>
    <div className={styles.page}>
      <p className={styles.configHint}>{t("编辑 ~/.cursor-byok-lite/config.toml 后重启生效")}</p>
      {models.length > 0
        ? list
        : <div className={styles.gate}><strong>{t("还没有可供 Cursor 使用的模型")}</strong><span>{t("在 ~/.cursor-byok-lite/config.toml 中添加 [[models]] 后重启。")}</span></div>}
    </div>
  </CursorCaGate></CursorCaProvider>;

  const activeGroups = grouping === "provider" ? providerGroups : typeGroups;
  const estimatedModelHeight = grouping === "flat"
    ? Math.max(380, Math.ceil(models.length / 3) * 196)
    : Math.max(380, activeGroups.reduce((height, group) => height + 60 + group.models.length * 56, 0) + Math.max(0, activeGroups.length - 1) * 20);

  return <>
    <PageActions position="left">
      <div className={styles.takeoverActions}>
        <span className={styles.takeoverStatus}>{cursorTakenOver ? t("已接管") : t("未接管")}</span>
        <TooltipTrigger label={takeoverLabel}>
          <Switch
            checked={cursorTakenOver}
            disabled={cursorBusy || (!cursorTakenOver && !caReady)}
            label={takeoverLabel}
            onChange={(enabled) => {
              if (enabled) void appStore.setCursorEnabled(true);
              else setConfirmDisableTakeover(true);
            }}
          />
        </TooltipTrigger>
        {testTargets.length > 0 && <div className={styles.groupActions} role="group" aria-label={t("操作")}>
          {canGroupByProvider && <button type="button" aria-pressed={grouping === "provider"} onClick={() => setGrouping("provider")}>{t("按供应商")}</button>}
          {canGroupByType && <button type="button" aria-pressed={grouping === "type"} onClick={() => setGrouping("type")}>{t("按类型")}</button>}
          {grouping !== "flat" && <button type="button" aria-pressed={false} onClick={() => setGrouping("flat")}>{t("默认平铺")}</button>}
          <button type="button" disabled={cursorBusy || (!batchTesting && testingModelHashes.size > 0)} onClick={() => void (batchTesting ? cancelAllModelTests() : testAllModels())}>{batchTesting ? t("取消全部测试") : t("一键测试")}</button>
        </div>}
      </div>
    </PageActions>
    <PageContent title="Cursor" sections={[{ key: "cursor-settings", estimatedHeight: estimatedModelHeight, content }]} />
    <ConfirmDialog
      open={confirmDisableTakeover}
      title={t("关闭接管Cursor？")}
      cancelLabel={t("取消")}
      confirmLabel={t("关闭接管")}
      onCancel={() => setConfirmDisableTakeover(false)}
      onConfirm={() => {
        setConfirmDisableTakeover(false);
        void appStore.setCursorEnabled(false);
      }}
    >
      <p>{t("关闭后将移除 Cursor 本地代理配置。如果你需要登陆官方账号，通常不需要关闭操作，推荐直接登陆你的账号即可(byok模型与官方账号的模型已支持无缝衔接)，是否继续关闭并清理代理？")}</p>
    </ConfirmDialog>
    <ConfirmDialog open={caCommand !== null} title={t("安装本地 CA")} cancelLabel={t("关闭")} confirmLabel={t("打开终端")} onCancel={() => setCaCommand(null)} onConfirm={openCaTerminal}>
      <div className={styles.editor}><strong>{t("需要授权安装证书")}</strong><span>{t("安装命令已自动复制。点击“打开终端”，将命令粘贴到终端中执行，并按提示输入密码。")}</span><pre className={styles.command}>{caCommand}</pre></div>
    </ConfirmDialog>
    <CursorModelTestResult state={undefined} testing={false} />
  </>;
}

function errorText(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause);
}
