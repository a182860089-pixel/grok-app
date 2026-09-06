import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const src = readFileSync(
  join(__dirname, "../components/side-workbench/SideTabBody.tsx"),
  "utf8",
);

describe("side tab body isolation", () => {
  it("lazy-loads Browser and Terminal so Files does not pull WebView/xterm", () => {
    expect(src).toMatch(/lazy\(async \(\) => \{\s*const m = await import\("\.\/BrowserTab"/);
    expect(src).toMatch(/lazy\(async \(\) => \{\s*const m = await import\("\.\/TerminalTab"/);
    expect(src).not.toMatch(/import \{ BrowserTab \} from "\.\/BrowserTab"/);
    expect(src).not.toMatch(/import \{ TerminalTab \} from "\.\/TerminalTab"/);
  });
});
