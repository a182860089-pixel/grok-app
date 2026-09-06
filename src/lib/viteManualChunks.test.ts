import { describe, expect, it } from "vitest";
import { vendorManualChunk } from "./viteManualChunks";

describe("vendorManualChunk", () => {
  it("ignores app source", () => {
    expect(vendorManualChunk("/repo/src/components/MarkdownChat.tsx")).toBeUndefined();
  });

  it("groups xterm including Windows paths", () => {
    expect(
      vendorManualChunk("D:\\repo\\node_modules\\@xterm\\xterm\\lib\\xterm.js"),
    ).toBe("xterm");
    expect(
      vendorManualChunk("/repo/node_modules/@xterm/addon-webgl/lib/addon.js"),
    ).toBe("xterm");
  });

  it("groups TipTap / ProseMirror, not React", () => {
    expect(
      vendorManualChunk("/repo/node_modules/@tiptap/react/dist/index.js"),
    ).toBe("tiptap");
    expect(
      vendorManualChunk("/repo/node_modules/prosemirror-model/dist/index.js"),
    ).toBe("tiptap");
    expect(
      vendorManualChunk("/repo/node_modules/react-dom/index.js"),
    ).toBeUndefined();
  });

  it("groups CodeMirror / lezer / style-mod together", () => {
    expect(
      vendorManualChunk("/repo/node_modules/@codemirror/view/dist/index.js"),
    ).toBe("codemirror");
    expect(
      vendorManualChunk(
        "D:\\repo\\node_modules\\@codemirror\\language\\dist\\index.js",
      ),
    ).toBe("codemirror");
    expect(
      vendorManualChunk("/repo/node_modules/@lezer/highlight/dist/index.js"),
    ).toBe("codemirror");
    expect(
      vendorManualChunk("/repo/node_modules/style-mod/src/style-mod.js"),
    ).toBe("codemirror");
    expect(
      vendorManualChunk("/repo/node_modules/crelt/index.js"),
    ).toBe("codemirror");
  });

  it("groups pdf / office / media previewers", () => {
    expect(
      vendorManualChunk("/repo/node_modules/pdfjs-dist/build/pdf.mjs"),
    ).toBe("pdfjs");
    expect(
      vendorManualChunk("/repo/node_modules/react-pdf/dist/index.js"),
    ).toBe("pdfjs");
    expect(
      vendorManualChunk("/repo/node_modules/xlsx/xlsx.mjs"),
    ).toBe("xlsx");
    expect(
      vendorManualChunk("/repo/node_modules/docx-preview/dist/docx-preview.js"),
    ).toBe("docx");
    expect(
      vendorManualChunk("/repo/node_modules/plyr/dist/plyr.min.js"),
    ).toBe("plyr");
    expect(
      vendorManualChunk("/repo/node_modules/highlight.js/lib/core.js"),
    ).toBe("hljs");
  });

  it("groups react-markdown and remark-gfm", () => {
    expect(
      vendorManualChunk("/repo/node_modules/react-markdown/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/remark-gfm/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/remark-math/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/rehype-katex/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/katex/dist/katex.min.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/micromark/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/katex/dist/katex.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/remark-math/index.js"),
    ).toBe("markdown");
    expect(
      vendorManualChunk("/repo/node_modules/rehype-katex/index.js"),
    ).toBe("markdown");
  });
});
