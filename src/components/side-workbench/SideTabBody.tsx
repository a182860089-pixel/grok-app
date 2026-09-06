/**
 * Per-kind body for Side Workbench (non-file kinds; files use FilesWorkspace).
 */

import { lazy, Suspense, useMemo } from "react";
import { createT, type Locale } from "@/i18n";
import type { SideTab } from "@/lib/sideWorkbench";

const BrowserTab = lazy(async () => {
  const m = await import("./BrowserTab");
  return { default: m.BrowserTab };
});
const TerminalTab = lazy(async () => {
  const m = await import("./TerminalTab");
  return { default: m.TerminalTab };
});

export type SideTabBodyProps = {
  locale: Locale | string;
  tab: SideTab;
  projectPath?: string | null;
  sshAlias?: string | null;
  active?: boolean;
};

export function SideTabBody({
  locale,
  tab,
  projectPath = null,
  sshAlias = null,
  active = true,
}: SideTabBodyProps) {
  const tr = useMemo(() => createT(locale as Locale), [locale]);

  if (tab.kind === "browser") {
    return (
      <Suspense fallback={null}>
        <BrowserTab
          locale={locale}
          tabId={tab.id}
          url={tab.url}
          title={tab.title || tab.name}
          active={active}
          sshAlias={sshAlias}
        />
      </Suspense>
    );
  }

  if (tab.kind === "terminal") {
    return (
      <Suspense fallback={null}>
        <TerminalTab
          locale={locale}
          tabId={tab.id}
          projectPath={projectPath}
          sshAlias={sshAlias}
          active={active}
        />
      </Suspense>
    );
  }

  let title = tab.name;
  if (tab.kind === "file") title = tr("side.placeholder.file");
  else if (tab.kind === "review") title = tr("side.placeholder.review");
  else if (tab.kind === "plan") title = tr("side.placeholder.plan");

  const detail =
    tab.kind === "file" && tab.path ? tab.path : tab.name;

  return (
    <div
      className="sw-body sw-body--placeholder"
      data-testid={`side-body-${tab.kind}`}
      data-side-kind={tab.kind}
    >
      <div className="sw-body__title">{title}</div>
      {detail ? <div className="sw-body__detail">{detail}</div> : null}
    </div>
  );
}
