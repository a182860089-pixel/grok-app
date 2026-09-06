import { describe, expect, it } from "vitest";
import {
  browserChipClassName,
  browserChipState,
  browserStatusMessageKey,
  type BrowserBrokerSnapshot,
} from "./browserBroker";

function snap(
  partial: Partial<BrowserBrokerSnapshot> &
    Pick<BrowserBrokerSnapshot, "targets">,
): BrowserBrokerSnapshot {
  return {
    connected: false,
    selectedId: null,
    browserUrl: null,
    keepAlive: false,
    error: null,
    ...partial,
  };
}

describe("browserBroker presentation", () => {
  it("marks the chip connected when a debug port is live", () => {
    const s = snap({
      connected: true,
      selectedId: "dedicated",
      browserUrl: "http://127.0.0.1:9333",
      targets: [
        {
          id: "dedicated",
          kind: "dedicated",
          name: "Grok debug Chrome",
          debugStatus: "ready",
          port: 9333,
          detail: "port 9333",
          selected: true,
        },
      ],
    });
    expect(browserChipState(s)).toBe("connected");
    expect(browserChipClassName("connected")).toContain("is-connected");
    expect(browserStatusMessageKey(s)).toBe("composer.browserConnected");
  });

  it("warns when the selected daily browser still needs Allow", () => {
    const s = snap({
      selectedId: "chrome",
      targets: [
        {
          id: "chrome",
          kind: "chrome",
          name: "Google Chrome",
          debugStatus: "needs_allow",
          port: null,
          detail: null,
          selected: true,
        },
      ],
    });
    expect(browserChipState(s)).toBe("warn");
    expect(browserStatusMessageKey(s)).toBe("composer.browserNeedsAllow");
  });

  it("stays idle with no snapshot", () => {
    expect(browserChipState(null)).toBe("idle");
    expect(browserChipClassName("idle")).toBe("chip chip--browser");
    expect(browserStatusMessageKey(null)).toBe("composer.browserDisconnected");
  });

  it("treats the in-app browser as connected when selected", () => {
    const s = snap({
      connected: true,
      selectedId: "embedded",
      browserUrl: "in-app",
      targets: [
        {
          id: "embedded",
          kind: "embedded",
          name: "In-app browser",
          debugStatus: "ready",
          port: null,
          detail: null,
          selected: true,
        },
      ],
    });
    expect(browserChipState(s)).toBe("connected");
    expect(browserStatusMessageKey(s)).toBe("composer.browserConnected");
  });
});
