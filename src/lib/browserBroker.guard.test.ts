import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

describe("browser broker composer chip", () => {
  it("mounts the labeled browser chip in the composer row", () => {
    const shell = readFileSync(
      join(__dirname, "../app/WorkbenchComposerShell.tsx"),
      "utf8",
    );
    expect(shell).toContain('import { BrowserBrokerChip } from "@/components/BrowserBrokerPanel"');
    expect(shell).toContain("<BrowserBrokerChip locale={locale} />");
  });
});
