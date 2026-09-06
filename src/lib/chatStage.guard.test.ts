import { readFileSync } from "node:fs";
import { join } from "node:path";
import { describe, expect, it } from "vitest";

const src = readFileSync(
  join(__dirname, "../app/WorkbenchChatStage.tsx"),
  "utf8",
);

describe("chat stage isolation", () => {
  it("lazy-loads ConversationThreadLive instead of importing the barrel", () => {
    expect(src).toMatch(
      /lazy\(async \(\) => \{\s*const m = await import\("@\/components\/lobe-chat\/ConversationThreadLive"/,
    );
    expect(src).not.toContain(
      'import { ConversationThreadLive } from "@/components/lobe-chat"',
    );
  });
});
