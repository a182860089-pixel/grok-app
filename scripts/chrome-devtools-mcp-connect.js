#!/usr/bin/env node
/**
 * Attach chrome-devtools-mcp to a Host-managed Chromium debug endpoint.
 *
 * Prefer GROK_BROWSER_ENDPOINT_FILE / chrome-devtools-endpoint.json
 * (dedicated --remote-debugging-port Chrome: no Allow dialog).
 * Fall back to DevToolsActivePort from a daily Chrome that already allowed
 * chrome://inspect/#remote-debugging.
 */
const { spawn } = require("child_process");
const fs = require("fs");
const path = require("path");

function fail(message) {
  console.error(`[chrome-devtools-mcp-connect] ${message}`);
  process.exit(1);
}

function readJson(file) {
  try {
    return JSON.parse(fs.readFileSync(file, "utf8"));
  } catch {
    return null;
  }
}

function parsePortFile(file) {
  if (!file || !fs.existsSync(file)) return null;
  const lines = fs
    .readFileSync(file, "utf8")
    .trim()
    .split(/\r?\n/)
    .map((l) => l.trim())
    .filter(Boolean);
  if (lines.length < 1 || !/^\d+$/.test(lines[0])) return null;
  const port = lines[0];
  const out = { browserUrl: `http://127.0.0.1:${port}` };
  if (lines.length >= 2) {
    const wsPath = lines[1].startsWith("/") ? lines[1] : `/${lines[1]}`;
    out.wsEndpoint = `ws://127.0.0.1:${port}${wsPath}`;
  }
  return out;
}

function endpointCandidates() {
  const out = [];
  if (process.env.GROK_BROWSER_ENDPOINT_FILE) {
    out.push(process.env.GROK_BROWSER_ENDPOINT_FILE);
  }
  out.push(path.join(__dirname, "..", "chrome-devtools-endpoint.json"));
  out.push(path.join(__dirname, "chrome-devtools-endpoint.json"));
  return out;
}

function dailyPortFiles() {
  const local = process.env.LOCALAPPDATA || "";
  const home = process.env.HOME || process.env.USERPROFILE || "";
  return [
    path.join(local, "Google", "Chrome", "User Data", "DevToolsActivePort"),
    path.join(local, "Microsoft", "Edge", "User Data", "DevToolsActivePort"),
    path.join(local, "BraveSoftware", "Brave-Browser", "User Data", "DevToolsActivePort"),
    path.join(home, "Library", "Application Support", "Google", "Chrome", "DevToolsActivePort"),
    path.join(home, "Library", "Application Support", "Microsoft Edge", "DevToolsActivePort"),
    path.join(home, ".config", "google-chrome", "DevToolsActivePort"),
    path.join(home, ".config", "microsoft-edge", "DevToolsActivePort"),
    path.join(home, ".config", "BraveSoftware", "Brave-Browser", "DevToolsActivePort"),
  ];
}

function resolveTarget() {
  for (const file of endpointCandidates()) {
    const ep = readJson(file);
    if (ep && (ep.browserUrl || ep.wsEndpoint)) return ep;
  }
  for (const file of dailyPortFiles()) {
    const parsed = parsePortFile(file);
    if (parsed) return parsed;
  }
  return null;
}

const target = resolveTarget();
if (!target) {
  fail(
    "No browser debug endpoint. Open the Browser chip in the composer, start Grok debug Chrome, or allow remote debugging at chrome://inspect/#remote-debugging.",
  );
}

const args = ["-y", "chrome-devtools-mcp@latest", "--no-usage-statistics"];
if (target.browserUrl) {
  args.push("--browserUrl", String(target.browserUrl));
} else if (target.wsEndpoint) {
  args.push("--wsEndpoint", String(target.wsEndpoint));
}

const npx = process.platform === "win32" ? "npx.cmd" : "npx";
const child = spawn(npx, args, {
  stdio: "inherit",
  shell: process.platform === "win32",
  env: process.env,
});

child.on("error", (err) => fail(`failed to start chrome-devtools-mcp: ${err.message}`));
child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  process.exit(code ?? 1);
});
