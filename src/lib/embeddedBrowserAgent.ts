/**
 * In-app Browser Agent (Cursor-style): Host MCP drives the same
 * `resource-browser-*` WebView the user is looking at.
 */

import { DEFAULT_BROWSER_URL } from "@/lib/sshLoopbackUrl";

export const EMBEDDED_BROWSER_AGENT_OPEN_EVENT = "side-browser://agent-open";

export type EmbeddedBrowserAgentOpenPayload = {
  url?: string | null;
  requestId?: string | null;
};

/** URL the side workbench should open when Host asks for a Browser tab. */
export function agentOpenUrl(
  payload: EmbeddedBrowserAgentOpenPayload | null | undefined,
): string {
  const u = (payload?.url || "").trim();
  return u || DEFAULT_BROWSER_URL;
}
