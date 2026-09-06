/**
 * Composer chip + panel: pick a Chromium instance for chrome-devtools-mcp.
 */

import { useCallback, useEffect, useMemo, useState } from "react";
import { GlassModal } from "@/components/GlassModal";
import { IconWorld } from "@/components/icons";
import { Tip } from "@/components/ui/tooltip";
import { createT, type Locale } from "@/i18n";
import * as api from "@/lib/api";
import {
  browserChipClassName,
  browserChipState,
  browserStatusMessageKey,
  type BrowserBrokerSnapshot,
  type BrowserDebugStatus,
  type BrowserTarget,
} from "@/lib/browserBroker";

export function BrowserBrokerChip({ locale }: { locale: Locale }) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [open, setOpen] = useState(false);
  const [snap, setSnap] = useState<BrowserBrokerSnapshot | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSnap(await api.browserBrokerSnapshot());
    } catch {
      /* host not ready */
    }
  }, []);

  useEffect(() => {
    void refresh();
    const id = window.setInterval(() => {
      void refresh();
    }, 4000);
    return () => window.clearInterval(id);
  }, [refresh]);

  const chipState = browserChipState(snap);
  const tip = snap?.connected
    ? tr("composer.browserTipConnected")
    : tr("composer.browserTip");

  return (
    <>
      <Tip label={tip}>
        <button
          type="button"
          className={browserChipClassName(chipState)}
          aria-label={tr("composer.browser")}
          aria-pressed={open}
          onClick={() => setOpen(true)}
        >
          <IconWorld size={14} />
          <span className="chip__label">{tr("composer.browser")}</span>
        </button>
      </Tip>
      <BrowserBrokerModal
        locale={locale}
        open={open}
        snap={snap}
        onClose={() => setOpen(false)}
        onSnap={setSnap}
      />
    </>
  );
}

function statusLabel(
  tr: ReturnType<typeof createT>,
  status: BrowserDebugStatus,
): string {
  switch (status) {
    case "ready":
      return tr("composer.browserReady");
    case "running":
      return tr("composer.browserRunning");
    case "needs_allow":
      return tr("composer.browserNeedsAllow");
    case "launching":
      return tr("composer.browserLaunching");
    default:
      return tr("composer.browserDisconnected");
  }
}

function BrowserBrokerModal({
  locale,
  open,
  snap,
  onClose,
  onSnap,
}: {
  locale: Locale;
  open: boolean;
  snap: BrowserBrokerSnapshot | null;
  onClose: () => void;
  onSnap: (snap: BrowserBrokerSnapshot) => void;
}) {
  const tr = useMemo(() => createT(locale), [locale]);
  const [busyId, setBusyId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    void api
      .browserBrokerSnapshot()
      .then((next) => {
        if (!cancelled) onSnap(next);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [open, onSnap]);

  const run = async (id: string, fn: () => Promise<BrowserBrokerSnapshot>) => {
    setBusyId(id);
    setError(null);
    try {
      onSnap(await fn());
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  };

  const headline = tr(browserStatusMessageKey(snap));

  return (
    <GlassModal
      open={open}
      onClose={onClose}
      title={tr("composer.browserPanelTitle")}
      size="md"
      wrapBody
      closeLabel={tr("common.close")}
    >
      <p className="browser-broker__hint">{tr("composer.browserPanelHint")}</p>
      <div
        className={
          "browser-broker__status" +
          (snap?.connected ? " is-connected" : "")
        }
      >
        <span>
          {headline}
          {snap?.browserUrl && snap.browserUrl !== "in-app" ? (
            <span className="browser-broker__url">{snap.browserUrl}</span>
          ) : null}
        </span>
        {snap?.connected ? (
          <button
            type="button"
            className="btn btn--sm"
            disabled={busyId === "disconnect"}
            onClick={() => run("disconnect", () => api.browserBrokerDisconnect())}
          >
            {tr("composer.browserDisconnect")}
          </button>
        ) : null}
      </div>
      {snap?.keepAlive ? (
        <p className="browser-broker__hint">{tr("composer.browserKeepAlive")}</p>
      ) : null}
      {error || snap?.error ? (
        <p className="browser-broker__err">
          {tr("composer.browserErr", { error: error || snap?.error || "" })}
        </p>
      ) : null}
      <ul className="browser-broker__list">
        {(snap?.targets ?? []).map((t) => (
          <TargetRow
            key={t.id}
            target={t}
            busy={busyId === t.id}
            tr={tr}
            statusLabel={statusLabel(tr, t.debugStatus)}
            connected={!!snap?.connected && t.selected}
            onConnect={() =>
              t.selected && snap?.connected
                ? run("disconnect", () => api.browserBrokerDisconnect())
                : t.id === "dedicated"
                  ? run(t.id, () => api.browserBrokerLaunchDedicated())
                  : t.debugStatus === "ready" || t.id === "embedded"
                    ? run(t.id, () => api.browserBrokerConnect(t.id))
                    : run(t.id, () => api.browserBrokerOpenInspect(t.id))
            }
            onInspect={() =>
              run(t.id, () => api.browserBrokerOpenInspect(t.id))
            }
          />
        ))}
      </ul>
      {!snap?.targets.length ? (
        <p className="browser-broker__hint">{tr("composer.browserNone")}</p>
      ) : null}
    </GlassModal>
  );
}

function TargetRow({
  target,
  busy,
  connected,
  tr,
  statusLabel: status,
  onConnect,
  onInspect,
}: {
  target: BrowserTarget;
  busy: boolean;
  connected: boolean;
  tr: ReturnType<typeof createT>;
  statusLabel: string;
  onConnect: () => void;
  onInspect: () => void;
}) {
  const primary = connected
    ? tr("composer.browserDisconnect")
    : target.id === "dedicated"
      ? tr("composer.browserLaunch")
      : target.debugStatus === "ready" || target.id === "embedded"
        ? tr("composer.browserConnect")
        : tr("composer.browserOpenInspect");
  const name =
    target.id === "embedded"
      ? tr("composer.browserEmbedded")
      : target.id === "dedicated"
        ? tr("composer.browserDedicated")
        : target.name;
  return (
    <li
      className={
        "browser-broker__row" + (target.selected ? " is-selected" : "")
      }
    >
      <div className="browser-broker__meta">
        <div className="browser-broker__name">{name}</div>
        <div className="browser-broker__detail">
          {status}
          {target.detail && target.id !== "embedded"
            ? ` · ${target.detail}`
            : ""}
        </div>
        {target.id === "embedded" ? (
          <div className="browser-broker__detail">
            {tr("composer.browserEmbeddedHint")}
          </div>
        ) : null}
        {target.id === "dedicated" ? (
          <div className="browser-broker__detail">
            {tr("composer.browserDedicatedHint")}
          </div>
        ) : null}
        {target.debugStatus === "needs_allow" && target.id !== "embedded" ? (
          <div className="browser-broker__detail">
            {tr("composer.browserNeedsAllowHint")}
          </div>
        ) : null}
      </div>
      <div className="browser-broker__actions">
        <button
          type="button"
          className={
            "btn btn--sm" + (connected ? "" : " btn--primary")
          }
          disabled={busy}
          onClick={onConnect}
        >
          {busy ? tr("composer.browserLaunching") : primary}
        </button>
        {target.debugStatus === "needs_allow" &&
        target.id !== "dedicated" &&
        target.id !== "embedded" &&
        !connected ? (
          <button
            type="button"
            className="btn btn--sm"
            disabled={busy}
            onClick={onInspect}
          >
            {tr("composer.browserOpenInspect")}
          </button>
        ) : null}
      </div>
    </li>
  );
}
