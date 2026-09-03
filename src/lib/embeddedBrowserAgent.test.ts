import { describe, expect, it } from "vitest";
import {
  agentOpenUrl,
  EMBEDDED_BROWSER_AGENT_OPEN_EVENT,
} from "./embeddedBrowserAgent";

describe("embeddedBrowserAgent", () => {
  it("opens the requested url and falls back to about:blank", () => {
    expect(agentOpenUrl({ url: "https://example.com/x" })).toBe(
      "https://example.com/x",
    );
    expect(agentOpenUrl({ url: "  " })).toBe("about:blank");
    expect(agentOpenUrl(null)).toBe("about:blank");
    expect(EMBEDDED_BROWSER_AGENT_OPEN_EVENT).toBe("side-browser://agent-open");
  });
});
