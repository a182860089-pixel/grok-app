/** Presentation helpers for the composer browser-control chip / panel. */

export type BrowserKind =
  | "embedded"
  | "dedicated"
  | "chrome"
  | "edge"
  | "brave"
  | "chromium"
  | "other";

export type BrowserDebugStatus =
  | "ready"
  | "running"
  | "needs_allow"
  | "stopped"
  | "launching";

export type BrowserTarget = {
  id: string;
  kind: BrowserKind;
  name: string;
  debugStatus: BrowserDebugStatus;
  port: number | null;
  detail: string | null;
  selected: boolean;
};

export type BrowserBrokerSnapshot = {
  connected: boolean;
  selectedId: string | null;
  browserUrl: string | null;
  keepAlive: boolean;
  error: string | null;
  targets: BrowserTarget[];
};

export type BrowserChipState = "idle" | "connected" | "warn";

export function browserChipState(
  snap: BrowserBrokerSnapshot | null | undefined,
): BrowserChipState {
  if (!snap) return "idle";
  if (snap.connected) return "connected";
  const sel = snap.targets.find((t) => t.selected || t.id === snap.selectedId);
  if (sel?.debugStatus === "needs_allow") return "warn";
  return "idle";
}

export function browserChipClassName(state: BrowserChipState): string {
  if (state === "connected") return "chip chip--browser is-connected";
  if (state === "warn") return "chip chip--browser is-warn";
  return "chip chip--browser";
}

export function browserStatusMessageKey(
  snap: BrowserBrokerSnapshot | null | undefined,
):
  | "composer.browserConnected"
  | "composer.browserNeedsAllow"
  | "composer.browserLaunching"
  | "composer.browserDisconnected" {
  if (!snap) return "composer.browserDisconnected";
  if (snap.connected) return "composer.browserConnected";
  const sel = snap.targets.find((t) => t.selected || t.id === snap.selectedId);
  if (sel?.debugStatus === "needs_allow") return "composer.browserNeedsAllow";
  if (sel?.debugStatus === "launching") return "composer.browserLaunching";
  return "composer.browserDisconnected";
}
